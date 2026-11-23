use std::time::Duration;

use rtc::peer_connection::configuration::media_engine::{MIME_TYPE_OPUS, MediaEngine};
use rtc::rtp_transceiver::rtp_sender::{RTCRtpCodec, RTCRtpCodecParameters, RtpCodecKind};
use webrtc::peer_connection::RTCIceServer;

use crate::modules::audio_types::OPUS_PAYLOAD_TYPE;

/// Limite rígido de participantes ativos por sala, incluindo o host.
pub const MAX_PARTICIPANTS: usize = 8;

/// Canal de áudio: 48 kHz, mono, Opus payload type 111.
pub const OPUS_CLOCK_RATE: u32 = 48_000;
pub const OPUS_CHANNELS: u16 = 1;

/// Configuração da malha e do handshake.
#[derive(Clone, Debug)]
pub struct TransportConfig {
    pub ice_servers: Vec<RTCIceServer>,
    pub udp_bind: String,
    pub heartbeat_interval: Duration,
    pub peer_timeout: Duration,
    pub max_participants: usize,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            ice_servers: vec![RTCIceServer {
                urls: vec!["stun:stun.cloudflare.com:3478".to_owned()],
                ..Default::default()
            }],
            udp_bind: "0.0.0.0:0".to_owned(),
            heartbeat_interval: Duration::from_secs(1),
            // A especificação exige timeout estritamente maior que 3000 ms.
            peer_timeout: Duration::from_millis(3_500),
            max_participants: MAX_PARTICIPANTS,
        }
    }
}

impl TransportConfig {
    /// Configuração sem STUN, adequada para testes em loopback.
    pub fn loopback() -> Self {
        Self {
            ice_servers: Vec::new(),
            ..Self::default()
        }
    }

    pub fn opus_parameters() -> RTCRtpCodecParameters {
        RTCRtpCodecParameters {
            rtp_codec: RTCRtpCodec {
                mime_type: MIME_TYPE_OPUS.to_owned(),
                clock_rate: OPUS_CLOCK_RATE,
                channels: OPUS_CHANNELS,
                sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                rtcp_feedback: Vec::new(),
            },
            payload_type: OPUS_PAYLOAD_TYPE,
        }
    }

    /// MediaEngine restrita a Opus mono no payload type usado pelo NetEq.
    pub fn media_engine() -> Result<MediaEngine, webrtc::error::Error> {
        let mut media_engine = MediaEngine::default();
        media_engine.register_codec(Self::opus_parameters(), RtpCodecKind::Audio)?;
        Ok(media_engine)
    }
}
