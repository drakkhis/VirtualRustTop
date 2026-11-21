use crate::decoder::MonitorFrameQueue;
use anyhow::{Context, Result};
use quinn::{
    AckFrequencyConfig, ClientConfig, Connection, Endpoint, EndpointConfig, RecvStream, SendStream,
    TransportConfig, VarInt,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use vdesk_protocol::{
    ControlMessage, InputEvent, MonitorDescriptor, VideoCodec, VideoFrameHeader, ALPN_VDESK,
    DEFAULT_PORT, DISCOVER_PING, DISCOVER_PORT, DISCOVER_REPLY_PREFIX,
};

const UDP_SOCKET_BUF_BYTES: libc::c_int = 4 * 1024 * 1024;
const DSCP_AF41_TOS: libc::c_int = 0x88;

fn tune_udp_socket(sock: &std::net::UdpSocket) {
    let fd = sock.as_raw_fd();
    unsafe {
        let buf_sz = UDP_SOCKET_BUF_BYTES;
        let _ = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&buf_sz as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        let _ = libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
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

        let mut rcvbuf: libc::c_int = 0;
        let mut sndbuf: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let _ = libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&mut rcvbuf as *mut libc::c_int).cast(),
            &mut len,
        );
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let _ = libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&mut sndbuf as *mut libc::c_int).cast(),
            &mut len,
        );
        log::info!(
            "Quest UDP socket tuned: SO_RCVBUF={} KB, SO_SNDBUF={} KB",
            rcvbuf / 1024,
            sndbuf / 1024
        );
    }
}

fn build_low_latency_transport() -> TransportConfig {
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

pub fn parse_host_addr(raw: &str) -> Option<SocketAddr> {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))?;
    line.parse::<SocketAddr>().ok().or_else(|| {
        line.parse::<std::net::IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, DEFAULT_PORT))
    })
}

pub async fn discover_host(fallback: Option<SocketAddr>) -> Option<SocketAddr> {
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
    if let Ok(Ok((n, peer))) = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        sock.recv_from(&mut buf),
    )
    .await
    {
        if let Some(port_bytes) = buf[..n].strip_prefix(DISCOVER_REPLY_PREFIX) {
            if let Ok(port_str) = std::str::from_utf8(port_bytes) {
                if let Ok(port) = port_str.trim().parse::<u16>() {
                    return Some(SocketAddr::new(peer.ip(), port));
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
    ctrl_tx: mpsc::UnboundedSender<ControlMessage>,
    rt_handle: tokio::runtime::Handle,
    pub monitors: Vec<MonitorDescriptor>,
}

impl QuicClient {
    pub async fn connect(server_addr: SocketAddr) -> Result<Self> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut tls_config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
            .with_no_client_auth();
        tls_config.alpn_protocols = vec![ALPN_VDESK.to_vec()];

        let transport = build_low_latency_transport();
        let mut client_config = ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls_config)?,
        ));
        client_config.transport_config(Arc::new(transport));

        let bind_addr: SocketAddr = if server_addr.is_ipv6() {
            ([0u16; 8], 0).into()
        } else {
            ([0, 0, 0, 0], 0).into()
        };
        let udp_socket = std::net::UdpSocket::bind(bind_addr)?;
        tune_udp_socket(&udp_socket);
        let runtime =
            quinn::default_runtime().ok_or_else(|| anyhow::anyhow!("No Tokio runtime found"))?;
        let mut endpoint = Endpoint::new(EndpointConfig::default(), None, udp_socket, runtime)?;
        endpoint.set_default_client_config(client_config);

        let conn = endpoint
            .connect(server_addr, "vdesk-host.local")?
            .await
            .context("Failed to connect to vdesk-host")?;

        let (mut ctrl_send, mut ctrl_recv) = conn.open_bi().await?;
        let _ = ctrl_send.set_priority(10);
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

        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel::<ControlMessage>();
        tokio::spawn(async move {
            while let Some(msg) = ctrl_rx.recv().await {
                let Ok(bytes) = postcard::to_allocvec(&msg) else {
                    continue;
                };
                if write_length_prefixed(&mut ctrl_send, &bytes).await.is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            conn,
            ctrl_tx,
            rt_handle: tokio::runtime::Handle::current(),
            monitors,
        })
    }

    pub fn start_video_streams(&self, queues: HashMap<u32, Arc<MonitorFrameQueue>>) {
        let conn_rx = self.conn.clone();
        let queues = Arc::new(queues);
        self.rt_handle.spawn(async move {
            while let Ok(mut uni) = conn_rx.accept_uni().await {
                let queues = Arc::clone(&queues);
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
                        if let Some(q) = queues.get(&header.stream_id) {
                            q.push(DecodedStreamPacket { header, payload });
                        }
                    }
                });
            }
        });
    }

    pub fn request_idr(&self, stream_id: u32) {
        let _ = self.ctrl_tx.send(ControlMessage::RequestIdr { stream_id });
    }

    pub fn set_stream_active(&self, stream_id: u32, active: bool) {
        let _ = self
            .ctrl_tx
            .send(ControlMessage::SetStreamActive { stream_id, active });
    }

    pub fn send_input(&self, event: &InputEvent) -> Result<()> {
        match event {
            InputEvent::KeyboardKey { .. } | InputEvent::PointerButton { .. } => {
                let _ = self
                    .ctrl_tx
                    .send(ControlMessage::ReliableInput(event.clone()));
            }
            InputEvent::PointerMotionAbsolute { .. } | InputEvent::PointerAxisScroll { .. } => {
                let bytes = postcard::to_allocvec(event)?;
                self.conn.send_datagram(bytes.into())?;
            }
        }
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
    send.write_all(&(payload.len() as u32).to_le_bytes())
        .await?;
    send.write_all(payload).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn parse_host_addr_handles_formats_and_comments() {
        assert_eq!(
            parse_host_addr("10.0.0.42:9445"),
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 42)),
                9445
            ))
        );
        assert_eq!(
            parse_host_addr("10.0.0.42"),
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 42)),
                DEFAULT_PORT
            ))
        );
        assert_eq!(
            parse_host_addr("[::1]:9443"),
            Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9443))
        );
        assert_eq!(
            parse_host_addr("\n  # comment line\n\n  172.16.0.5:9500  \n# trailing"),
            Some(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(172, 16, 0, 5)),
                9500
            ))
        );
        assert_eq!(parse_host_addr(""), None);
        assert_eq!(parse_host_addr("   \n# only comments\n"), None);
        assert_eq!(parse_host_addr("not-an-ip"), None);
    }
}
