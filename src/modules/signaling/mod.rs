//! Sinalização inicial via servidor Rendezvous.
//!
//! O servidor apenas conhece salas, identidades e envelopes de SDP/ICE. Não
//! recebe áudio, não mantém histórico e não decide quem é o host.

pub mod client;
pub mod server;

use thiserror::Error;

pub use client::{SignalingClient, SignalingEvent, wait_registered};
pub use server::{RendezvousServer, serve};

#[derive(Debug, Error)]
pub enum SignalingError {
    #[error("erro de WebSocket: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),

    #[error("erro de JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("conexão de sinalização encerrada")]
    Closed,

    #[error("timeout aguardando o servidor de sinalização")]
    Timeout,

    #[error("erro de transporte: {0}")]
    Transport(String),
}
