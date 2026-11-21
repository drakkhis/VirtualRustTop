//! KDE / Freedesktop StatusNotifierItem system tray controller for `vdesk-host.service`.

use anyhow::Result;
use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, Status, ToolTip, Tray, TrayMethods};
use std::process::Command;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceState {
    Running,
    Starting,
    Stopped,
    Failed,
}

impl ServiceState {
    fn label(self) -> &'static str {
        match self {
            ServiceState::Running => "Running",
            ServiceState::Starting => "Starting",
            ServiceState::Stopped => "Stopped",
            ServiceState::Failed => "Failed",
        }
    }

    fn dot_argb(self) -> [u8; 4] {
        match self {
            ServiceState::Running => [255, 34, 197, 94],   // green
            ServiceState::Starting => [255, 245, 158, 11], // amber
            ServiceState::Stopped => [255, 148, 163, 184], // slate gray
            ServiceState::Failed => [255, 239, 68, 68],    // red
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum TrayAction {
    Start,
    Stop,
    Restart,
    Quit,
}

struct VdeskTray {
    state: ServiceState,
    action_tx: mpsc::UnboundedSender<TrayAction>,
}

impl Tray for VdeskTray {
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "vdesk-host-tray".to_string()
    }

    fn title(&self) -> String {
        format!("VirtualRustTop Host ({})", self.state.label())
    }

    fn status(&self) -> Status {
        match self.state {
            ServiceState::Failed => Status::NeedsAttention,
            _ => Status::Active,
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        vec![render_vr_icon(self.state)]
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: "VirtualRustTop".to_string(),
            description: format!("Host service status: {}", self.state.label()),
            ..Default::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let tx_start = self.action_tx.clone();
        let tx_stop = self.action_tx.clone();
        let tx_restart = self.action_tx.clone();
        let tx_quit = self.action_tx.clone();

        let is_active = matches!(self.state, ServiceState::Running | ServiceState::Starting);

        vec![
            StandardItem {
                label: format!("VirtualRustTop — {}", self.state.label()),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Start Server".to_string(),
                enabled: !is_active,
                activate: Box::new(move |_| {
                    let _ = tx_start.send(TrayAction::Start);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Stop Server".to_string(),
                enabled: is_active,
                activate: Box::new(move |_| {
                    let _ = tx_stop.send(TrayAction::Stop);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Restart Server".to_string(),
                enabled: true,
                activate: Box::new(move |_| {
                    let _ = tx_restart.send(TrayAction::Restart);
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit Tray".to_string(),
                activate: Box::new(move |_| {
                    let _ = tx_quit.send(TrayAction::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

fn query_service_state(service_name: &str) -> ServiceState {
    let Ok(out) = Command::new("systemctl")
        .args(["--user", "is-active", service_name])
        .output()
    else {
        return ServiceState::Stopped;
    };
    match String::from_utf8_lossy(&out.stdout).trim() {
        "active" => ServiceState::Running,
        "activating" | "reloading" | "deactivating" => ServiceState::Starting,
        "failed" => ServiceState::Failed,
        _ => ServiceState::Stopped,
    }
}

fn run_systemctl(service_name: &str, verb: &str) {
    match Command::new("systemctl")
        .args(["--user", verb, service_name])
        .status()
    {
        Ok(st) if st.success() => info!("systemctl --user {} {} succeeded", verb, service_name),
        Ok(st) => warn!(
            "systemctl --user {} {} exited with status {}",
            verb, service_name, st
        ),
        Err(e) => warn!("Failed to execute systemctl --user {}: {:#}", verb, e),
    }
}

async fn run_systemctl_and_query(service_name: &str, verb: &'static str) -> ServiceState {
    let svc = service_name.to_string();
    tokio::task::spawn_blocking(move || {
        run_systemctl(&svc, verb);
        query_service_state(&svc)
    })
    .await
    .unwrap_or(ServiceState::Stopped)
}

pub async fn run_tray(service_name: String) -> Result<()> {
    let (action_tx, mut action_rx) = mpsc::unbounded_channel();
    let initial_state = query_service_state(&service_name);

    let mut handle = None;
    for attempt in 0..10 {
        let tray = VdeskTray {
            state: initial_state,
            action_tx: action_tx.clone(),
        };
        match tray.spawn().await {
            Ok(h) => {
                handle = Some(h);
                break;
            }
            Err(e) => {
                if attempt == 9 {
                    return Err(anyhow::anyhow!("Failed to spawn system tray icon: {}", e));
                }
                warn!("StatusNotifierWatcher not ready yet ({}); retrying...", e);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
    let handle = handle.expect("tray handle initialized");
    info!(
        "VirtualRustTop system tray active for {} (initial state: {})",
        service_name,
        initial_state.label()
    );

    let mut current_state = initial_state;
    let mut poll_tick = tokio::time::interval(Duration::from_secs(2));
    let mut sigterm = signal(SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                break;
            }
            _ = sigterm.recv() => {
                break;
            }
            _ = poll_tick.tick() => {
                let svc = service_name.clone();
                let st = tokio::task::spawn_blocking(move || query_service_state(&svc))
                    .await
                    .unwrap_or(current_state);
                if st != current_state {
                    current_state = st;
                    let _ = handle.update(move |t| t.state = st).await;
                }
            }
            Some(action) = action_rx.recv() => {
                match action {
                    TrayAction::Start => {
                        let _ = handle.update(|t| t.state = ServiceState::Starting).await;
                        let st = run_systemctl_and_query(&service_name, "start").await;
                        current_state = st;
                        let _ = handle.update(move |t| t.state = st).await;
                    }
                    TrayAction::Stop => {
                        let st = run_systemctl_and_query(&service_name, "stop").await;
                        current_state = st;
                        let _ = handle.update(move |t| t.state = st).await;
                    }
                    TrayAction::Restart => {
                        let _ = handle.update(|t| t.state = ServiceState::Starting).await;
                        let st = run_systemctl_and_query(&service_name, "restart").await;
                        current_state = st;
                        let _ = handle.update(move |t| t.state = st).await;
                    }
                    TrayAction::Quit => {
                        break;
                    }
                }
            }
        }
    }

    handle.shutdown().await;
    Ok(())
}

fn render_vr_icon(state: ServiceState) -> Icon {
    const W: usize = 32;
    const H: usize = 32;
    let mut data = vec![0u8; W * H * 4];

    let set_px = |buf: &mut [u8], x: usize, y: usize, argb: [u8; 4]| {
        if x < W && y < H {
            let i = (y * W + x) * 4;
            buf[i..i + 4].copy_from_slice(&argb);
        }
    };

    // Dark rounded square badge (3..28)
    for y in 2..30 {
        for x in 2..30 {
            let corner = !(6..=25).contains(&x) && !(6..=25).contains(&y);
            let extreme = (x == 2 || x == 29) && (y == 2 || y == 29);
            if extreme || (corner && ((x == 2 || x == 29) && !(5..=26).contains(&y))) {
                continue;
            }
            let border = x == 2 || x == 29 || y == 2 || y == 29;
            let col = if border {
                [255, 71, 85, 105]
            } else {
                [255, 15, 23, 42]
            };
            set_px(&mut data, x, y, col);
        }
    }

    // Crisp cyan VR visor silhouette in center
    for y in 11..20 {
        for x in 6..26 {
            let nose_cutout = y >= 17 && (14..=17).contains(&x);
            let outer_corner = (y == 11 || y == 19) && (x == 6 || x == 25);
            if nose_cutout || outer_corner {
                continue;
            }
            let inner = (8..24).contains(&x)
                && (13..18).contains(&y)
                && !(y >= 16 && (13..=18).contains(&x));
            let col = if inner {
                [255, 56, 189, 248]
            } else {
                [255, 226, 232, 240]
            };
            set_px(&mut data, x, y, col);
        }
    }

    // Status indicator dot in bottom-right corner (center ~25.5, 25.5, r ~4.5)
    let dot = state.dot_argb();
    for y in 20..32 {
        for x in 20..32 {
            let dx = x as f32 - 25.5;
            let dy = y as f32 - 25.5;
            let d = (dx * dx + dy * dy).sqrt();
            if d <= 5.2 {
                let col = if d <= 3.7 { dot } else { [255, 15, 23, 42] };
                set_px(&mut data, x, y, col);
            }
        }
    }

    Icon {
        width: W as i32,
        height: H as i32,
        data,
    }
}
