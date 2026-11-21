use ashpd::desktop::{
    remote_desktop::{DeviceType, RemoteDesktop},
    screencast::{CursorMode, Screencast, SourceType, Stream as PortalStream},
    PersistMode, Session,
};
use ashpd::WindowIdentifier;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use tracing::{info, warn};

pub struct ActivePortalSession {
    pub pipewire_fd: OwnedFd,
    pub streams: Vec<PortalStream>,
    pub remote_desktop: RemoteDesktop<'static>,
    pub session: Session<'static, RemoteDesktop<'static>>,
}

fn restore_token_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("vdesk-rs").join("portal_restore_token"))
}

fn load_restore_token() -> Option<String> {
    let path = restore_token_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn save_restore_token(token: &str) {
    let Some(path) = restore_token_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, token);
}

async fn try_start_session(
    restore_token: Option<&str>,
    persist_mode: PersistMode,
) -> anyhow::Result<ActivePortalSession> {
    let remote_desktop = RemoteDesktop::new().await?;
    let screencast = Screencast::new().await?;

    let session = remote_desktop.create_session().await?;

    remote_desktop
        .select_devices(
            &session,
            DeviceType::Keyboard | DeviceType::Pointer,
            restore_token,
            persist_mode,
        )
        .await?;

    // Embed the cursor directly into the DMA-BUF for lowest latency.
    screencast
        .select_sources(
            &session,
            CursorMode::Embedded,
            SourceType::Monitor.into(),
            true, // Multiple monitors allowed
            None,
            PersistMode::DoNot,
        )
        .await?;

    let response = remote_desktop
        .start(&session, &WindowIdentifier::default())
        .await?
        .response()?;

    if let Some(token) = response.restore_token() {
        if !token.is_empty() {
            save_restore_token(token);
            info!("Saved Wayland portal restore token");
        }
    }

    let streams = response.streams().unwrap_or_default().to_vec();
    let pipewire_fd = screencast.open_pipe_wire_remote(&session).await?;

    info!(
        "Wayland portal session started with {} stream(s)",
        streams.len()
    );
    for s in &streams {
        info!(
            "  Stream node_id={}, size={:?}, pos={:?}",
            s.pipe_wire_node_id(),
            s.size(),
            s.position()
        );
    }

    Ok(ActivePortalSession {
        pipewire_fd,
        streams,
        remote_desktop,
        session,
    })
}

pub async fn init_wayland_session() -> anyhow::Result<ActivePortalSession> {
    let saved = load_restore_token();
    match try_start_session(saved.as_deref(), PersistMode::ExplicitlyRevoked).await {
        Ok(sess) => Ok(sess),
        Err(e) => {
            warn!(
                "Persistent portal session failed ({:#}); retrying ephemeral session",
                e
            );
            try_start_session(None, PersistMode::DoNot).await
        }
    }
}
