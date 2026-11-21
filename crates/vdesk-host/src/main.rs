mod audio;
mod capture;
mod input;
mod net;
mod nvenc;
mod portal;
mod tray;

use clap::Parser;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use vdesk_protocol::{InputEvent, MonitorDescriptor, VideoCodec};

#[derive(Parser, Debug)]
#[command(
    name = "vdesk-host",
    version,
    about = "VirtualRustTop Linux Wayland -> Quest 3 virtual desktop host"
)]
struct Cli {
    /// QUIC UDP listen port
    #[arg(long, env = "VDESK_PORT", default_value_t = vdesk_protocol::DEFAULT_PORT)]
    port: u16,

    /// UDP LAN auto-discovery responder port (0 disables discovery)
    #[arg(long, env = "VDESK_DISCOVER_PORT", default_value_t = vdesk_protocol::DISCOVER_PORT)]
    discover_port: u16,

    /// Maximum capture/encode frame rate (30-120 FPS)
    #[arg(long, env = "VDESK_MAX_FPS", default_value_t = 60, value_parser = clap::value_parser!(u32).range(30..=120))]
    max_fps: u32,

    /// Fixed target bitrate override in kbps (auto-scaled per monitor when unset)
    #[arg(long, env = "VDESK_BITRATE_KBPS")]
    bitrate_kbps: Option<u32>,

    /// Disable PipeWire virtual audio sink routing to the Quest client
    #[arg(long, env = "VDESK_NO_AUDIO")]
    no_audio: bool,

    /// Run the KDE / Freedesktop system tray controller instead of the streamer
    #[arg(long)]
    tray: bool,

    /// Systemd user unit controlled by --tray
    #[arg(long, env = "VDESK_SERVICE_NAME", default_value = "vdesk-host.service")]
    service: String,

    /// Verify /dev/uinput virtual device registration and exit
    #[arg(long)]
    test_uinput: bool,

    /// Verify XDG Desktop Portal initialization and exit
    #[arg(long)]
    test_portal: bool,

    /// Dump N encoded AV1 frames from the primary monitor and exit
    #[arg(long, value_name = "COUNT")]
    dump_frames: Option<usize>,

    /// Output file path used with --dump-frames
    #[arg(long, default_value = "vdesk_dump.av1")]
    dump_output: PathBuf,

    /// Inject N synthetic pointer motion events via /dev/uinput and exit
    #[arg(long, value_name = "COUNT")]
    wiggle_uinput: Option<usize>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    if cli.tray {
        return tray::run_tray(cli.service).await;
    }

    info!("vdesk-host starting up");

    if let Some(count) = cli.wiggle_uinput {
        info!("Running --wiggle-uinput for {} iterations...", count);
        let mut injector = input::KernelInputInjector::new(1920, 1080)?;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        for i in 0..count {
            let u = 0.45 + ((i % 20) as f32) * 0.005;
            injector.inject(
                InputEvent::PointerMotionAbsolute {
                    stream_id: 0,
                    u,
                    v: 0.5,
                },
                0,
                0,
                1920,
                1080,
            )?;
            tokio::time::sleep(std::time::Duration::from_millis(16)).await;
        }
        info!("Completed --wiggle-uinput ({} events)", count);
        return Ok(());
    }

    if cli.test_uinput {
        info!("Running /dev/uinput registration test...");
        let mut injector = input::KernelInputInjector::new(1920, 1080)?;
        info!("Registered virtual input device: vdesk-rs Quest 3 Spatial Input");
        injector.inject(
            InputEvent::PointerMotionAbsolute {
                stream_id: 0,
                u: 0.5,
                v: 0.5,
            },
            0,
            0,
            1920,
            1080,
        )?;
        info!("Injected center pointer motion event via /dev/uinput");
        return Ok(());
    }

    let portal_session = portal::init_wayland_session().await?;

    let max_fps = cli.max_fps;
    let bitrate_override = cli.bitrate_kbps;
    info!(
        "Streaming config: port={}, discover_port={}, max_fps={}, bitrate_override={:?}, audio_enabled={}",
        cli.port,
        cli.discover_port,
        max_fps,
        bitrate_override,
        !cli.no_audio
    );

    let mut max_w = 1920i32;
    let mut max_h = 1080i32;
    let mut monitors = Vec::new();
    let mut monitor_bounds: HashMap<u32, (i32, i32, u32, u32)> = HashMap::new();

    for (idx, stream) in portal_session.streams.iter().enumerate() {
        let node_id = stream.pipe_wire_node_id();
        let (x, y) = stream.position().unwrap_or((0, 0));
        let (w, h) = stream.size().unwrap_or((1920, 1080));
        max_w = max_w.max(x + w);
        max_h = max_h.max(y + h);

        monitor_bounds.insert(node_id, (x, y, w as u32, h as u32));
        monitors.push(MonitorDescriptor {
            stream_id: node_id,
            name: format!("Monitor-{}", idx + 1),
            width: w as u32,
            height: h as u32,
            refresh_rate_mhz: max_fps * 1000,
            logical_x: x,
            logical_y: y,
            codec: VideoCodec::Av1YUV420,
        });
    }

    let mut injector = input::KernelInputInjector::new(max_w, max_h)?;
    info!(
        "Initialized KernelInputInjector with desktop bounds {}x{}",
        max_w, max_h
    );

    if cli.test_portal {
        if let Some(mon) = monitors.first() {
            info!(
                "Testing portal notify_pointer_motion_absolute on stream {} ({}x{})",
                mon.stream_id, mon.width, mon.height
            );
            portal_session
                .remote_desktop
                .notify_pointer_motion_absolute(
                    &portal_session.session,
                    mon.stream_id,
                    (mon.width as f64) * 0.5,
                    (mon.height as f64) * 0.5,
                )
                .await?;
            injector.inject(
                InputEvent::PointerMotionAbsolute {
                    stream_id: mon.stream_id,
                    u: 0.5,
                    v: 0.5,
                },
                mon.logical_x,
                mon.logical_y,
                mon.width,
                mon.height,
            )?;
            info!("Portal + uinput test completed successfully");
        }
        return Ok(());
    }

    if let Some(target_frames) = cli.dump_frames {
        // Pick the largest monitor (typically the primary landscape display) for single-file dump
        let primary = monitors
            .iter()
            .max_by_key(|m| m.width * m.height)
            .ok_or_else(|| anyhow::anyhow!("No PipeWire streams returned by portal"))?;

        let (def_kbps, min_kbps, max_kbps) =
            capture::default_bitrate_bounds(primary.width, primary.height, bitrate_override);
        let control = Arc::new(capture::StreamControl::new(def_kbps, min_kbps, max_kbps));
        let active_clients = Arc::new(AtomicUsize::new(1));
        let (dump_tx, mut dump_rx) = broadcast::channel::<Arc<capture::EncodedFrame>>(24);

        let _capture_handle = capture::spawn_capture_thread(
            Some(portal_session.pipewire_fd),
            primary.stream_id,
            VideoCodec::Av1YUV420,
            max_fps,
            bitrate_override,
            control,
            active_clients,
            dump_tx,
        );

        let mut file = std::fs::File::create(&cli.dump_output)?;
        let mut count = 0usize;
        info!(
            "Dumping {} encoded frames to {}...",
            target_frames,
            cli.dump_output.display()
        );
        while let Ok(frame) = dump_rx.recv().await {
            file.write_all(&frame.payload)?;
            count += 1;
            if count >= target_frames {
                break;
            }
        }
        info!("Dumped {} frames to {}", count, cli.dump_output.display());
        return Ok(());
    }

    if monitors.is_empty() {
        anyhow::bail!("No PipeWire streams returned by portal");
    }

    let mut capture_handles = Vec::with_capacity(monitors.len());
    let mut stream_controls: HashMap<u32, Arc<capture::StreamControl>> = HashMap::new();
    let mut monitor_channels: HashMap<u32, broadcast::Sender<Arc<capture::EncodedFrame>>> =
        HashMap::new();
    let active_clients = Arc::new(AtomicUsize::new(0));
    let mut pw_fd = Some(portal_session.pipewire_fd);
    for mon in &monitors {
        let (def_kbps, min_kbps, max_kbps) =
            capture::default_bitrate_bounds(mon.width, mon.height, bitrate_override);
        let control = Arc::new(capture::StreamControl::new(def_kbps, min_kbps, max_kbps));
        let (frame_tx, _) = broadcast::channel::<Arc<capture::EncodedFrame>>(24);
        stream_controls.insert(mon.stream_id, control.clone());
        monitor_channels.insert(mon.stream_id, frame_tx.clone());
        info!(
            "Spawning PipeWire capture thread for {} (node_id={}, {}x{}, max_fps={})",
            mon.name, mon.stream_id, mon.width, mon.height, max_fps
        );
        capture_handles.push(capture::spawn_capture_thread(
            pw_fd.take(),
            mon.stream_id,
            mon.codec,
            max_fps,
            bitrate_override,
            control,
            active_clients.clone(),
            frame_tx,
        ));
    }
    let stream_controls = Arc::new(stream_controls);

    // Nudge pointer on each monitor so KWin emits initial PipeWire frames on a static desktop.
    for mon in &monitors {
        let _ = portal_session
            .remote_desktop
            .notify_pointer_motion_absolute(
                &portal_session.session,
                mon.stream_id,
                (mon.width as f64) * 0.5,
                (mon.height as f64) * 0.5,
            )
            .await;
        let _ = injector.inject(
            InputEvent::PointerMotionAbsolute {
                stream_id: mon.stream_id,
                u: 0.5,
                v: 0.5,
            },
            mon.logical_x,
            mon.logical_y,
            mon.width,
            mon.height,
        );
    }

    let audio_router = if cli.no_audio {
        audio::AudioRouter::new_disabled()
    } else {
        audio::AudioRouter::new()
    };
    let audio_router_net = audio_router.clone();

    let (input_tx, mut input_rx) = mpsc::channel::<InputEvent>(256);
    let quic_server = net::QuicServer::bind_with_discover(cli.port, cli.discover_port)?;
    let quic_task = tokio::spawn(async move {
        if let Err(e) = quic_server
            .run(
                monitors,
                monitor_channels,
                stream_controls,
                active_clients,
                audio_router_net,
                input_tx,
            )
            .await
        {
            warn!("QUIC server exited: {:#}", e);
        }
    });

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    // Hybrid input dispatch loop
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Shutting down vdesk-host (Ctrl-C)");
                break;
            }
            _ = sigterm.recv() => {
                info!("Shutting down vdesk-host (SIGTERM)");
                break;
            }
            Some(ev) = input_rx.recv() => {
                match ev {
                    InputEvent::PointerMotionAbsolute { stream_id, u, v } => {
                        if let Some(&(ox, oy, w, h)) = monitor_bounds.get(&stream_id) {
                            let _ = portal_session.remote_desktop.notify_pointer_motion_absolute(
                                &portal_session.session,
                                stream_id,
                                (u.clamp(0.0, 1.0) as f64) * (w as f64),
                                (v.clamp(0.0, 1.0) as f64) * (h as f64),
                            ).await;
                            let _ = injector.inject(ev, ox, oy, w, h);
                        }
                    }
                    other => {
                        let _ = injector.inject(other, 0, 0, max_w as u32, max_h as u32);
                    }
                }
            }
        }
    }

    audio_router.force_restore();
    quic_task.abort();
    for handle in &mut capture_handles {
        handle.stop_and_join();
    }
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        portal_session.session.close(),
    )
    .await;
    std::process::exit(0);
}
