//! Transporte WebRTC da malha P2P.
//!
//! Esta camada conhece `PeerConnection`, tracks, RTP e DataChannels. Ela não
//! importa `cpal`, `ring_buff`, `AudioEncoder` ou `AudioReceiver`: o acoplamento
//! com áudio fica em `call_session::audio_bridge`.

pub mod config;
pub mod control;
pub mod mesh;
pub mod peer;

use rtc::rtp;
use thiserror::Error;
use webrtc::peer_connection::RTCIceCandidateInit;
use webrtc::peer_connection::RTCPeerConnectionState;

use crate::modules::protocol::{ControlMessage, PeerId};

pub use config::{MAX_PARTICIPANTS, TransportConfig};
pub use control::{ChannelRole, channel_role};
pub use mesh::{MeshTransport, OutboundSignal};
pub use peer::PeerLink;

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("erro WebRTC: {0}")]
    WebRtc(#[from] webrtc::error::Error),

    #[error("erro de JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("conexão encerrada")]
    Closed,

    #[error("estado inválido: {0}")]
    InvalidState(String),
}

/// Eventos emitidos pelos handlers das `PeerConnection`.
#[derive(Debug)]
pub enum TransportEvent {
    LocalIceCandidate {
        peer_id: PeerId,
        candidate: RTCIceCandidateInit,
    },
    InboundRtp {
        peer_id: PeerId,
        packet: rtp::Packet,
    },
    ControlMessage {
        peer_id: PeerId,
        message: ControlMessage,
    },
    ControlChannelOpen {
        peer_id: PeerId,
    },
    ConnectionStateChanged {
        peer_id: PeerId,
        state: RTCPeerConnectionState,
    },
    RemoteTrackEnded {
        peer_id: PeerId,
    },
}
