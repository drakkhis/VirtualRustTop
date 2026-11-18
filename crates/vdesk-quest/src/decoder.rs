#![cfg_attr(not(target_os = "android"), allow(dead_code, unused_variables))]

use std::borrow::Cow;

const FALLBACK_AV1_2560X1440_SEQ_HDR: [u8; 14] = [
    0x0a, 0x0c, 0x00, 0x00, 0x00, 0x24, 0xc6, 0xff, 0x9f, 0xc5, 0xa7, 0x20, 0x80, 0x80,
];

/// Return `true` if the low-overhead AV1 OBU bitstream contains an `OBU_SEQUENCE_HEADER` (`obu_type == 1`).
pub fn has_av1_seq_header(bitstream: &[u8]) -> bool {
    let mut pos = 0usize;
    while pos < bitstream.len() {
        let hdr = bitstream[pos];
        let obu_type = (hdr >> 3) & 0x0F;
        let has_ext = (hdr >> 2) & 1 != 0;
        let has_size = (hdr >> 1) & 1 != 0;
        pos += 1 + usize::from(has_ext);
        if !has_size || pos >= bitstream.len() {
            return false;
        }
        let mut size: usize = 0;
        let mut shift = 0u32;
        loop {
            if pos >= bitstream.len() || shift >= 28 {
                return false;
            }
            let b = bitstream[pos];
            pos += 1;
            size |= ((b & 0x7F) as usize) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        let Some(end) = pos.checked_add(size) else {
            return false;
        };
        if end > bitstream.len() {
            return false;
        }
        if obu_type == 1 {
            return true;
        }
        pos = end;
    }
    false
}

/// Prepend `FALLBACK_AV1_2560X1440_SEQ_HDR` right after a leading 2-byte `OBU_TEMPORAL_DELIMITER`
/// (`[0x12, 0x00]`), or at offset 0, if an AV1 IDR packet lacks an `OBU_SEQUENCE_HEADER`.
fn splice_fallback_av1_seq_header(payload: &[u8]) -> Cow<'_, [u8]> {
    if has_av1_seq_header(payload) {
        return Cow::Borrowed(payload);
    }
    let insert_at = if payload.starts_with(&[0x12, 0x00]) {
        2
    } else {
        0
    };
    let mut patched = Vec::with_capacity(payload.len() + FALLBACK_AV1_2560X1440_SEQ_HDR.len());
    patched.extend_from_slice(&payload[..insert_at]);
    patched.extend_from_slice(&FALLBACK_AV1_2560X1440_SEQ_HDR);
    patched.extend_from_slice(&payload[insert_at..]);
    Cow::Owned(patched)
}

#[cfg(target_os = "android")]
mod imp {
    use super::{has_av1_seq_header, splice_fallback_av1_seq_header};
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
            let ptr = ndk_sys::ANativeWindow_fromSurface(env as *mut _, surface as *mut _);
            let nn = NonNull::new(ptr)
                .ok_or_else(|| anyhow!("ANativeWindow_fromSurface returned null"))?;
            let native_window = NativeWindow::from_ptr(nn);
            let mime_codec = Self::configure_with_surface(
                video_codec,
                width,
                height,
                &native_window,
                Some(native_window.clone()),
            )?;
            Ok(mime_codec)
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
            // Ultra-low-latency Snapdragon XR2 Gen 2 decoder flags
            format.set_i32("low-latency", 1);
            format.set_i32("vendor.qti-ext-dec-low-latency.enable", 1);
            format.set_i32("priority", 0);

            codec.configure(&format, Some(surface), MediaCodecDirection::Decoder)?;
            codec.start()?;

            Ok(Self {
                codec,
                video_codec,
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

            let effective_payload: Cow<'_, [u8]> =
                if is_idr && self.video_codec == VideoCodec::Av1YUV420 {
                    let has_seq_hdr = has_av1_seq_header(payload);
                    let preview_len = payload.len().min(16);
                    log::info!(
                        "AMediaCodec submitting IDR frame: len={}, has_seq_hdr={}, first16={:02x?}",
                        payload.len(),
                        has_seq_hdr,
                        &payload[..preview_len]
                    );
                    if has_seq_hdr {
                        Cow::Borrowed(payload)
                    } else {
                        log::warn!(
                            "AV1 IDR missing OBU_SEQUENCE_HEADER; splicing fallback 2560x1440 sequence header"
                        );
                        splice_fallback_av1_seq_header(payload)
                    }
                } else {
                    Cow::Borrowed(payload)
                };

            let dequeue_timeout = if self.frames_rendered == 0 {
                Duration::from_millis(20)
            } else {
                Duration::from_millis(15)
            };

            if let DequeuedInputBufferResult::Buffer(mut input_buf) =
                self.codec.dequeue_input_buffer(dequeue_timeout)?
            {
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

        pub fn drain_output_with_timeout(&mut self, timeout: Duration) -> Result<()> {
            loop {
                match self.codec.dequeue_output_buffer(timeout)? {
                    DequeuedOutputBufferInfoResult::Buffer(output_buf) => {
                        // Render directly to the OpenXR swapchain ANativeWindow surface
                        self.codec.release_output_buffer(output_buf, true)?;
                        self.frames_rendered += 1;
                        if self.frames_rendered == 1 {
                            log::info!("AMediaCodec rendered first frame to OpenXR surface");
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
