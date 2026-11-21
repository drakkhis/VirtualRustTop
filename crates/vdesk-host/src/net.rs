use crate::capture::{EncodedFrame, StreamControl};
use anyhow::{Context, Result};
use bytes::Bytes;
use quinn::{
    AckFrequencyConfig, Endpoint, EndpointConfig, RecvStream, SendStream, ServerConfig,
    TransportConfig, VarInt,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, info, warn};
use vdesk_protocol::{
    encode_audio_packet, ControlMessage, InputEvent, MonitorDescriptor, VideoFrameHeader,
    ALPN_VDESK, AUDIO_CHANNELS, AUDIO_HEADER_LEN, AUDIO_SAMPLE_RATE, DEFAULT_PORT, DISCOVER_PING,
    DISCOVER_PORT, DISCOVER_REPLY_PREFIX,
};

const UDP_SOCKET_BUF_BYTES: libc::c_int = 4 * 1024 * 1024;
const DSCP_AF41_TOS: libc::c_int = 0x88;

pub fn tune_udp_socket(sock: &std::net::UdpSocket, label: &str) {
    let fd = sock.as_raw_fd();
    unsafe {
        let buf_sz = UDP_SOCKET_BUF_BYTES;
        let _ = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&buf_sz as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        let _ = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&buf_sz as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        if sock.local_addr().is_ok_and(|a| a.is_ipv4()) {
            let tos = DSCP_AF41_TOS;
            let _ = libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                libc::IP_TOS,
                (&tos as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }

        let mut sndbuf: libc::c_int = 0;
        let mut rcvbuf: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let _ = libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&mut sndbuf as *mut libc::c_int).cast(),
            &mut len,
        );
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let _ = libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&mut rcvbuf as *mut libc::c_int).cast(),
            &mut len,
        );
        info!(
            "{} UDP socket tuned: SO_SNDBUF={} KB, SO_RCVBUF={} KB",
            label,
            sndbuf / 1024,
            rcvbuf / 1024
        );
    }
}

pub fn build_low_latency_transport() -> TransportConfig {
    let mut transport = TransportConfig::default();
    transport.max_idle_timeout(Some(
        Duration::from_secs(10)
            .try_into()
            .expect("valid idle timeout"),
    ));
    transport.keep_alive_interval(Some(Duration::from_secs(2)));
    transport.initial_rtt(Duration::from_millis(10));
    transport.stream_receive_window(VarInt::from_u32(8 * 1024 * 1024));
    transport.receive_window(VarInt::from_u32(16 * 1024 * 1024));
    transport.send_window(16 * 1024 * 1024);
    transport.datagram_receive_buffer_size(Some(2 * 1024 * 1024));
    transport.datagram_send_buffer_size(2 * 1024 * 1024);

    let mut bbr = quinn::congestion::BbrConfig::default();
    bbr.initial_window(256 * 1024);
    transport.congestion_controller_factory(Arc::new(bbr));

    let mut ack_cfg = AckFrequencyConfig::default();
    ack_cfg.max_ack_delay(Some(Duration::from_millis(5)));
    ack_cfg.ack_eliciting_threshold(2u32.into());
    transport.ack_frequency_config(Some(ack_cfg));
    transport
}

struct ClientCountGuard {
    active_clients: Arc<AtomicUsize>,
    stream_controls: Arc<HashMap<u32, Arc<StreamControl>>>,
}

impl ClientCountGuard {
    fn enter(
        active_clients: Arc<AtomicUsize>,
        stream_controls: Arc<HashMap<u32, Arc<StreamControl>>>,
    ) -> Self {
        active_clients.fetch_add(1, Ordering::AcqRel);
        Self {
            active_clients,
            stream_controls,
        }
    }
}

impl Drop for ClientCountGuard {
    fn drop(&mut self) {
        if self.active_clients.fetch_sub(1, Ordering::AcqRel) == 1 {
            for ctrl in self.stream_controls.values() {
                ctrl.active.store(true, Ordering::Release);
                ctrl.target_bitrate_kbps
                    .store(ctrl.default_kbps(), Ordering::Release);
            }
        }
    }
}

struct ClientMetrics {
    dropped_frames: AtomicU64,
    slow_sends: AtomicU64,
    frames_sent: AtomicU64,
}

impl ClientMetrics {
    fn new() -> Self {
        Self {
            dropped_frames: AtomicU64::new(0),
            slow_sends: AtomicU64::new(0),
            frames_sent: AtomicU64::new(0),
        }
    }
}

pub struct QuicServer {
    endpoint: Endpoint,
    discover_port: u16,
}

impl QuicServer {
    pub fn bind_with_discover(port: u16, discover_port: u16) -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec![
            "vdesk-host.local".to_string(),
            "localhost".to_string(),
        ])
        .context("Failed to generate self-signed TLS certificate")?;

        let cert_der = CertificateDer::from(cert.cert);
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));

        let mut tls_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .context("Failed to build rustls ServerConfig")?;
        tls_config.alpn_protocols = vec![ALPN_VDESK.to_vec()];

        let transport = build_low_latency_transport();
        let mut server_config = ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(tls_config)?,
        ));
        server_config.transport_config(Arc::new(transport));

        let addr: SocketAddr = ([0, 0, 0, 0], port).into();
        let std_socket = std::net::UdpSocket::bind(addr)
            .with_context(|| format!("Failed to bind UDP socket on {}", addr))?;
        tune_udp_socket(&std_socket, "QUIC server");
        let runtime =
            quinn::default_runtime().ok_or_else(|| anyhow::anyhow!("No Tokio runtime found"))?;
        let endpoint = Endpoint::new(
            EndpointConfig::default(),
            Some(server_config),
            std_socket,
            runtime,
        )
        .with_context(|| format!("Failed to initialize QUIC server on {}", addr))?;

        info!("QUIC server listening on udp://0.0.0.0:{}", port);
        Ok(Self {
            endpoint,
            discover_port,
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn bind(port: u16) -> Result<Self> {
        Self::bind_with_discover(port, if port == 0 { 0 } else { DISCOVER_PORT })
    }

    #[allow(dead_code)]
    pub fn default_bind() -> Result<Self> {
        Self::bind_with_discover(DEFAULT_PORT, DISCOVER_PORT)
    }

    #[allow(dead_code)]
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    pub async fn run(
        self,
        monitors: Vec<MonitorDescriptor>,
        monitor_channels: HashMap<u32, broadcast::Sender<Arc<EncodedFrame>>>,
        stream_controls: Arc<HashMap<u32, Arc<StreamControl>>>,
        active_clients: Arc<AtomicUsize>,
        audio_router: Arc<crate::audio::AudioRouter>,
        input_tx: mpsc::Sender<InputEvent>,
    ) -> Result<()> {
        let latest_idrs: Arc<RwLock<HashMap<u32, Arc<EncodedFrame>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        for (&stream_id, tx) in &monitor_channels {
            let mut cache_rx = tx.subscribe();
            let cache_map = latest_idrs.clone();
            tokio::spawn(async move {
                loop {
                    match cache_rx.recv().await {
                        Ok(frame) if frame.is_idr => {
                            cache_map.write().await.insert(stream_id, frame);
                        }
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }

        let quic_port = self
            .endpoint
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(DEFAULT_PORT);
        let discover_port = self.discover_port;
        if discover_port != 0 {
            tokio::spawn(async move {
                match tokio::net::UdpSocket::bind(("0.0.0.0", discover_port)).await {
                    Ok(sock) => {
                        info!(
                            "LAN discovery responder listening on udp://0.0.0.0:{}",
                            discover_port
                        );
                        let mut buf = [0u8; 64];
                        while let Ok((n, peer)) = sock.recv_from(&mut buf).await {
                            if &buf[..n] == DISCOVER_PING {
                                let mut reply = DISCOVER_REPLY_PREFIX.to_vec();
                                reply.extend_from_slice(quic_port.to_string().as_bytes());
                                let _ = sock.send_to(&reply, peer).await;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Failed to bind LAN discovery responder on udp://0.0.0.0:{}: {}",
                            discover_port, e
                        );
                    }
                }
            });
        }

        let monitor_channels = Arc::new(monitor_channels);
        while let Some(incoming) = self.endpoint.accept().await {
            let monitors = monitors.clone();
            let monitor_channels = monitor_channels.clone();
            let input_tx = input_tx.clone();
            let idr_cache = latest_idrs.clone();
            let stream_controls = stream_controls.clone();
            let active_clients = active_clients.clone();
            let audio_router = audio_router.clone();

            tokio::spawn(async move {
                match incoming.await {
                    Ok(conn) => {
                        info!("Quest client connected from {}", conn.remote_address());
                        if let Err(e) = handle_connection(
                            conn,
                            monitors,
                            monitor_channels,
                            idr_cache,
                            stream_controls,
                            active_clients,
                            audio_router,
                            input_tx,
                        )
                        .await
                        {
                            warn!("Client session ended: {:#}", e);
                        }
                    }
                    Err(e) => warn!("QUIC handshake failed: {}", e),
                }
            });
        }
        Ok(())
    }
}

async fn send_video_frame_on_stream(stream: &mut SendStream, frame: &EncodedFrame) -> Result<()> {
    let header = VideoFrameHeader {
        stream_id: frame.stream_id,
        frame_index: frame.frame_index,
        capture_timestamp_us: frame.capture_timestamp_us,
        is_idr: frame.is_idr,
        payload_len: frame.payload.len() as u32,
    };
    let hdr_bytes = postcard::to_allocvec(&header)?;
    let mut hdr_packet = Vec::with_capacity(4 + hdr_bytes.len());
    hdr_packet.extend_from_slice(&(hdr_bytes.len() as u32).to_le_bytes());
    hdr_packet.extend_from_slice(&hdr_bytes);
    stream
        .write_all_chunks(&mut [Bytes::from(hdr_packet), frame.payload.clone()])
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    conn: quinn::Connection,
    mut monitors: Vec<MonitorDescriptor>,
    monitor_channels: Arc<HashMap<u32, broadcast::Sender<Arc<EncodedFrame>>>>,
    latest_idrs: Arc<RwLock<HashMap<u32, Arc<EncodedFrame>>>>,
    stream_controls: Arc<HashMap<u32, Arc<StreamControl>>>,
    active_clients: Arc<AtomicUsize>,
    audio_router: Arc<crate::audio::AudioRouter>,
    input_tx: mpsc::Sender<InputEvent>,
) -> Result<()> {
    let _client_guard = ClientCountGuard::enter(active_clients, stream_controls.clone());

    // 1. Accept initial bidirectional control stream
    let (mut ctrl_send, mut ctrl_recv) = conn
        .accept_bi()
        .await
        .context("Failed to accept control stream")?;
    let _ = ctrl_send.set_priority(10);

    let hello_bytes = read_length_prefixed(&mut ctrl_recv).await?;
    let hello: ControlMessage = postcard::from_bytes(&hello_bytes)?;
    info!("Received {:?}", hello);

    // Snapshot latest IDR per stream and update monitor descriptors with negotiated physical size
    let cached_idrs: HashMap<u32, Arc<EncodedFrame>> = {
        let guard = latest_idrs.read().await;
        for mon in &mut monitors {
            if let Some(idr) = guard.get(&mon.stream_id) {
                if idr.width > 0 && idr.height > 0 {
                    mon.width = idr.width;
                    mon.height = idr.height;
                }
            }
        }
        guard.clone()
    };

    let manifest = ControlMessage::ServerManifest {
        monitors: monitors.clone(),
        audio_sample_rate: 48_000,
        audio_channels: 2,
    };
    write_length_prefixed(&mut ctrl_send, &postcard::to_allocvec(&manifest)?).await?;
    info!("Sent ServerManifest to Quest client");

    let metrics = Arc::new(ClientMetrics::new());

    // Spawn independent persistent uni-stream sender task per monitor so one monitor never head-of-line blocks another.
    let mut sender_handles = Vec::with_capacity(monitors.len());
    for mon in &monitors {
        let stream_id = mon.stream_id;
        let Some(frame_tx) = monitor_channels.get(&stream_id) else {
            continue;
        };
        let Some(control) = stream_controls.get(&stream_id).cloned() else {
            continue;
        };
        let mut frame_rx = frame_tx.subscribe();
        let cached_idr = cached_idrs.get(&stream_id).cloned();
        let conn_mon = conn.clone();
        let metrics_mon = metrics.clone();

        sender_handles.push(tokio::spawn(async move {
            let mut stream = match conn_mon.open_uni().await {
                Ok(s) => s,
                Err(e) => {
                    debug!("Failed to open uni stream for {}: {}", stream_id, e);
                    return;
                }
            };
            let _ = stream.set_priority(0);

            let mut last_sent_idx: Option<u64> = None;
            let mut last_idr_req = Instant::now();

            if let Some(idr) = cached_idr {
                if send_video_frame_on_stream(&mut stream, &idr).await.is_ok() {
                    last_sent_idx = Some(idr.frame_index);
                    metrics_mon.frames_sent.fetch_add(1, Ordering::Relaxed);
                } else {
                    return;
                }
            }
            control.force_idr.store(true, Ordering::Release);

            loop {
                match frame_rx.recv().await {
                    Ok(frame) => {
                        if !control.active.load(Ordering::Relaxed) {
                            last_sent_idx = None;
                            continue;
                        }
                        let in_sequence = frame.is_idr
                            || last_sent_idx
                                .is_some_and(|prev| frame.frame_index == prev.wrapping_add(1));
                        if !in_sequence {
                            if last_idr_req.elapsed() >= Duration::from_millis(500) {
                                control.force_idr.store(true, Ordering::Release);
                                last_idr_req = Instant::now();
                            }
                            continue;
                        }
                        let send_start = Instant::now();
                        if let Err(e) = send_video_frame_on_stream(&mut stream, &frame).await {
                            debug!("Video stream {} ended: {:#}", stream_id, e);
                            break;
                        }
                        last_sent_idx = Some(frame.frame_index);
                        metrics_mon.frames_sent.fetch_add(1, Ordering::Relaxed);
                        if send_start.elapsed() > Duration::from_millis(80) {
                            metrics_mon.slow_sends.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        metrics_mon.dropped_frames.fetch_add(n, Ordering::Relaxed);
                        last_sent_idx = None;
                        warn!(
                            "Dropped {} video frames for stream_id={}; requesting IDR",
                            n, stream_id
                        );
                        if last_idr_req.elapsed() >= Duration::from_millis(500) {
                            control.force_idr.store(true, Ordering::Release);
                            last_idr_req = Instant::now();
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }));
    }

    // Switch host audio to virtual sink and stream PCM chunks over QUIC datagrams
    let (mut audio_rx, _audio_guard) = audio_router.client_connected();
    let conn_audio = conn.clone();
    let audio_task = tokio::spawn(async move {
        let mut seq = 0u32;
        loop {
            match audio_rx.recv().await {
                Ok(samples) => {
                    let max_dgram = conn_audio.max_datagram_size().unwrap_or(1150);
                    let max_samples = ((max_dgram.saturating_sub(AUDIO_HEADER_LEN)) / 4) * 2;
                    let step = max_samples.max(2);
                    for chunk in samples.chunks(step) {
                        let pkt =
                            encode_audio_packet(seq, AUDIO_SAMPLE_RATE, AUDIO_CHANNELS, chunk);
                        seq = seq.wrapping_add(1);
                        if conn_audio.send_datagram(pkt.into()).is_err() {
                            return;
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // 2. Spawn datagram input receiver
    let conn_in = conn.clone();
    let input_tx_dgram = input_tx.clone();
    let input_task = tokio::spawn(async move {
        loop {
            match conn_in.read_datagram().await {
                Ok(bytes) => match postcard::from_bytes::<InputEvent>(&bytes) {
                    Ok(ev) => {
                        if input_tx_dgram.send(ev).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => debug!("Malformed input datagram: {}", e),
                },
                Err(e) => {
                    debug!("Input datagram loop ended: {}", e);
                    break;
                }
            }
        }
    });

    // 3. Spawn control & reliable-input receiver
    let controls_ctrl = stream_controls.clone();
    let input_tx_ctrl = input_tx;
    let ctrl_task = tokio::spawn(async move {
        let mut last_client_idr: HashMap<u32, Instant> = HashMap::new();
        while let Ok(buf) = read_length_prefixed(&mut ctrl_recv).await {
            match postcard::from_bytes::<ControlMessage>(&buf) {
                Ok(ControlMessage::RequestIdr { stream_id }) => {
                    let allow = last_client_idr
                        .get(&stream_id)
                        .is_none_or(|t| t.elapsed() >= Duration::from_millis(250));
                    if allow {
                        if let Some(ctrl) = controls_ctrl.get(&stream_id) {
                            ctrl.force_idr.store(true, Ordering::Release);
                            last_client_idr.insert(stream_id, Instant::now());
                            info!("Quest requested IDR for stream_id={}", stream_id);
                        }
                    }
                }
                Ok(ControlMessage::UpdateBitrate {
                    stream_id,
                    bitrate_kbps,
                }) => {
                    if let Some(ctrl) = controls_ctrl.get(&stream_id) {
                        let clamped = bitrate_kbps.clamp(ctrl.min_kbps(), ctrl.max_kbps());
                        ctrl.target_bitrate_kbps.store(clamped, Ordering::Release);
                        info!(
                            "Quest updated bitrate for stream_id={} -> {} kbps",
                            stream_id, clamped
                        );
                    }
                }
                Ok(ControlMessage::SetStreamActive { stream_id, active }) => {
                    if let Some(ctrl) = controls_ctrl.get(&stream_id) {
                        ctrl.active.store(active, Ordering::Release);
                        if active {
                            ctrl.force_idr.store(true, Ordering::Release);
                        }
                        info!("Stream {} active={}", stream_id, active);
                    }
                }
                Ok(ControlMessage::ReliableInput(ev)) => {
                    if input_tx_ctrl.send(ev).await.is_err() {
                        break;
                    }
                }
                Ok(msg) => debug!("Control message from Quest: {:?}", msg),
                Err(e) => warn!("Invalid ControlMessage: {}", e),
            }
        }
    });

    // 4. Real-time Adaptive Bitrate (ABR) loop sampling QUIC path + queue metrics every 500 ms
    let conn_abr = conn.clone();
    let controls_abr = stream_controls.clone();
    let metrics_abr = metrics.clone();
    let abr_task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tick.tick().await;

        let initial_path = conn_abr.stats().path;
        let mut prev_sent = initial_path.sent_packets;
        let mut prev_lost = initial_path.lost_packets;
        let mut prev_congestion = initial_path.congestion_events;
        let mut prev_drops = metrics_abr.dropped_frames.load(Ordering::Relaxed);
        let mut prev_slow = metrics_abr.slow_sends.load(Ordering::Relaxed);
        let mut prev_frames = metrics_abr.frames_sent.load(Ordering::Relaxed);
        let mut clean_windows: u32 = 0;

        loop {
            tick.tick().await;

            let path = conn_abr.stats().path;
            let cur_drops = metrics_abr.dropped_frames.load(Ordering::Relaxed);
            let cur_slow = metrics_abr.slow_sends.load(Ordering::Relaxed);
            let cur_frames = metrics_abr.frames_sent.load(Ordering::Relaxed);

            let delta_sent = path.sent_packets.saturating_sub(prev_sent);
            let delta_lost = path.lost_packets.saturating_sub(prev_lost);
            let delta_congestion = path.congestion_events.saturating_sub(prev_congestion);
            let delta_drops = cur_drops.saturating_sub(prev_drops);
            let delta_slow = cur_slow.saturating_sub(prev_slow);
            let delta_frames = cur_frames.saturating_sub(prev_frames);

            prev_sent = path.sent_packets;
            prev_lost = path.lost_packets;
            prev_congestion = path.congestion_events;
            prev_drops = cur_drops;
            prev_slow = cur_slow;
            prev_frames = cur_frames;

            let loss_pct = if delta_sent > 0 {
                (delta_lost as f64) * 100.0 / (delta_sent as f64)
            } else {
                0.0
            };
            let rtt_ms = path.rtt.as_secs_f64() * 1000.0;

            let congested = delta_drops > 0
                || delta_slow > 0
                || (delta_sent >= 20 && loss_pct > 2.0)
                || (delta_sent >= 20 && rtt_ms > 45.0)
                || delta_congestion >= 3;

            let healthy = delta_frames >= 10
                && delta_drops == 0
                && delta_slow == 0
                && loss_pct < 0.3
                && rtt_ms < 22.0
                && delta_congestion == 0;

            if congested {
                clean_windows = 0;
                for (&sid, ctrl) in controls_abr.iter() {
                    if !ctrl.active.load(Ordering::Relaxed) {
                        continue;
                    }
                    let cur = ctrl.target_bitrate_kbps.load(Ordering::Relaxed);
                    let min = ctrl.min_kbps();
                    let next = ((u64::from(cur) * 80) / 100).max(u64::from(min)) as u32;
                    if next != cur {
                        ctrl.target_bitrate_kbps.store(next, Ordering::Release);
                        info!(
                            "ABR down stream {}: {} -> {} kbps (rtt={:.1}ms loss={:.2}% drops={} slow={} cong={})",
                            sid, cur, next, rtt_ms, loss_pct, delta_drops, delta_slow, delta_congestion
                        );
                    }
                }
            } else if healthy {
                clean_windows = clean_windows.saturating_add(1);
                if clean_windows.is_multiple_of(4) {
                    for (&sid, ctrl) in controls_abr.iter() {
                        if !ctrl.active.load(Ordering::Relaxed) {
                            continue;
                        }
                        let cur = ctrl.target_bitrate_kbps.load(Ordering::Relaxed);
                        let max = ctrl.max_kbps();
                        let next = cur.saturating_add(3_000).min(max);
                        if next != cur {
                            ctrl.target_bitrate_kbps.store(next, Ordering::Release);
                            info!(
                                "ABR up stream {}: {} -> {} kbps (rtt={:.1}ms loss={:.2}%)",
                                sid, cur, next, rtt_ms, loss_pct
                            );
                        }
                    }
                }
            } else {
                clean_windows = 0;
            }
        }
    });

    conn.closed().await;
    info!("Quest client disconnected");

    abr_task.abort();
    for h in sender_handles {
        h.abort();
    }
    audio_task.abort();
    input_task.abort();
    ctrl_task.abort();
    Ok(())
}

async fn read_length_prefixed(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 4 * 1024 * 1024 {
        anyhow::bail!("Control message too large: {} bytes", len);
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_length_prefixed(send: &mut SendStream, payload: &[u8]) -> Result<()> {
    send.write_all(&(payload.len() as u32).to_le_bytes())
        .await?;
    send.write_all(payload).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use quinn::Endpoint as ClientEndpoint;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};
    use std::net::{IpAddr, Ipv4Addr};
    use vdesk_protocol::VideoCodec;

    #[derive(Debug)]
    struct TestSkipVerify;

    impl ServerCertVerifier for TestSkipVerify {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> std::result::Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &DigitallySignedStruct,
        ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![
                SignatureScheme::RSA_PKCS1_SHA256,
                SignatureScheme::ECDSA_NISTP256_SHA256,
                SignatureScheme::ED25519,
                SignatureScheme::RSA_PSS_SHA256,
            ]
        }
    }

    #[tokio::test]
    async fn quic_handshake_frame_and_input_loopback() {
        let server = QuicServer::bind(0).expect("bind ephemeral QUIC server");
        let port = server.local_addr().expect("local_addr").port();
        let server_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);

        let monitors = vec![MonitorDescriptor {
            stream_id: 93,
            name: "Monitor-1".into(),
            width: 2560,
            height: 1440,
            refresh_rate_mhz: 90_000,
            logical_x: 0,
            logical_y: 0,
            codec: VideoCodec::Av1YUV420,
        }];

        let (bcast_tx, _) = broadcast::channel::<Arc<EncodedFrame>>(8);
        let mut monitor_channels = HashMap::new();
        monitor_channels.insert(93u32, bcast_tx.clone());

        let (input_tx, mut input_rx) = mpsc::channel::<InputEvent>(8);
        let mut controls_map = HashMap::new();
        let req_ctrl = Arc::new(StreamControl::new(35_000, 8_750, 61_250));
        controls_map.insert(93u32, req_ctrl.clone());
        let stream_controls = Arc::new(controls_map);
        let active_clients = Arc::new(AtomicUsize::new(0));

        let audio_router = crate::audio::AudioRouter::new_disabled();
        let controls_srv = stream_controls.clone();
        let clients_srv = active_clients.clone();
        let server_task = tokio::spawn(async move {
            let _ = server
                .run(
                    monitors,
                    monitor_channels,
                    controls_srv,
                    clients_srv,
                    audio_router,
                    input_tx,
                )
                .await;
        });

        let mut tls = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(TestSkipVerify))
            .with_no_client_auth();
        tls.alpn_protocols = vec![ALPN_VDESK.to_vec()];

        let transport = build_low_latency_transport();

        let mut client_cfg = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
        ));
        client_cfg.transport_config(Arc::new(transport));

        let mut client_ep = ClientEndpoint::client(([127, 0, 0, 1], 0).into()).unwrap();
        client_ep.set_default_client_config(client_cfg);

        let conn = client_ep
            .connect(server_addr, "vdesk-host.local")
            .unwrap()
            .await
            .expect("QUIC connect");

        // 1. Handshake: ClientHello -> ServerManifest
        let (mut ctrl_send, mut ctrl_recv) = conn.open_bi().await.unwrap();
        let hello = ControlMessage::ClientHello {
            supported_codecs: vec![VideoCodec::Av1YUV420],
            max_refresh_hz: 90,
        };
        write_length_prefixed(&mut ctrl_send, &postcard::to_allocvec(&hello).unwrap())
            .await
            .unwrap();

        let manifest_bytes = read_length_prefixed(&mut ctrl_recv).await.unwrap();
        let manifest: ControlMessage = postcard::from_bytes(&manifest_bytes).unwrap();
        match manifest {
            ControlMessage::ServerManifest { monitors, .. } => {
                assert_eq!(monitors.len(), 1);
                assert_eq!(monitors[0].stream_id, 93);
                assert_eq!(monitors[0].width, 2560);
            }
            other => panic!("Expected ServerManifest, got {:?}", other),
        }

        // Wait briefly so the sender task has subscribed before we broadcast frames.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(active_clients.load(Ordering::Acquire), 1);

        // 2. Broadcast an encoded video frame from host -> client uni stream
        let expected_payload = vec![0xAAu8, 0xBB, 0xCC, 0xDD];
        bcast_tx
            .send(Arc::new(EncodedFrame {
                stream_id: 93,
                width: 2560,
                height: 1440,
                frame_index: 0,
                capture_timestamp_us: 12345,
                is_idr: true,
                payload: Bytes::from(expected_payload.clone()),
            }))
            .unwrap();

        let mut uni = conn.accept_uni().await.unwrap();

        let hdr_bytes = read_length_prefixed(&mut uni).await.unwrap();
        let hdr: VideoFrameHeader = postcard::from_bytes(&hdr_bytes).unwrap();
        assert_eq!(hdr.stream_id, 93);
        assert!(hdr.is_idr);
        assert_eq!(hdr.payload_len, 4);
        let mut rx_payload = vec![0u8; hdr.payload_len as usize];
        uni.read_exact(&mut rx_payload).await.unwrap();
        assert_eq!(rx_payload, expected_payload);

        // 2b. Second frame arrives in order on the same persistent uni stream
        let expected_payload2 = vec![0x11u8, 0x22];
        bcast_tx
            .send(Arc::new(EncodedFrame {
                stream_id: 93,
                width: 2560,
                height: 1440,
                frame_index: 1,
                capture_timestamp_us: 23456,
                is_idr: false,
                payload: Bytes::from(expected_payload2.clone()),
            }))
            .unwrap();

        let hdr2_bytes = read_length_prefixed(&mut uni).await.unwrap();
        let hdr2: VideoFrameHeader = postcard::from_bytes(&hdr2_bytes).unwrap();
        assert_eq!(hdr2.frame_index, 1);
        let mut rx_payload2 = vec![0u8; hdr2.payload_len as usize];
        uni.read_exact(&mut rx_payload2).await.unwrap();
        assert_eq!(rx_payload2, expected_payload2);

        // 2c. Out-of-order P-frame (gap 1 -> 5) is suppressed locally; next IDR (6) is sent
        bcast_tx
            .send(Arc::new(EncodedFrame {
                stream_id: 93,
                width: 2560,
                height: 1440,
                frame_index: 5,
                capture_timestamp_us: 30000,
                is_idr: false,
                payload: Bytes::from_static(&[0x99]),
            }))
            .unwrap();
        bcast_tx
            .send(Arc::new(EncodedFrame {
                stream_id: 93,
                width: 2560,
                height: 1440,
                frame_index: 6,
                capture_timestamp_us: 31000,
                is_idr: true,
                payload: Bytes::from_static(&[0x55, 0x66]),
            }))
            .unwrap();
        let hdr6_bytes = read_length_prefixed(&mut uni).await.unwrap();
        let hdr6: VideoFrameHeader = postcard::from_bytes(&hdr6_bytes).unwrap();
        assert_eq!(hdr6.frame_index, 6);
        assert!(hdr6.is_idr);
        let mut rx_payload6 = vec![0u8; hdr6.payload_len as usize];
        uni.read_exact(&mut rx_payload6).await.unwrap();
        assert_eq!(rx_payload6, vec![0x55, 0x66]);

        // 2d. RequestIdr updates host atomic flag, UpdateBitrate updates target, SetStreamActive toggles active
        req_ctrl.force_idr.store(false, Ordering::Release);
        write_length_prefixed(
            &mut ctrl_send,
            &postcard::to_allocvec(&ControlMessage::RequestIdr { stream_id: 93 }).unwrap(),
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(req_ctrl.force_idr.load(Ordering::Acquire));

        write_length_prefixed(
            &mut ctrl_send,
            &postcard::to_allocvec(&ControlMessage::UpdateBitrate {
                stream_id: 93,
                bitrate_kbps: 20_000,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(req_ctrl.target_bitrate_kbps.load(Ordering::Acquire), 20_000);

        write_length_prefixed(
            &mut ctrl_send,
            &postcard::to_allocvec(&ControlMessage::SetStreamActive {
                stream_id: 93,
                active: false,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!req_ctrl.active.load(Ordering::Acquire));
        req_ctrl.force_idr.store(false, Ordering::Release);
        bcast_tx
            .send(Arc::new(EncodedFrame {
                stream_id: 93,
                width: 2560,
                height: 1440,
                frame_index: 7,
                capture_timestamp_us: 32000,
                is_idr: true,
                payload: Bytes::from_static(&[0x77]),
            }))
            .unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(40),
                read_length_prefixed(&mut uni)
            )
            .await
            .is_err(),
            "Paused stream should not transmit frames"
        );
        write_length_prefixed(
            &mut ctrl_send,
            &postcard::to_allocvec(&ControlMessage::SetStreamActive {
                stream_id: 93,
                active: true,
            })
            .unwrap(),
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(req_ctrl.active.load(Ordering::Acquire));
        assert!(req_ctrl.force_idr.load(Ordering::Acquire));

        // 3. Send InputEvent datagram from client -> host
        let ev = InputEvent::PointerMotionAbsolute {
            stream_id: 93,
            u: 0.25,
            v: 0.75,
        };
        conn.send_datagram(postcard::to_allocvec(&ev).unwrap().into())
            .unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_secs(2), input_rx.recv())
            .await
            .expect("input timeout")
            .expect("input event");
        match received {
            InputEvent::PointerMotionAbsolute { stream_id, u, v } => {
                assert_eq!(stream_id, 93);
                assert!((u - 0.25).abs() < 1e-6);
                assert!((v - 0.75).abs() < 1e-6);
            }
            other => panic!("Unexpected input event: {:?}", other),
        }

        // 4. Send reliable KeyboardKey over control stream from client -> host
        write_length_prefixed(
            &mut ctrl_send,
            &postcard::to_allocvec(&ControlMessage::ReliableInput(InputEvent::KeyboardKey {
                evdev_keycode: 30,
                pressed: true,
            }))
            .unwrap(),
        )
        .await
        .unwrap();

        let rel_received = tokio::time::timeout(std::time::Duration::from_secs(2), input_rx.recv())
            .await
            .expect("reliable input timeout")
            .expect("reliable input event");
        match rel_received {
            InputEvent::KeyboardKey {
                evdev_keycode,
                pressed,
            } => {
                assert_eq!(evdev_keycode, 30);
                assert!(pressed);
            }
            other => panic!("Unexpected reliable input event: {:?}", other),
        }

        server_task.abort();
    }
}
