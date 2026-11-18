//! PipeWire virtual audio sink capture + automatic PC audio mute/restore.

use anyhow::{Context, Result};
use libspa as spa;
use libspa::buffer::ChunkFlags;
use libspa::pod::Pod;
use pipewire as pw;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{info, warn};

pub const VIRTUAL_SINK_NAME: &str = "vdesk_quest_audio";
pub const CHUNK_SAMPLES: usize = 480; // 240 stereo frames @ 48 kHz = 5 ms

pub struct AudioCaptureHandle {
    quit_tx: pw::channel::Sender<()>,
    join: Option<JoinHandle<Result<()>>>,
}

impl AudioCaptureHandle {
    pub fn stop_and_join(&mut self) {
        let _ = self.quit_tx.send(());
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for AudioCaptureHandle {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

struct AudioRouterInner {
    active_clients: usize,
    prev_sink: Option<String>,
    handle: Option<AudioCaptureHandle>,
}

pub struct AudioRouter {
    inner: Mutex<AudioRouterInner>,
    packet_tx: broadcast::Sender<Arc<Vec<i16>>>,
    enabled: bool,
}

pub struct AudioClientGuard {
    router: Arc<AudioRouter>,
}

impl Drop for AudioClientGuard {
    fn drop(&mut self) {
        self.router.client_disconnected();
    }
}

impl AudioRouter {
    pub fn new() -> Arc<Self> {
        let (packet_tx, _) = broadcast::channel(64);
        Arc::new(Self {
            inner: Mutex::new(AudioRouterInner {
                active_clients: 0,
                prev_sink: None,
                handle: None,
            }),
            packet_tx,
            enabled: true,
        })
    }

    #[allow(dead_code)]
    pub fn new_disabled() -> Arc<Self> {
        let (packet_tx, _) = broadcast::channel(16);
        Arc::new(Self {
            inner: Mutex::new(AudioRouterInner {
                active_clients: 0,
                prev_sink: None,
                handle: None,
            }),
            packet_tx,
            enabled: false,
        })
    }

    pub fn client_connected(
        self: &Arc<Self>,
    ) -> (broadcast::Receiver<Arc<Vec<i16>>>, AudioClientGuard) {
        let rx = self.packet_tx.subscribe();
        let should_start = {
            let mut inner = self.inner.lock().expect("AudioRouter mutex poisoned");
            inner.active_clients += 1;
            if self.enabled && inner.active_clients == 1 {
                inner.prev_sink = pactl_get_default_sink();
                info!("Routing desktop audio to Quest 3 (saved default sink={:?})", inner.prev_sink);
                inner.handle = Some(spawn_virtual_sink_thread(self.packet_tx.clone()));
                true
            } else {
                false
            }
        };

        if should_start {
            let router = Arc::clone(self);
            std::thread::spawn(move || {
                for _ in 0..30 {
                    if sink_exists(VIRTUAL_SINK_NAME) {
                        let inner = router.inner.lock().expect("AudioRouter mutex poisoned");
                        if inner.active_clients > 0 {
                            route_all_streams_to_sink(VIRTUAL_SINK_NAME);
                            info!("Switched default audio output to {} (PC audio muted)", VIRTUAL_SINK_NAME);
                        }
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                warn!("Timed out waiting for PipeWire sink {} to appear", VIRTUAL_SINK_NAME);
            });
        }

        (
            rx,
            AudioClientGuard {
                router: Arc::clone(self),
            },
        )
    }

    fn client_disconnected(&self) {
        let mut inner = self.inner.lock().expect("AudioRouter mutex poisoned");
        inner.active_clients = inner.active_clients.saturating_sub(1);
        if inner.active_clients == 0 {
            self.stop_and_restore_locked(&mut inner);
        }
    }

    pub fn force_restore(&self) {
        let mut inner = self.inner.lock().expect("AudioRouter mutex poisoned");
        inner.active_clients = 0;
        self.stop_and_restore_locked(&mut inner);
    }

    fn stop_and_restore_locked(&self, inner: &mut AudioRouterInner) {
        let prev_sink = inner.prev_sink.take();
        let mut handle = inner.handle.take();

        if self.enabled {
            if let Some(ref sink) = prev_sink {
                route_all_streams_to_sink(sink);
                info!("Restored PC default audio output to {}", sink);
            }
        }
        if let Some(ref mut h) = handle {
            h.stop_and_join();
        }
    }
}

pub fn spawn_virtual_sink_thread(
    packet_tx: broadcast::Sender<Arc<Vec<i16>>>,
) -> AudioCaptureHandle {
    let (quit_tx, quit_rx) = pw::channel::channel();
    let join = std::thread::spawn(move || {
        let res = run_virtual_sink_loop(packet_tx, quit_rx);
        if let Err(ref e) = res {
            warn!("PipeWire virtual audio loop exited with error: {:#}", e);
        }
        res
    });
    AudioCaptureHandle {
        quit_tx,
        join: Some(join),
    }
}

fn run_virtual_sink_loop(
    packet_tx: broadcast::Sender<Arc<Vec<i16>>>,
    quit_rx: pw::channel::Receiver<()>,
) -> Result<()> {
    pw::init();

    let mainloop = std::rc::Rc::new(
        pw::main_loop::MainLoopBox::new(None).context("Failed to create PipeWire MainLoop")?,
    );
    let context =
        pw::context::ContextBox::new(mainloop.loop_(), None).context("Failed to create PW Context")?;
    let core = context
        .connect(None)
        .context("Failed to connect to PipeWire core")?;

    let _sink = core
        .create_object::<pw::node::Node>(
            "adapter",
            &pw::properties::properties! {
                "factory.name" => "support.null-audio-sink",
                "node.name" => VIRTUAL_SINK_NAME,
                "node.description" => "VirtualRustTop Quest 3",
                "media.class" => "Audio/Sink",
                "audio.position" => "FL,FR",
                "object.linger" => "false",
            },
        )
        .context("Failed to create PipeWire virtual audio sink")?;

    let props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
        *pw::keys::STREAM_CAPTURE_SINK => "true",
        "target.object" => VIRTUAL_SINK_NAME,
        "node.name" => "vdesk_quest_audio_capture",
        *pw::keys::NODE_LATENCY => "240/48000",
    };

    let stream = pw::stream::StreamBox::new(&core, "vdesk-audio-capture", props)
        .context("Failed to create PipeWire audio capture stream")?;

    let mut pending: Vec<i16> = Vec::with_capacity(CHUNK_SAMPLES * 2);
    let _listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let chunk = data.chunk();
            if chunk.flags().contains(ChunkFlags::CORRUPTED) || chunk.size() == 0 {
                return;
            }
            let offset = chunk.offset() as usize;
            let size = chunk.size() as usize;
            if let Some(bytes) = data.data() {
                if offset + size <= bytes.len() {
                    let pcm_bytes = &bytes[offset..offset + size];
                    for pair in pcm_bytes.chunks_exact(2) {
                        pending.push(i16::from_le_bytes([pair[0], pair[1]]));
                    }
                    while pending.len() >= CHUNK_SAMPLES {
                        let chunk_vec: Vec<i16> = pending.drain(..CHUNK_SAMPLES).collect();
                        let _ = packet_tx.send(Arc::new(chunk_vec));
                    }
                }
            }
        })
        .register()
        .context("Failed to register PipeWire audio capture listener")?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::S16LE);
    audio_info.set_rate(48_000);
    audio_info.set_channels(2);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    audio_info.set_position(position);

    let obj = spa::pod::Object {
        type_: spa::sys::SPA_TYPE_OBJECT_Format,
        id: spa::sys::SPA_PARAM_EnumFormat,
        properties: audio_info.into(),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow::anyhow!("Failed to serialize audio format POD: {:?}", e))?
    .0
    .into_inner();

    let param_pod = Pod::from_bytes(&values).context("Invalid audio format POD")?;
    let mut params = [param_pod];

    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .context("Failed to connect PipeWire audio capture stream")?;

    let mainloop_weak = std::rc::Rc::downgrade(&mainloop);
    let _quit_listener = quit_rx.attach(mainloop.loop_(), move |_| {
        if let Some(ml) = mainloop_weak.upgrade() {
            ml.quit();
        }
    });

    info!(
        "PipeWire virtual sink {} + 48kHz stereo capture active",
        VIRTUAL_SINK_NAME
    );
    mainloop.run();
    Ok(())
}

fn pactl_get_default_sink() -> Option<String> {
    let out = Command::new("pactl")
        .arg("get-default-sink")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sink = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sink.is_empty() || sink == VIRTUAL_SINK_NAME {
        None
    } else {
        Some(sink)
    }
}

fn sink_exists(name: &str) -> bool {
    let Ok(out) = Command::new("pactl").args(["list", "short", "sinks"]).output() else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|line| line.split_whitespace().nth(1) == Some(name))
}

fn route_all_streams_to_sink(sink_name: &str) {
    let _ = Command::new("pactl")
        .args(["set-default-sink", sink_name])
        .status();

    let Ok(out) = Command::new("pactl")
        .args(["list", "short", "sink-inputs"])
        .output()
    else {
        return;
    };
    if !out.status.success() {
        return;
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(input_id) = line.split_whitespace().next() {
            let _ = Command::new("pactl")
                .args(["move-sink-input", input_id, sink_name])
                .status();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_sink_creates_and_cleans_up() {
        let (tx, _rx) = broadcast::channel(8);
        let mut handle = spawn_virtual_sink_thread(tx);

        let mut appeared = false;
        for _ in 0..40 {
            if sink_exists(VIRTUAL_SINK_NAME) {
                appeared = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(appeared, "Virtual PipeWire sink did not appear in pactl list short sinks");

        handle.stop_and_join();

        let mut removed = false;
        for _ in 0..20 {
            if !sink_exists(VIRTUAL_SINK_NAME) {
                removed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(removed, "Virtual PipeWire sink lingered after stop");
    }
}
