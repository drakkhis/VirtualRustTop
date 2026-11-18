//! Low-latency lock-free audio ring buffer and Android AAudio playback.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const RING_CAP: usize = 16_384; // ~170 ms stereo @ 48 kHz (power of 2)
const RING_MASK: usize = RING_CAP - 1;
const MAX_LATENCY_SAMPLES: usize = 4_800; // Trim above 50 ms
const TRIM_TARGET_SAMPLES: usize = 1_920; // Trim back to 20 ms
const PREFILL_SAMPLES: usize = 960; // 10 ms prefill after underrun

pub struct AudioRingBuffer {
    buf: UnsafeCell<Box<[i16]>>,
    read_idx: AtomicUsize,
    write_idx: AtomicUsize,
    primed: AtomicBool,
}

// Safe because `AudioRingBuffer` is strictly Single-Producer Single-Consumer:
// - Producer (`push`) owns `write_idx` and only writes slots ahead of `read_idx`.
// - Consumer (`pop_into`) owns `read_idx` and `primed` and only reads slots behind `write_idx`.
unsafe impl Send for AudioRingBuffer {}
unsafe impl Sync for AudioRingBuffer {}

impl Default for AudioRingBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioRingBuffer {
    pub fn new() -> Self {
        Self {
            buf: UnsafeCell::new(vec![0i16; RING_CAP].into_boxed_slice()),
            read_idx: AtomicUsize::new(0),
            write_idx: AtomicUsize::new(0),
            primed: AtomicBool::new(false),
        }
    }

    pub fn push(&self, samples: &[i16]) {
        if samples.is_empty() {
            return;
        }
        let r = self.read_idx.load(Ordering::Acquire);
        let w = self.write_idx.load(Ordering::Relaxed);
        let used = w.wrapping_sub(r);
        let free = RING_CAP.saturating_sub(used);
        let to_write = samples.len().min(free);
        if to_write == 0 {
            return;
        }
        // SAFETY: Producer exclusively owns `[w .. w + to_write)` behind `r + RING_CAP`.
        let ring = unsafe { &mut *self.buf.get() };
        for (i, &s) in samples[..to_write].iter().enumerate() {
            ring[w.wrapping_add(i) & RING_MASK] = s;
        }
        self.write_idx
            .store(w.wrapping_add(to_write), Ordering::Release);
    }

    pub fn pop_into(&self, dst: &mut [i16]) {
        if dst.is_empty() {
            return;
        }
        let w = self.write_idx.load(Ordering::Acquire);
        let mut r = self.read_idx.load(Ordering::Relaxed);
        let mut avail = w.wrapping_sub(r);

        if avail > MAX_LATENCY_SAMPLES {
            r = w.wrapping_sub(TRIM_TARGET_SAMPLES);
            avail = TRIM_TARGET_SAMPLES;
        }

        if !self.primed.load(Ordering::Relaxed) {
            if avail < PREFILL_SAMPLES {
                dst.fill(0);
                self.read_idx.store(r, Ordering::Release);
                return;
            }
            self.primed.store(true, Ordering::Relaxed);
        }

        let to_read = avail.min(dst.len());
        // SAFETY: Consumer exclusively reads `[r .. r + to_read)` behind `w`.
        let ring = unsafe { &*self.buf.get() };
        for (i, out) in dst[..to_read].iter_mut().enumerate() {
            *out = ring[r.wrapping_add(i) & RING_MASK];
        }
        r = r.wrapping_add(to_read);
        self.read_idx.store(r, Ordering::Release);

        if to_read < dst.len() {
            dst[to_read..].fill(0);
            self.primed.store(false, Ordering::Relaxed);
        }
    }
}

#[cfg(target_os = "android")]
mod imp {
    use super::AudioRingBuffer;
    use anyhow::Result;
    use ndk::audio::{
        AudioCallbackResult, AudioContentType, AudioDirection, AudioFormat, AudioPerformanceMode,
        AudioSharingMode, AudioStream, AudioStreamBuilder, AudioUsage,
    };
    use std::ffi::c_void;
    use std::sync::Arc;

    pub struct QuestAudioPlayer {
        ring: Arc<AudioRingBuffer>,
        stop_tx: Option<tokio::sync::oneshot::Sender<()>>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl QuestAudioPlayer {
        pub fn start(sample_rate: i32, channels: i32) -> Result<Self> {
            let ring = Arc::new(AudioRingBuffer::new());
            let ring_thread = Arc::clone(&ring);
            let (init_tx, init_rx) = std::sync::mpsc::channel::<Result<()>>();
            let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

            let join = std::thread::Builder::new()
                .name("vdesk-aaudio".to_string())
                .spawn(move || {
                    let ring_cb = Arc::clone(&ring_thread);
                    let open_res = (|| -> Result<AudioStream> {
                        let stream = AudioStreamBuilder::new()?
                            .direction(AudioDirection::Output)
                            .sample_rate(sample_rate)
                            .channel_count(channels)
                            .format(AudioFormat::PCM_I16)
                            .performance_mode(AudioPerformanceMode::LowLatency)
                            .sharing_mode(AudioSharingMode::Shared)
                            .usage(AudioUsage::Media)
                            .content_type(AudioContentType::Movie)
                            .data_callback(Box::new(
                                move |stream, audio_data: *mut c_void, num_frames: i32| {
                                    if num_frames <= 0 || audio_data.is_null() {
                                        return AudioCallbackResult::Continue;
                                    }
                                    let ch = stream.channel_count().max(1) as usize;
                                    let total_samples = (num_frames as usize) * ch;
                                    let dst = unsafe {
                                        std::slice::from_raw_parts_mut(
                                            audio_data as *mut i16,
                                            total_samples,
                                        )
                                    };
                                    ring_cb.pop_into(dst);
                                    AudioCallbackResult::Continue
                                },
                            ))
                            .open_stream()?;

                        log::info!(
                            "AAudio playback started: {} Hz, {} ch, burst={} frames",
                            stream.sample_rate(),
                            stream.channel_count(),
                            stream.frames_per_burst()
                        );
                        stream.request_start()?;
                        Ok(stream)
                    })();

                    match open_res {
                        Ok(stream) => {
                            let _ = init_tx.send(Ok(()));
                            let _ = stop_rx.blocking_recv();
                            let _ = stream.request_stop();
                        }
                        Err(e) => {
                            let _ = init_tx.send(Err(e));
                        }
                    }
                })?;

            init_rx
                .recv()
                .map_err(|_| anyhow::anyhow!("AAudio init thread exited unexpectedly"))??;

            Ok(Self {
                ring,
                stop_tx: Some(stop_tx),
                join: Some(join),
            })
        }

        pub fn push_pcm(&self, samples: &[i16]) {
            self.ring.push(samples);
        }
    }

    impl Drop for QuestAudioPlayer {
        fn drop(&mut self) {
            drop(self.stop_tx.take());
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
mod imp {
    use super::AudioRingBuffer;
    use anyhow::Result;
    use std::sync::Arc;

    pub struct QuestAudioPlayer {
        ring: Arc<AudioRingBuffer>,
    }

    impl QuestAudioPlayer {
        pub fn start(_sample_rate: i32, _channels: i32) -> Result<Self> {
            Ok(Self {
                ring: Arc::new(AudioRingBuffer::new()),
            })
        }

        pub fn push_pcm(&self, samples: &[i16]) {
            self.ring.push(samples);
        }
    }
}

pub use imp::QuestAudioPlayer;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_ring_buffer_push_pop_and_trim() {
        let ring = AudioRingBuffer::new();

        // Below prefill threshold (960 samples): outputs silence
        let small = vec![123i16; 200];
        ring.push(&small);
        let mut out = vec![99i16; 100];
        ring.pop_into(&mut out);
        assert!(out.iter().all(|&s| s == 0));

        // Fill past PREFILL_SAMPLES and verify normal read
        let batch: Vec<i16> = (0..1000).map(|i| i as i16).collect();
        ring.push(&batch);
        let mut head = vec![0i16; 200];
        ring.pop_into(&mut head);
        assert_eq!(&head[..], &small[..]);

        // Overflow past MAX_LATENCY_SAMPLES (4800) trims down to TRIM_TARGET_SAMPLES (1920)
        // Consume remainder first
        let mut drain = vec![0i16; 1000];
        ring.pop_into(&mut drain);
        assert_eq!(&drain[..], &batch[..]);

        // Re-prime and push 6000 samples (0..6000) -> trim keeps newest 1920 samples (4080..6000)
        let big: Vec<i16> = (0..6000).map(|i| i as i16).collect();
        ring.push(&big);
        let mut trimmed_head = vec![0i16; 16];
        ring.pop_into(&mut trimmed_head);
        assert_eq!(trimmed_head[0], 6000 - TRIM_TARGET_SAMPLES as i16);
    }
}
