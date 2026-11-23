//! Tipos compartilhados entre sinalização, malha WebRTC e coordenação da sala.
//!
//! Este módulo é deliberadamente independente de áudio e de `cpal`. Ele define
//! apenas identidade, roster e mensagens de controle/sinalização.

use std::fmt;
use std::str::FromStr;

use rtc::peer_connection::transport::RTCIceCandidateInit;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Label do DataChannel de controle da chamada.
pub const CONTROL_CHANNEL_LABEL: &str = "hydrargyum.ctrl";

/// Label reservado para um futuro canal de chat. Nada é enviado nele no MVP.
pub const CHAT_CHANNEL_LABEL: &str = "hydrargyum.chat";

/// Identidade estável de um participante durante uma sessão.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PeerId(pub Uuid);

impl PeerId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn as_u128(self) -> u128 {
        self.0.as_u128()
    }
}

impl Default for PeerId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for PeerId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(value)?))
    }
}

/// Identificador numérico usado pelo módulo de áudio para separar os NetEqs.
pub type ParticipantId = u32;

/// Entrada imutável do roster. A ordem canônica é `(join_timestamp, peer_id)`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub peer_id: PeerId,
    pub participant_id: ParticipantId,
    pub join_timestamp: u64,
}

impl Member {
    pub fn order_key(&self) -> (u64, PeerId) {
        (self.join_timestamp, self.peer_id)
    }
}

/// Fotografia replicada do estado da sala.
///
/// `epoch` muda quando o host muda. `revision` muda a cada alteração de
/// participação dentro do mesmo host. Mensagens antigas podem ser descartadas
/// comparando `(epoch, revision)`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RosterSnapshot {
    pub epoch: u64,
    pub revision: u64,
    pub host_id: PeerId,
    pub next_join_timestamp: u64,
    pub next_participant_id: ParticipantId,
    pub members: Vec<Member>,
}

impl RosterSnapshot {
    pub fn version(&self) -> (u64, u64) {
        (self.epoch, self.revision)
    }

    pub fn member(&self, peer_id: PeerId) -> Option<&Member> {
        self.members.iter().find(|member| member.peer_id == peer_id)
    }

    pub fn contains(&self, peer_id: PeerId) -> bool {
        self.member(peer_id).is_some()
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Ordena os membros pela regra FIFO determinística.
    pub fn sorted_members(&self) -> Vec<Member> {
        let mut members = self.members.clone();
        members.sort_by_key(Member::order_key);
        members
    }
}

/// Motivo de rejeição de entrada na sala.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RejectReason {
    RoomFull,
    NotHost,
    StaleEpoch,
    DuplicatePeer,
    ShuttingDown,
}

impl fmt::Display for RejectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::RoomFull => "sala cheia",
            Self::NotHost => "peer não é o host",
            Self::StaleEpoch => "epoch desatualizado",
            Self::DuplicatePeer => "peer duplicado",
            Self::ShuttingDown => "sala sendo encerrada",
        };
        f.write_str(text)
    }
}

/// Mensagens trocadas pelo servidor Rendezvous durante o handshake.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SignalPayload {
    /// Pedido de entrada. É enviado em broadcast; apenas o host responde.
    JoinRequest {
        peer_id: PeerId,
        display_name: String,
    },
    JoinAccepted {
        self_participant_id: ParticipantId,
        roster: RosterSnapshot,
    },
    JoinRejected {
        reason: RejectReason,
    },
    Offer {
        negotiation_id: u64,
        sdp: String,
        participant_id: ParticipantId,
    },
    Answer {
        negotiation_id: u64,
        sdp: String,
        participant_id: ParticipantId,
    },
    IceCandidate {
        candidate: RTCIceCandidateInit,
    },
    /// Atualização de roster para peers que ainda não têm canal de controle.
    RosterUpdate {
        roster: RosterSnapshot,
    },
    Leave {
        peer_id: PeerId,
    },
    HostClaim {
        host_id: PeerId,
        epoch: u64,
        roster: RosterSnapshot,
    },
    HostTransfer {
        host_id: PeerId,
        epoch: u64,
        roster: RosterSnapshot,
    },
}

/// Mensagens do plano de controle, trafegadas nos DataChannels P2P.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ControlMessage {
    Hello {
        peer_id: PeerId,
        participant_id: ParticipantId,
        host_id: PeerId,
        epoch: u64,
        revision: u64,
    },
    Heartbeat {
        peer_id: PeerId,
        host_id: PeerId,
        epoch: u64,
        revision: u64,
        sent_at_ms: u64,
    },
    RosterSnapshot(RosterSnapshot),
    SyncRequest {
        epoch: u64,
        revision: u64,
    },
    HostClaim {
        host_id: PeerId,
        epoch: u64,
        roster: RosterSnapshot,
    },
    HostTransfer {
        host_id: PeerId,
        epoch: u64,
        roster: RosterSnapshot,
    },
    Leave {
        peer_id: PeerId,
    },
}

/// Envelopes do WebSocket do Rendezvous. O servidor roteia `payload` sem
/// inspecionar mídia (não existe mídia aqui) nem manter histórico.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ClientMessage {
    Register {
        room: String,
        peer_id: PeerId,
        display_name: String,
    },
    Relay {
        to: Option<PeerId>,
        payload: SignalPayload,
    },
    Leave,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerMessage {
    Registered {
        peers: Vec<PeerId>,
    },
    PeerJoined {
        peer_id: PeerId,
        display_name: String,
    },
    PeerLeft {
        peer_id: PeerId,
    },
    Relay {
        from: PeerId,
        payload: SignalPayload,
    },
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roster_orders_by_timestamp_then_peer_id() {
        let first = PeerId::new();
        let second = PeerId::new();
        let roster = RosterSnapshot {
            epoch: 1,
            revision: 1,
            host_id: first,
            next_join_timestamp: 2,
            next_participant_id: 3,
            members: vec![
                Member {
                    peer_id: second,
                    participant_id: 2,
                    join_timestamp: 1,
                },
                Member {
                    peer_id: first,
                    participant_id: 1,
                    join_timestamp: 0,
                },
            ],
        };

        let sorted = roster.sorted_members();
        assert_eq!(sorted[0].peer_id, first);
        assert_eq!(sorted[1].peer_id, second);
    }
}
