use anyhow::{Context, Result};
use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream, TransportConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use vdesk_protocol::{
    ControlMessage, InputEvent, MonitorDescriptor, VideoCodec, VideoFrameHeader, ALPN_VDESK,
    DISCOVER_PING, DISCOVER_PORT, DISCOVER_REPLY_PREFIX,
};

pub async fn discover_host(fallback: SocketAddr) -> SocketAddr {
    let Ok(sock) = tokio::net::UdpSocket::bind(("0.0.0.0", 0)).await else {
        return fallback;
    };
    if sock.set_broadcast(true).is_err() {
        return fallback;
    }
    let _ = sock
        .send_to(DISCOVER_PING, ("255.255.255.255", DISCOVER_PORT))
        .await;
    let mut buf = [0u8; 64];
    if let Ok(Ok((n, peer))) =
        tokio::time::timeout(std::time::Duration::from_millis(500), sock.recv_from(&mut buf)).await
    {
        if let Some(port_bytes) = buf[..n].strip_prefix(DISCOVER_REPLY_PREFIX) {
            if let Ok(port_str) = std::str::from_utf8(port_bytes) {
                if let Ok(port) = port_str.trim().parse::<u16>() {
                    return SocketAddr::new(peer.ip(), port);
                }
            }
        }
    }
    fallback
}

#[derive(Debug)]
struct SkipServerVerification;

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
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

pub struct DecodedStreamPacket {
    pub header: VideoFrameHeader,
    pub payload: Vec<u8>,
}

pub struct QuicClient {
    conn: Connection,
    ctrl_send: Arc<tokio::sync::Mutex<SendStream>>,
    rt_handle: tokio::runtime::Handle,
    pub monitors: Vec<MonitorDescriptor>,
}

impl QuicClient {
    pub async fn connect(
        server_addr: SocketAddr,
        frame_tx: mpsc::Sender<DecodedStreamPacket>,
    ) -> Result<Self> {
        let rt_handle = tokio::runtime::Handle::current();
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut tls_config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
            .with_no_client_auth();
        tls_config.alpn_protocols = vec![ALPN_VDESK.to_vec()];

        let mut transport = TransportConfig::default();
        transport.max_idle_timeout(Some(
            std::time::Duration::from_secs(10)
                .try_into()
                .expect("valid idle timeout"),
        ));
        transport.datagram_receive_buffer_size(Some(1024 * 1024));
        transport.datagram_send_buffer_size(1024 * 1024);

        let mut client_config = ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)?,
        ));
        client_config.transport_config(Arc::new(transport));

        let mut endpoint = Endpoint::client(([0, 0, 0, 0], 0).into())?;
        endpoint.set_default_client_config(client_config);

        let conn = endpoint
            .connect(server_addr, "vdesk-host.local")?
            .await
            .context("Failed to connect to vdesk-host")?;

        let (mut ctrl_send, mut ctrl_recv) = conn.open_bi().await?;
        let hello = ControlMessage::ClientHello {
            supported_codecs: vec![VideoCodec::Av1YUV420, VideoCodec::HevcYUV420],
            max_refresh_hz: 120,
        };
        write_length_prefixed(&mut ctrl_send, &postcard::to_allocvec(&hello)?).await?;

        let manifest_bytes = read_length_prefixed(&mut ctrl_recv).await?;
        let manifest: ControlMessage = postcard::from_bytes(&manifest_bytes)?;
        let (monitors, audio_sample_rate, audio_channels) = match manifest {
            ControlMessage::ServerManifest {
                monitors,
                audio_sample_rate,
                audio_channels,
            } => (monitors, audio_sample_rate, audio_channels),
            other => anyhow::bail!("Expected ServerManifest, got {:?}", other),
        };

        let conn_audio = conn.clone();
        tokio::spawn(async move {
            let player = match crate::audio::QuestAudioPlayer::start(
                audio_sample_rate as i32,
                audio_channels as i32,
            ) {
                Ok(p) => p,
                Err(e) => {
                    log::warn!("AAudio playback disabled: {:#}", e);
                    return;
                }
            };
            while let Ok(bytes) = conn_audio.read_datagram().await {
                if let Some((_, samples)) = vdesk_protocol::decode_audio_packet(&bytes) {
                    player.push_pcm(&samples);
                }
            }
        });

        let conn_rx = conn.clone();
        tokio::spawn(async move {
            while let Ok(mut uni) = conn_rx.accept_uni().await {
                let tx = frame_tx.clone();
                tokio::spawn(async move {
                    loop {
                        let Ok(hdr_bytes) = read_length_prefixed(&mut uni).await else {
                            break;
                        };
                        let Ok(header) = postcard::from_bytes::<VideoFrameHeader>(&hdr_bytes)
                        else {
                            break;
                        };
                        if header.payload_len > 64 * 1024 * 1024 {
                            break;
                        }
                        let mut payload = vec![0u8; header.payload_len as usize];
                        if uni.read_exact(&mut payload).await.is_err() {
                            break;
                        }
                        if tx.send(DecodedStreamPacket { header, payload }).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });

        Ok(Self {
            conn,
            ctrl_send: Arc::new(tokio::sync::Mutex::new(ctrl_send)),
            rt_handle,
            monitors,
        })
    }

    pub fn request_idr(&self, stream_id: u32) {
        let ctrl_send = self.ctrl_send.clone();
        self.rt_handle.spawn(async move {
            if let Ok(bytes) = postcard::to_allocvec(&ControlMessage::RequestIdr { stream_id }) {
                let mut send = ctrl_send.lock().await;
                let _ = write_length_prefixed(&mut send, &bytes).await;
            }
        });
    }

    pub fn send_input(&self, event: &InputEvent) -> Result<()> {
        let bytes = postcard::to_allocvec(event)?;
        self.conn.send_datagram(bytes.into())?;
        Ok(())
    }
}

impl Drop for QuicClient {
    fn drop(&mut self) {
        self.conn.close(0u32.into(), b"disconnect");
    }
}

async fn read_length_prefixed(recv: &mut RecvStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_length_prefixed(send: &mut SendStream, payload: &[u8]) -> Result<()> {
    send.write_all(&(payload.len() as u32).to_le_bytes()).await?;
    send.write_all(payload).await?;
    Ok(())
}
