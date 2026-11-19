use crate::capture::EncodedFrame;
use anyhow::{Context, Result};
use quinn::{Endpoint, RecvStream, SendStream, ServerConfig, TransportConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, info, warn};
use vdesk_protocol::{
    encode_audio_packet, ControlMessage, InputEvent, MonitorDescriptor, VideoFrameHeader,
    ALPN_VDESK, AUDIO_CHANNELS, AUDIO_HEADER_LEN, AUDIO_SAMPLE_RATE, DEFAULT_PORT, DISCOVER_PING,
    DISCOVER_PORT, DISCOVER_REPLY_PREFIX,
};

pub struct QuicServer {
    endpoint: Endpoint,
}

impl QuicServer {
    pub fn bind(port: u16) -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec![
            "vdesk-host.local".to_string(),
            "localhost".to_string(),
        ])
        .context("Failed to generate self-signed TLS certificate")?;

        let cert_der = CertificateDer::from(cert.cert);
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            cert.key_pair.serialize_der(),
        ));

        let mut tls_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der], key_der)
            .context("Failed to build rustls ServerConfig")?;
        tls_config.alpn_protocols = vec![ALPN_VDESK.to_vec()];

        let mut transport = TransportConfig::default();
        transport.max_idle_timeout(Some(
            std::time::Duration::from_secs(10)
                .try_into()
                .expect("valid idle timeout"),
        ));
        transport.datagram_receive_buffer_size(Some(1024 * 1024));
        transport.datagram_send_buffer_size(1024 * 1024);

        let mut server_config = ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(tls_config)?,
        ));
        server_config.transport_config(Arc::new(transport));

        let addr: SocketAddr = ([0, 0, 0, 0], port).into();
        let endpoint = Endpoint::server(server_config, addr)
            .with_context(|| format!("Failed to bind QUIC server on {}", addr))?;

        info!("QUIC server listening on udp://0.0.0.0:{}", port);
        Ok(Self { endpoint })
    }

    pub fn default_bind() -> Result<Self> {
        Self::bind(DEFAULT_PORT)
    }

    #[allow(dead_code)]
    pub fn local_addr(&self) -> Result<SocketAddr> {
        Ok(self.endpoint.local_addr()?)
    }

    pub async fn run(
        self,
        monitors: Vec<MonitorDescriptor>,
        frame_tx: broadcast::Sender<Arc<EncodedFrame>>,
        idr_flags: Arc<HashMap<u32, Arc<AtomicBool>>>,
        audio_router: Arc<crate::audio::AudioRouter>,
        input_tx: mpsc::Sender<InputEvent>,
    ) -> Result<()> {
        let latest_idrs: Arc<RwLock<HashMap<u32, Arc<EncodedFrame>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let mut cache_rx = frame_tx.subscribe();
        let cache_map = latest_idrs.clone();
        tokio::spawn(async move {
            loop {
                match cache_rx.recv().await {
                    Ok(frame) if frame.is_idr => {
                        cache_map.write().await.insert(frame.stream_id, frame);
                    }
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        let quic_port = self.endpoint.local_addr().map(|a| a.port()).unwrap_or(DEFAULT_PORT);
        tokio::spawn(async move {
            if let Ok(sock) = tokio::net::UdpSocket::bind(("0.0.0.0", DISCOVER_PORT)).await {
                let mut buf = [0u8; 64];
                while let Ok((n, peer)) = sock.recv_from(&mut buf).await {
                    if &buf[..n] == DISCOVER_PING {
                        let mut reply = DISCOVER_REPLY_PREFIX.to_vec();
                        reply.extend_from_slice(quic_port.to_string().as_bytes());
                        let _ = sock.send_to(&reply, peer).await;
                    }
                }
            }
        });

        while let Some(incoming) = self.endpoint.accept().await {
            let monitors = monitors.clone();
            let frame_rx = frame_tx.subscribe();
            let input_tx = input_tx.clone();
            let idr_cache = latest_idrs.clone();
            let idr_flags = idr_flags.clone();
            let audio_router = audio_router.clone();

            tokio::spawn(async move {
                match incoming.await {
                    Ok(conn) => {
                        info!("Quest client connected from {}", conn.remote_address());
                        if let Err(e) = handle_connection(
                            conn,
                            monitors,
                            frame_rx,
                            idr_cache,
                            idr_flags,
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

async fn send_video_frame_ordered(
    conn: &quinn::Connection,
    monitor_streams: &mut HashMap<u32, SendStream>,
    frame: &EncodedFrame,
) -> Result<()> {
    let header = VideoFrameHeader {
        stream_id: frame.stream_id,
        frame_index: frame.frame_index,
        capture_timestamp_us: frame.capture_timestamp_us,
        is_idr: frame.is_idr,
        payload_len: frame.payload.len() as u32,
    };
    let hdr_bytes = postcard::to_allocvec(&header)?;
    let stream = match monitor_streams.entry(frame.stream_id) {
        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => entry.insert(conn.open_uni().await?),
    };
    if let Err(e) = async {
        stream
            .write_all(&(hdr_bytes.len() as u32).to_le_bytes())
            .await?;
        stream.write_all(&hdr_bytes).await?;
        stream.write_all(&frame.payload).await?;
        Ok::<(), anyhow::Error>(())
    }
    .await
    {
        monitor_streams.remove(&frame.stream_id);
        return Err(e);
    }
    Ok(())
}

async fn handle_connection(
    conn: quinn::Connection,
    mut monitors: Vec<MonitorDescriptor>,
    mut frame_rx: broadcast::Receiver<Arc<EncodedFrame>>,
    latest_idrs: Arc<RwLock<HashMap<u32, Arc<EncodedFrame>>>>,
    idr_flags: Arc<HashMap<u32, Arc<AtomicBool>>>,
    audio_router: Arc<crate::audio::AudioRouter>,
    input_tx: mpsc::Sender<InputEvent>,
) -> Result<()> {
    // 1. Accept initial bidirectional control stream
    let (mut ctrl_send, mut ctrl_recv) = conn
        .accept_bi()
        .await
        .context("Failed to accept control stream")?;

    let hello_bytes = read_length_prefixed(&mut ctrl_recv).await?;
    let hello: ControlMessage = postcard::from_bytes(&hello_bytes)?;
    info!("Received {:?}", hello);

    // Snapshot latest IDR per stream and update monitor descriptors with negotiated physical size
    let cached_idrs: Vec<Arc<EncodedFrame>> = {
        let guard = latest_idrs.read().await;
        for mon in &mut monitors {
            if let Some(idr) = guard.get(&mon.stream_id) {
                if idr.width > 0 && idr.height > 0 {
                    mon.width = idr.width;
                    mon.height = idr.height;
                }
            }
        }
        guard.values().cloned().collect()
    };

    let manifest = ControlMessage::ServerManifest {
        monitors,
        audio_sample_rate: 48_000,
        audio_channels: 2,
    };
    write_length_prefixed(&mut ctrl_send, &postcard::to_allocvec(&manifest)?).await?;
    info!("Sent ServerManifest to Quest client");

    // Persist one ordered unidirectional stream per monitor to guarantee strict frame ordering.
    let mut monitor_streams: HashMap<u32, SendStream> = HashMap::new();

    // Immediately push the latest IDR keyframe per monitor so AMediaCodec starts decoding at once
    for idr in &cached_idrs {
        send_video_frame_ordered(&conn, &mut monitor_streams, idr).await?;
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
                        let pkt = encode_audio_packet(seq, AUDIO_SAMPLE_RATE, AUDIO_CHANNELS, chunk);
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
    let idr_flags_ctrl = idr_flags.clone();
    let input_tx_ctrl = input_tx;
    let ctrl_task = tokio::spawn(async move {
        while let Ok(buf) = read_length_prefixed(&mut ctrl_recv).await {
            match postcard::from_bytes::<ControlMessage>(&buf) {
                Ok(ControlMessage::RequestIdr { stream_id }) => {
                    if let Some(flag) = idr_flags_ctrl.get(&stream_id) {
                        flag.store(true, Ordering::Release);
                        info!("Quest requested IDR for stream_id={}", stream_id);
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

    // 4. Stream encoded video frames in strict FIFO order on per-monitor persistent QUIC streams
    loop {
        tokio::select! {
            _ = conn.closed() => {
                info!("Quest client disconnected");
                break;
            }
            res = frame_rx.recv() => {
                match res {
                    Ok(frame) => {
                        if let Err(e) = send_video_frame_ordered(&conn, &mut monitor_streams, &frame).await {
                            debug!("Video stream ended: {:#}", e);
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Dropped {} video frames for slow client; requesting IDRs", n);
                        for flag in idr_flags.values() {
                            flag.store(true, Ordering::Release);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
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
    send.write_all(&(payload.len() as u32).to_le_bytes()).await?;
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
        let (input_tx, mut input_rx) = mpsc::channel::<InputEvent>(8);
        let mut flags_map = HashMap::new();
        let req_idr = Arc::new(AtomicBool::new(false));
        flags_map.insert(93u32, req_idr.clone());
        let idr_flags = Arc::new(flags_map);

        let bcast_server = bcast_tx.clone();
        let audio_router = crate::audio::AudioRouter::new_disabled();
        let server_task = tokio::spawn(async move {
            let _ = server
                .run(monitors, bcast_server, idr_flags, audio_router, input_tx)
                .await;
        });

        let mut tls = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(TestSkipVerify))
            .with_no_client_auth();
        tls.alpn_protocols = vec![ALPN_VDESK.to_vec()];

        let mut transport = TransportConfig::default();
        transport.datagram_receive_buffer_size(Some(1024 * 1024));
        transport.datagram_send_buffer_size(1024 * 1024);

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
                payload: expected_payload.clone(),
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
                payload: expected_payload2.clone(),
            }))
            .unwrap();

        let hdr2_bytes = read_length_prefixed(&mut uni).await.unwrap();
        let hdr2: VideoFrameHeader = postcard::from_bytes(&hdr2_bytes).unwrap();
        assert_eq!(hdr2.frame_index, 1);
        let mut rx_payload2 = vec![0u8; hdr2.payload_len as usize];
        uni.read_exact(&mut rx_payload2).await.unwrap();
        assert_eq!(rx_payload2, expected_payload2);

        // 2c. RequestIdr updates host atomic flag
        write_length_prefixed(
            &mut ctrl_send,
            &postcard::to_allocvec(&ControlMessage::RequestIdr { stream_id: 93 }).unwrap(),
        )
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(req_idr.load(Ordering::Acquire));

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
