#![cfg_attr(not(target_os = "android"), allow(dead_code, unused_variables))]

use crate::net::DecodedStreamPacket;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const MAX_QUEUE_LEN: usize = 12;

struct QueueState {
    frames: VecDeque<DecodedStreamPacket>,
    closed: bool,
}

pub enum QueuePop {
    Packet(DecodedStreamPacket),
    Timeout,
    Closed,
}

pub struct MonitorFrameQueue {
    state: Mutex<QueueState>,
    cv: Condvar,
}

impl Default for MonitorFrameQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl MonitorFrameQueue {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                frames: VecDeque::with_capacity(MAX_QUEUE_LEN),
                closed: false,
            }),
            cv: Condvar::new(),
        }
    }

    pub fn push(&self, pkt: DecodedStreamPacket) {
        let mut st = self.state.lock().expect("MonitorFrameQueue mutex poisoned");
        if st.closed {
            return;
        }
        if pkt.header.is_idr && st.frames.len() >= 2 {
            st.frames.clear();
        }
        while st.frames.len() >= MAX_QUEUE_LEN {
            st.frames.pop_front();
        }
        st.frames.push_back(pkt);
        self.cv.notify_one();
    }

    pub fn pop_with_drain_timeout(&self, drain_timeout: Option<Duration>) -> QueuePop {
        let mut st = self.state.lock().expect("MonitorFrameQueue mutex poisoned");
        loop {
            if let Some(pkt) = st.frames.pop_front() {
                return QueuePop::Packet(pkt);
            }
            if st.closed {
                return QueuePop::Closed;
            }
            match drain_timeout {
                Some(d) => {
                    let (next_st, res) = self
                        .cv
                        .wait_timeout(st, d)
                        .expect("MonitorFrameQueue mutex poisoned");
                    st = next_st;
                    if res.timed_out() && st.frames.is_empty() && !st.closed {
                        return QueuePop::Timeout;
                    }
                }
                None => {
                    st = self.cv.wait(st).expect("MonitorFrameQueue mutex poisoned");
                }
            }
        }
    }

    pub fn close(&self) {
        let mut st = self.state.lock().expect("MonitorFrameQueue mutex poisoned");
        st.closed = true;
        self.cv.notify_all();
    }

    pub fn len(&self) -> usize {
        self.state
            .lock()
            .expect("MonitorFrameQueue mutex poisoned")
            .frames
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

const FALLBACK_AV1_2560X1440_SEQ_HDR: [u8; 14] = [
    0x0a, 0x0c, 0x00, 0x00, 0x00, 0x24, 0xc6, 0xff, 0x9f, 0xc5, 0xa7, 0x20, 0x80, 0x80,
];

fn find_av1_seq_header_range(bitstream: &[u8]) -> Option<(usize, usize)> {
    let mut pos = 0usize;
    while pos < bitstream.len() {
        let start = pos;
        let hdr = bitstream[pos];
        let obu_type = (hdr >> 3) & 0x0F;
        let has_ext = (hdr >> 2) & 1 != 0;
        let has_size = (hdr >> 1) & 1 != 0;
        pos += 1 + usize::from(has_ext);
        if !has_size || pos >= bitstream.len() {
            return None;
        }
        let mut size: usize = 0;
        let mut shift = 0u32;
        loop {
            if pos >= bitstream.len() || shift >= 28 {
                return None;
            }
            let b = bitstream[pos];
            pos += 1;
            size |= ((b & 0x7F) as usize) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        let end = pos.checked_add(size)?;
        if end > bitstream.len() {
            return None;
        }
        if obu_type == 1 {
            return Some((start, end));
        }
        pos = end;
    }
    None
}

/// Return `true` if the low-overhead AV1 OBU bitstream contains an `OBU_SEQUENCE_HEADER` (`obu_type == 1`).
pub fn has_av1_seq_header(bitstream: &[u8]) -> bool {
    find_av1_seq_header_range(bitstream).is_some()
}

/// Extract the `OBU_SEQUENCE_HEADER` (`obu_type == 1`) bytes from a low-overhead AV1 OBU bitstream.
pub fn extract_av1_seq_header(bitstream: &[u8]) -> Option<Vec<u8>> {
    find_av1_seq_header_range(bitstream).map(|(start, end)| bitstream[start..end].to_vec())
}

fn splice_av1_seq_header<'a>(payload: &'a [u8], seq_hdr: &[u8]) -> Cow<'a, [u8]> {
    if has_av1_seq_header(payload) || seq_hdr.is_empty() {
        return Cow::Borrowed(payload);
    }
    let insert_at = if payload.starts_with(&[0x12, 0x00]) {
        2
    } else {
        0
    };
    let mut patched = Vec::with_capacity(payload.len() + seq_hdr.len());
    patched.extend_from_slice(&payload[..insert_at]);
    patched.extend_from_slice(seq_hdr);
    patched.extend_from_slice(&payload[insert_at..]);
    Cow::Owned(patched)
}

/// Prepend `FALLBACK_AV1_2560X1440_SEQ_HDR` right after a leading 2-byte `OBU_TEMPORAL_DELIMITER`
/// (`[0x12, 0x00]`), or at offset 0, if an AV1 IDR packet lacks an `OBU_SEQUENCE_HEADER`.
fn splice_fallback_av1_seq_header(payload: &[u8]) -> Cow<'_, [u8]> {
    splice_av1_seq_header(payload, &FALLBACK_AV1_2560X1440_SEQ_HDR)
}

#[cfg(target_os = "android")]
mod imp {
    use super::{find_av1_seq_header_range, splice_av1_seq_header, splice_fallback_av1_seq_header};
    use anyhow::{anyhow, Result};
    use ndk::media::media_codec::{
        DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection,
        MediaFormat,
    };
    use ndk::native_window::NativeWindow;
    use std::borrow::Cow;
    use std::ptr::NonNull;
    use std::time::Duration;
    use vdesk_protocol::VideoCodec;

    pub struct HardwareDecoder {
        codec: MediaCodec,
        video_codec: VideoCodec,
        width: u32,
        height: u32,
        cached_seq_header: Option<Vec<u8>>,
        // Keep the ANativeWindow alive for the lifetime of the configured MediaCodec.
        _surface: Option<NativeWindow>,
        waiting_for_idr: bool,
        last_frame_index: Option<u64>,
        pub frames_rendered: u64,
    }

    impl HardwareDecoder {
        pub fn new(
            video_codec: VideoCodec,
            width: u32,
            height: u32,
            surface: &NativeWindow,
        ) -> Result<Self> {
            Self::configure_with_surface(video_codec, width, height, surface, None)
        }

        /// Wrap a Java `android.view.Surface` into an `ANativeWindow` on the caller JNI thread.
        ///
        /// # Safety
        /// `env` must be a valid JNI thread environment and `surface` a valid `android.view.Surface` jobject.
        pub unsafe fn wrap_java_surface(
            env: *mut jni::sys::JNIEnv,
            surface: jni::sys::jobject,
        ) -> Result<NativeWindow> {
            let ptr = ndk_sys::ANativeWindow_fromSurface(env as *mut _, surface as *mut _);
            let nn = NonNull::new(ptr)
                .ok_or_else(|| anyhow!("ANativeWindow_fromSurface returned null"))?;
            Ok(NativeWindow::from_ptr(nn))
        }

        pub fn from_native_window(
            native_window: NativeWindow,
            video_codec: VideoCodec,
            width: u32,
            height: u32,
        ) -> Result<Self> {
            Self::configure_with_surface(
                video_codec,
                width,
                height,
                &native_window,
                Some(native_window.clone()),
            )
        }

        /// Wrap a Java `android.view.Surface` (e.g. returned by OpenXR
        /// `xrCreateSwapchainAndroidSurfaceKHR`) into an `ANativeWindow` and bind it
        /// directly as the `AMediaCodec` output surface.
        ///
        /// # Safety
        /// `env` must be a valid JNI thread environment and `surface` a valid `android.view.Surface` jobject.
        pub unsafe fn from_java_surface(
            env: *mut jni::sys::JNIEnv,
            surface: jni::sys::jobject,
            video_codec: VideoCodec,
            width: u32,
            height: u32,
        ) -> Result<Self> {
            let native_window = Self::wrap_java_surface(env, surface)?;
            Self::from_native_window(native_window, video_codec, width, height)
        }

        fn configure_with_surface(
            video_codec: VideoCodec,
            width: u32,
            height: u32,
            surface: &NativeWindow,
            owned_surface: Option<NativeWindow>,
        ) -> Result<Self> {
            let mime = match video_codec {
                VideoCodec::Av1YUV420 => "video/av01",
                VideoCodec::HevcYUV420 | VideoCodec::HevcYUV444 => "video/hevc",
            };

            let codec = MediaCodec::from_decoder_type(mime)
                .ok_or_else(|| anyhow!("Failed to create AMediaCodec decoder for {}", mime))?;

            let mut format = MediaFormat::new();
            format.set_str("mime", mime);
            format.set_i32("width", width as i32);
            format.set_i32("height", height as i32);
            // Ultra-low-latency Snapdragon XR2 Gen 2 decoder flags + DVFS operating rate
            format.set_i32("low-latency", 1);
            format.set_i32("vendor.qti-ext-dec-low-latency.enable", 1);
            format.set_i32("priority", 0);
            format.set_i32("operating-rate", 120);
            format.set_i32("frame-rate", 60);

            codec.configure(&format, Some(surface), MediaCodecDirection::Decoder)?;
            codec.start()?;

            Ok(Self {
                codec,
                video_codec,
                width,
                height,
                cached_seq_header: None,
                _surface: owned_surface,
                waiting_for_idr: true,
                last_frame_index: None,
                frames_rendered: 0,
            })
        }

        pub fn has_rendered_frame(&self) -> bool {
            self.frames_rendered > 0
        }

        /// Returns `Ok(true)` when a frame gap or input timeout requires requesting an IDR keyframe.
        pub fn submit_and_render(
            &mut self,
            payload: &[u8],
            frame_index: u64,
            pts_us: u64,
            is_idr: bool,
        ) -> Result<bool> {
            if self.waiting_for_idr {
                if !is_idr {
                    return Ok(false);
                }
                self.waiting_for_idr = false;
            } else if !is_idr
                && self
                    .last_frame_index
                    .is_some_and(|prev| frame_index != prev.wrapping_add(1))
            {
                log::warn!(
                    "Detected frame index gap ({:?} -> {}), waiting for IDR",
                    self.last_frame_index,
                    frame_index
                );
                self.waiting_for_idr = true;
                return Ok(true);
            }

            let effective_payload: Cow<'_, [u8]> = if is_idr
                && self.video_codec == VideoCodec::Av1YUV420
            {
                let seq_range = find_av1_seq_header_range(payload);
                let preview_len = payload.len().min(16);
                log::info!(
                    "AMediaCodec submitting IDR frame: len={}, has_seq_hdr={}, first16={:02x?}",
                    payload.len(),
                    seq_range.is_some(),
                    &payload[..preview_len]
                );
                if let Some((start, end)) = seq_range {
                    self.cached_seq_header = Some(payload[start..end].to_vec());
                    Cow::Borrowed(payload)
                } else if let Some(ref seq_hdr) = self.cached_seq_header {
                    log::warn!(
                            "AV1 IDR missing OBU_SEQUENCE_HEADER; splicing cached {}x{} sequence header",
                            self.width,
                            self.height
                        );
                    splice_av1_seq_header(payload, seq_hdr)
                } else if self.width == 2560 && self.height == 1440 {
                    log::warn!(
                            "AV1 IDR missing OBU_SEQUENCE_HEADER; splicing fallback 2560x1440 sequence header"
                        );
                    splice_fallback_av1_seq_header(payload)
                } else {
                    log::warn!(
                            "AV1 IDR missing OBU_SEQUENCE_HEADER for {}x{} stream; forwarding unmodified",
                            self.width,
                            self.height
                        );
                    Cow::Borrowed(payload)
                }
            } else {
                Cow::Borrowed(payload)
            };

            // Drain completed output buffers first so the decoder pool never starves input buffers.
            self.drain_output()?;

            for _ in 0..6 {
                match self.codec.dequeue_input_buffer(Duration::from_millis(4))? {
                    DequeuedInputBufferResult::Buffer(mut input_buf) => {
                        let dst = input_buf.buffer_mut();
                        let len = effective_payload.len().min(dst.len());
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                effective_payload.as_ptr(),
                                dst.as_mut_ptr() as *mut u8,
                                len,
                            );
                        }
                        self.codec
                            .queue_input_buffer(input_buf, 0, len, pts_us, 0)?;
                        self.last_frame_index = Some(frame_index);

                        self.drain_output_with_timeout(Duration::from_micros(0))?;
                        if self.frames_rendered == 0 {
                            for _ in 0..25 {
                                self.drain_output_with_timeout(Duration::from_millis(2))?;
                                if self.frames_rendered > 0 {
                                    break;
                                }
                            }
                        }
                        return Ok(false);
                    }
                    DequeuedInputBufferResult::TryAgainLater => {
                        self.drain_output()?;
                    }
                }
            }

            log::warn!(
                "AMediaCodec input buffer timeout (frame_index={}); requesting IDR",
                frame_index
            );
            self.waiting_for_idr = true;
            self.drain_output()?;
            Ok(true)
        }

        pub fn drain_output(&mut self) -> Result<()> {
            self.drain_output_with_timeout(Duration::from_micros(0))
        }

        pub fn drain_output_with_timeout(&mut self, mut timeout: Duration) -> Result<()> {
            let mut latest_buf = None;
            loop {
                match self.codec.dequeue_output_buffer(timeout)? {
                    DequeuedOutputBufferInfoResult::Buffer(output_buf) => {
                        timeout = Duration::ZERO;
                        if let Some(prev) = latest_buf.replace(output_buf) {
                            // Drop superseded frames without churning the OpenXR swapchain surface.
                            self.codec.release_output_buffer(prev, false)?;
                        }
                    }
                    DequeuedOutputBufferInfoResult::OutputFormatChanged => {
                        log::info!("AMediaCodec output format changed");
                        continue;
                    }
                    DequeuedOutputBufferInfoResult::OutputBuffersChanged => {
                        continue;
                    }
                    DequeuedOutputBufferInfoResult::TryAgainLater => break,
                }
            }

            if let Some(newest) = latest_buf {
                // Render only the newest decoded frame directly to the OpenXR swapchain surface.
                self.codec.release_output_buffer(newest, true)?;
                let first = self.frames_rendered == 0;
                self.frames_rendered += 1;
                if first {
                    log::info!("AMediaCodec rendered first frame to OpenXR surface");
                }
            }

            Ok(())
        }
    }
}

#[cfg(not(target_os = "android"))]
mod imp {
    use anyhow::Result;
    use vdesk_protocol::VideoCodec;

    pub struct HardwareDecoder {
        pub codec_type: VideoCodec,
        pub width: u32,
        pub height: u32,
        waiting_for_idr: bool,
        last_frame_index: Option<u64>,
        pub frames_rendered: u64,
    }

    impl HardwareDecoder {
        pub fn new_stub(video_codec: VideoCodec, width: u32, height: u32) -> Result<Self> {
            Ok(Self {
                codec_type: video_codec,
                width,
                height,
                waiting_for_idr: true,
                last_frame_index: None,
                frames_rendered: 0,
            })
        }

        pub fn has_rendered_frame(&self) -> bool {
            self.frames_rendered > 0
        }

        pub fn submit_and_render(
            &mut self,
            _payload: &[u8],
            frame_index: u64,
            _pts_us: u64,
            is_idr: bool,
        ) -> Result<bool> {
            if self.waiting_for_idr {
                if !is_idr {
                    return Ok(false);
                }
                self.waiting_for_idr = false;
            } else if !is_idr
                && self
                    .last_frame_index
                    .is_some_and(|prev| frame_index != prev.wrapping_add(1))
            {
                self.waiting_for_idr = true;
                return Ok(true);
            }
            self.last_frame_index = Some(frame_index);
            self.frames_rendered += 1;
            Ok(false)
        }

        pub fn drain_output(&mut self) -> Result<()> {
            Ok(())
        }
    }
}

pub use imp::HardwareDecoder;

pub struct DecoderWorker {
    has_rendered: Arc<AtomicBool>,
    queue: Arc<MonitorFrameQueue>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl DecoderWorker {
    pub fn has_rendered_frame(&self) -> bool {
        self.has_rendered.load(Ordering::Acquire)
    }

    #[cfg(target_os = "android")]
    /// # Safety
    /// `env` must be a valid JNI thread environment and `surface` a valid `android.view.Surface` jobject.
    pub unsafe fn spawn_from_java_surface(
        env: *mut jni::sys::JNIEnv,
        surface: jni::sys::jobject,
        stream_id: u32,
        video_codec: vdesk_protocol::VideoCodec,
        width: u32,
        height: u32,
        on_request_idr: Arc<dyn Fn(u32) + Send + Sync>,
    ) -> anyhow::Result<(Self, Arc<MonitorFrameQueue>)> {
        let native_window = HardwareDecoder::wrap_java_surface(env, surface)?;
        let queue = Arc::new(MonitorFrameQueue::new());
        let has_rendered = Arc::new(AtomicBool::new(false));
        let (init_tx, init_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();

        let queue_thread = Arc::clone(&queue);
        let has_rendered_thread = Arc::clone(&has_rendered);
        let join = std::thread::Builder::new()
            .name(format!("vdesk-dec-{}", stream_id))
            .spawn(move || {
                match HardwareDecoder::from_native_window(native_window, video_codec, width, height)
                {
                    Ok(decoder) => {
                        let _ = init_tx.send(Ok(()));
                        run_decoder_thread(
                            decoder,
                            stream_id,
                            queue_thread,
                            has_rendered_thread,
                            on_request_idr,
                        );
                    }
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                    }
                }
            })?;

        match init_rx.recv() {
            Ok(Ok(())) => Ok((
                Self {
                    has_rendered,
                    queue: Arc::clone(&queue),
                    join: Some(join),
                },
                queue,
            )),
            Ok(Err(e)) => {
                queue.close();
                let _ = join.join();
                Err(e)
            }
            Err(_) => {
                queue.close();
                let _ = join.join();
                Err(anyhow::anyhow!(
                    "Decoder worker thread {} exited during init",
                    stream_id
                ))
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    pub fn spawn_stub(
        stream_id: u32,
        video_codec: vdesk_protocol::VideoCodec,
        width: u32,
        height: u32,
        on_request_idr: Arc<dyn Fn(u32) + Send + Sync>,
    ) -> anyhow::Result<(Self, Arc<MonitorFrameQueue>)> {
        let queue = Arc::new(MonitorFrameQueue::new());
        let has_rendered = Arc::new(AtomicBool::new(false));
        let (init_tx, init_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();

        let queue_thread = Arc::clone(&queue);
        let has_rendered_thread = Arc::clone(&has_rendered);
        let join = std::thread::Builder::new()
            .name(format!("vdesk-dec-stub-{}", stream_id))
            .spawn(
                move || match HardwareDecoder::new_stub(video_codec, width, height) {
                    Ok(decoder) => {
                        let _ = init_tx.send(Ok(()));
                        run_decoder_thread(
                            decoder,
                            stream_id,
                            queue_thread,
                            has_rendered_thread,
                            on_request_idr,
                        );
                    }
                    Err(e) => {
                        let _ = init_tx.send(Err(e));
                    }
                },
            )?;

        init_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("Stub decoder init thread exited"))??;

        Ok((
            Self {
                has_rendered,
                queue: Arc::clone(&queue),
                join: Some(join),
            },
            queue,
        ))
    }
}

impl Drop for DecoderWorker {
    fn drop(&mut self) {
        self.queue.close();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn run_decoder_thread(
    mut decoder: HardwareDecoder,
    stream_id: u32,
    queue: Arc<MonitorFrameQueue>,
    has_rendered: Arc<AtomicBool>,
    on_request_idr: Arc<dyn Fn(u32) + Send + Sync>,
) {
    let mut drain_ticks_left: u8 = 0;
    let mut last_idr_req: Option<Instant> = None;

    loop {
        let wait_opt = if drain_ticks_left > 0 {
            Some(Duration::from_millis(4))
        } else {
            None
        };
        match queue.pop_with_drain_timeout(wait_opt) {
            QueuePop::Packet(pkt) => {
                drain_ticks_left = 5;
                let res = decoder.submit_and_render(
                    &pkt.payload,
                    pkt.header.frame_index,
                    pkt.header.capture_timestamp_us,
                    pkt.header.is_idr,
                );
                if decoder.has_rendered_frame() {
                    has_rendered.store(true, Ordering::Release);
                }
                let need_idr = match res {
                    Ok(flag) => flag,
                    Err(e) => {
                        log::warn!("Decoder submit failed on stream {}: {:#}", stream_id, e);
                        true
                    }
                };
                if need_idr
                    && last_idr_req.is_none_or(|t| t.elapsed() >= Duration::from_millis(500))
                {
                    (on_request_idr)(stream_id);
                    last_idr_req = Some(Instant::now());
                }
            }
            QueuePop::Timeout => {
                drain_ticks_left = drain_ticks_left.saturating_sub(1);
                let _ = decoder.drain_output();
                if decoder.has_rendered_frame() {
                    has_rendered.store(true, Ordering::Release);
                }
            }
            QueuePop::Closed => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use vdesk_protocol::{VideoCodec, VideoFrameHeader};

    fn make_pkt(frame_index: u64, is_idr: bool) -> DecodedStreamPacket {
        DecodedStreamPacket {
            header: VideoFrameHeader {
                stream_id: 93,
                frame_index,
                capture_timestamp_us: frame_index * 16_666,
                is_idr,
                payload_len: 4,
            },
            payload: vec![0xAA, 0xBB, 0xCC, 0xDD],
        }
    }

    #[test]
    fn av1_seq_header_detection_and_fallback_splice() {
        assert!(has_av1_seq_header(&FALLBACK_AV1_2560X1440_SEQ_HDR));

        // OBU_TEMPORAL_DELIMITER (0x12, 0x00) + OBU_FRAME_HEADER-like payload (obu_type=3 => 0x1a)
        let idr_without_seq = [0x12u8, 0x00, 0x1a, 0x02, 0xaa, 0xbb];
        assert!(!has_av1_seq_header(&idr_without_seq));

        let spliced = splice_fallback_av1_seq_header(&idr_without_seq);
        assert!(has_av1_seq_header(&spliced));
        assert_eq!(&spliced[..2], &[0x12, 0x00]);
        assert_eq!(
            &spliced[2..2 + FALLBACK_AV1_2560X1440_SEQ_HDR.len()],
            &FALLBACK_AV1_2560X1440_SEQ_HDR
        );
        assert_eq!(
            &spliced[2 + FALLBACK_AV1_2560X1440_SEQ_HDR.len()..],
            &[0x1a, 0x02, 0xaa, 0xbb]
        );

        let extracted = extract_av1_seq_header(&spliced).expect("extract OBU_SEQUENCE_HEADER");
        assert_eq!(extracted, FALLBACK_AV1_2560X1440_SEQ_HDR);

        let custom_seq = [0x0au8, 0x03, 0x11, 0x22, 0x33];
        let custom_spliced = splice_av1_seq_header(&idr_without_seq, &custom_seq);
        assert_eq!(
            extract_av1_seq_header(&custom_spliced).as_deref(),
            Some(&custom_seq[..])
        );
    }

    #[test]
    fn monitor_frame_queue_fast_forwards_on_idr() {
        let q = MonitorFrameQueue::new();
        q.push(make_pkt(1, false));
        q.push(make_pkt(2, false));
        assert_eq!(q.len(), 2);

        // Pushing an IDR when >= 2 stale frames are buffered drops stale frames immediately.
        q.push(make_pkt(3, true));
        assert_eq!(q.len(), 1);
        match q.pop_with_drain_timeout(Some(Duration::from_millis(10))) {
            QueuePop::Packet(pkt) => {
                assert_eq!(pkt.header.frame_index, 3);
                assert!(pkt.header.is_idr);
            }
            _ => panic!("Expected IDR packet"),
        }
        assert!(q.is_empty());
    }

    #[test]
    fn decoder_worker_stub_processes_frames_and_drops_cleanly() {
        let idr_reqs = Arc::new(AtomicUsize::new(0));
        let idr_cb = Arc::clone(&idr_reqs);
        let (worker, queue) = DecoderWorker::spawn_stub(
            93,
            VideoCodec::Av1YUV420,
            2560,
            1440,
            Arc::new(move |_| {
                idr_cb.fetch_add(1, Ordering::Relaxed);
            }),
        )
        .expect("spawn_stub");

        assert!(!worker.has_rendered_frame());
        // Non-IDR before first IDR is ignored safely
        queue.push(make_pkt(0, false));
        // IDR renders
        queue.push(make_pkt(1, true));
        for _ in 0..50 {
            if worker.has_rendered_frame() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(worker.has_rendered_frame());

        // Frame gap triggers IDR callback once (debounced)
        queue.push(make_pkt(5, false));
        for _ in 0..50 {
            if idr_reqs.load(Ordering::Relaxed) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(idr_reqs.load(Ordering::Relaxed), 1);

        drop(worker);
        assert!(matches!(
            queue.pop_with_drain_timeout(None),
            QueuePop::Closed
        ));
    }
}
