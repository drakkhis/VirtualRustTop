mod audio;
mod capture;
mod input;
mod net;
mod nvenc;
mod portal;
mod tray;

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use vdesk_protocol::{InputEvent, MonitorDescriptor, VideoCodec};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--tray") {
        return tray::run_tray().await;
    }
    let test_input_only = args.iter().any(|a| a == "--test-uinput");
    let test_portal = args.iter().any(|a| a == "--test-portal");
    let dump_frames: Option<usize> = args
        .windows(2)
        .find(|w| w[0] == "--dump-frames")
        .and_then(|w| w[1].parse().ok());
    let wiggle_uinput: Option<usize> = args
        .windows(2)
        .find(|w| w[0] == "--wiggle-uinput")
        .and_then(|w| w[1].parse().ok());

    info!("vdesk-host starting up");

    if let Some(count) = wiggle_uinput {
        info!("Running --wiggle-uinput for {} iterations...", count);
        let mut injector = input::KernelInputInjector::new(2560, 1440)?;
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
                2560,
                1440,
            )?;
            tokio::time::sleep(std::time::Duration::from_millis(16)).await;
        }
        info!("Completed --wiggle-uinput ({} events)", count);
        return Ok(());
    }

    if test_input_only {
        info!("Running /dev/uinput registration test...");
        let mut injector = input::KernelInputInjector::new(2560, 1440)?;
        info!("Registered virtual input device: vdesk-rs Quest 3 Spatial Input");
        injector.inject(
            InputEvent::PointerMotionAbsolute {
                stream_id: 0,
                u: 0.5,
                v: 0.5,
            },
            0,
            0,
            2560,
            1440,
        )?;
        info!("Injected center pointer motion event via /dev/uinput");
        return Ok(());
    }

    let portal_session = portal::init_wayland_session().await?;

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
            refresh_rate_mhz: 90_000,
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

    if test_portal {
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

    let (cap_tx, mut cap_rx) = mpsc::channel::<capture::EncodedFrame>(128);

    if let Some(target_frames) = dump_frames {
        // Pick the largest monitor (typically the primary landscape display) for single-file dump
        let first_node_id = monitors
            .iter()
            .max_by_key(|m| m.width * m.height)
            .map(|m| m.stream_id)
            .ok_or_else(|| anyhow::anyhow!("No PipeWire streams returned by portal"))?;

        let _capture_handle = capture::spawn_capture_thread(
            Some(portal_session.pipewire_fd),
            first_node_id,
            VideoCodec::Av1YUV420,
            25_000,
            Arc::new(AtomicBool::new(false)),
            cap_tx,
        );

        let out_path = "vdesk_dump.av1";
        let mut file = std::fs::File::create(out_path)?;
        let mut count = 0usize;
        info!("Dumping {} encoded frames to {}...", target_frames, out_path);
        while let Some(frame) = cap_rx.recv().await {
            file.write_all(&frame.payload)?;
            count += 1;
            if count >= target_frames {
                break;
            }
        }
        info!("Dumped {} frames to {}", count, out_path);
        return Ok(());
    }

    if monitors.is_empty() {
        anyhow::bail!("No PipeWire streams returned by portal");
    }

    let mut capture_handles = Vec::with_capacity(monitors.len());
    let mut idr_flags: HashMap<u32, Arc<AtomicBool>> = HashMap::new();
    let mut pw_fd = Some(portal_session.pipewire_fd);
    for mon in &monitors {
        let force_idr = Arc::new(AtomicBool::new(false));
        idr_flags.insert(mon.stream_id, force_idr.clone());
        info!(
            "Spawning PipeWire capture thread for {} (node_id={}, {}x{})",
            mon.name, mon.stream_id, mon.width, mon.height
        );
        capture_handles.push(capture::spawn_capture_thread(
            pw_fd.take(),
            mon.stream_id,
            mon.codec,
            25_000,
            force_idr,
            cap_tx.clone(),
        ));
    }
    let idr_flags = Arc::new(idr_flags);

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

    let (bcast_tx, _) = broadcast::channel::<Arc<capture::EncodedFrame>>(128);
    let bcast_tx_clone = bcast_tx.clone();
    let bcast_task = tokio::spawn(async move {
        while let Some(frame) = cap_rx.recv().await {
            let _ = bcast_tx_clone.send(Arc::new(frame));
        }
    });

    let audio_router = audio::AudioRouter::new();
    let audio_router_net = audio_router.clone();

    let (input_tx, mut input_rx) = mpsc::channel::<InputEvent>(256);
    let quic_server = net::QuicServer::default_bind()?;
    let quic_task = tokio::spawn(async move {
        if let Err(e) = quic_server
            .run(monitors, bcast_tx, idr_flags, audio_router_net, input_tx)
            .await
        {
            warn!("QUIC server exited: {:#}", e);
        }
    });

    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

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
    bcast_task.abort();
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
