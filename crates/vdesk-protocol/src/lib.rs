use bitflags::bitflags;
use serde::{de::DeserializeOwned, Deserialize, Serialize};

pub const ALPN_VDESK: &[u8] = b"vdesk-xr/1";
pub const DEFAULT_PORT: u16 = 9443;
pub const DISCOVER_PORT: u16 = 9444;
pub const DISCOVER_PING: &[u8] = b"VDESK_PING";
pub const DISCOVER_REPLY_PREFIX: &[u8] = b"VDESK_HOST:";

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct FrameFlags: u8 {
        const IDR = 1 << 0;
        const INTRA_REFRESH = 1 << 1;
        const CURSOR_EMBEDDED = 1 << 2;
    }
}

pub fn encode_message<T: Serialize>(msg: &T) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_allocvec(msg)
}

pub fn decode_message<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, postcard::Error> {
    postcard::from_bytes(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoCodec {
    HevcYUV420,
    HevcYUV444, // Preferred for razor-sharp chroma text on desktop
    Av1YUV420,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorDescriptor {
    pub stream_id: u32, // Matches PipeWire node_id
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_rate_mhz: u32,
    pub logical_x: i32, // Global Wayland layout X offset
    pub logical_y: i32, // Global Wayland layout Y offset
    pub codec: VideoCodec,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMessage {
    /// Sent by Quest upon connection
    ClientHello {
        supported_codecs: Vec<VideoCodec>,
        max_refresh_hz: u32,
    },
    /// Sent by Host after PipeWire streams initialize
    ServerManifest {
        monitors: Vec<MonitorDescriptor>,
        audio_sample_rate: u32,
        audio_channels: u8,
    },
    /// Sent by Quest when packet loss is detected or decoder resets
    RequestIdr { stream_id: u32 },
    /// Dynamic bitrate or curvature adjustment
    UpdateBitrate {
        stream_id: u32,
        bitrate_kbps: u32,
    },
}

/// High-frequency Video Frame Header prepended to QUIC stream or fragmented datagrams
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoFrameHeader {
    pub stream_id: u32,
    pub frame_index: u64,
    pub capture_timestamp_us: u64,
    pub is_idr: bool,
    pub payload_len: u32,
}

/// Input events sent from Quest 3 controllers / Bluetooth keyboard back to Linux host
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InputEvent {
    /// UV coordinates (0.0..=1.0) hit-tested on a specific monitor cylinder in VR
    PointerMotionAbsolute {
        stream_id: u32,
        u: f32,
        v: f32,
    },
    /// Linux evdev button code (e.g., BTN_LEFT = 0x110, BTN_RIGHT = 0x111)
    PointerButton { button: u32, pressed: bool },
    /// Smooth 2D thumbstick scrolling
    PointerAxisScroll { dx: f32, dy: f32 },
    /// Linux evdev keycode (KEY_*)
    KeyboardKey { evdev_keycode: u32, pressed: bool },
}

pub const AUDIO_PACKET_MAGIC: [u8; 4] = *b"VDAU";
pub const AUDIO_HEADER_LEN: usize = 16;
pub const AUDIO_SAMPLE_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPacketHeader {
    pub seq: u32,
    pub sample_rate: u32,
    pub channels: u16,
}

pub fn encode_audio_packet(seq: u32, sample_rate: u32, channels: u16, pcm_i16: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(AUDIO_HEADER_LEN + pcm_i16.len() * 2);
    out.extend_from_slice(&AUDIO_PACKET_MAGIC);
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    for &sample in pcm_i16 {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

pub fn decode_audio_packet(bytes: &[u8]) -> Option<(AudioPacketHeader, Vec<i16>)> {
    if bytes.len() < AUDIO_HEADER_LEN {
        return None;
    }
    if bytes[0..4] != AUDIO_PACKET_MAGIC {
        return None;
    }
    let payload = &bytes[AUDIO_HEADER_LEN..];
    if payload.len() % 2 != 0 {
        return None;
    }
    let seq = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let sample_rate = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let channels = u16::from_le_bytes(bytes[12..14].try_into().ok()?);
    let mut samples = Vec::with_capacity(payload.len() / 2);
    for chunk in payload.chunks_exact(2) {
        samples.push(i16::from_le_bytes([chunk[0], chunk[1]]));
    }
    Some((
        AudioPacketHeader {
            seq,
            sample_rate,
            channels,
        },
        samples,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_packet_roundtrip() {
        let pcm = vec![0i16, 1234, -5678, i16::MAX, i16::MIN, 42];
        let pkt = encode_audio_packet(77, AUDIO_SAMPLE_RATE, AUDIO_CHANNELS, &pcm);
        assert_eq!(pkt.len(), AUDIO_HEADER_LEN + pcm.len() * 2);
        let (hdr, decoded) = decode_audio_packet(&pkt).expect("decode audio packet");
        assert_eq!(
            hdr,
            AudioPacketHeader {
                seq: 77,
                sample_rate: AUDIO_SAMPLE_RATE,
                channels: AUDIO_CHANNELS,
            }
        );
        assert_eq!(decoded, pcm);
        assert!(decode_audio_packet(b"BAD!123456789012").is_none());
        assert!(decode_audio_packet(&pkt[..pkt.len() - 1]).is_none());
    }
}
