use std::collections::HashMap;
use std::sync::Arc;

use rtc::rtp;
use tokio::sync::{Mutex, mpsc};
use webrtc::peer_connection::RTCIceCandidateInit;

use crate::modules::protocol::{ControlMessage, ParticipantId, PeerId, SignalPayload};

use super::config::TransportConfig;
use super::peer::PeerLink;
use super::{TransportError, TransportEvent};

/// Sinal que precisa ser enviado ao peer remoto pelo servidor Rendezvous.
#[derive(Clone, Debug)]
pub struct OutboundSignal {
    pub to: PeerId,
    pub payload: SignalPayload,
}

/// Malha de conexões diretas entre todos os participantes.
pub struct MeshTransport {
    local_peer_id: PeerId,
    config: TransportConfig,
    event_tx: mpsc::UnboundedSender<TransportEvent>,
    links: Mutex<HashMap<PeerId, Arc<PeerLink>>>,
    outbound_tx: mpsc::UnboundedSender<OutboundSignal>,
    pending_candidates: Mutex<HashMap<PeerId, Vec<RTCIceCandidateInit>>>,
}

impl MeshTransport {
    pub fn new(
        local_peer_id: PeerId,
        config: TransportConfig,
    ) -> (
        Self,
        mpsc::UnboundedReceiver<TransportEvent>,
        mpsc::UnboundedReceiver<OutboundSignal>,
    ) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (outbound_tx, outbound_rx) = mpsc::unbounded_channel();

        (
            Self {
                local_peer_id,
                config,
                event_tx,
                links: Mutex::new(HashMap::new()),
                outbound_tx,
                pending_candidates: Mutex::new(HashMap::new()),
            },
            event_rx,
            outbound_rx,
        )
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.local_peer_id
    }

    pub async fn peer_ids(&self) -> Vec<PeerId> {
        self.links.lock().await.keys().copied().collect()
    }

    pub async fn has_link(&self, peer_id: PeerId) -> bool {
        self.links.lock().await.contains_key(&peer_id)
    }

    /// Garante que existe um `PeerLink` para o peer. O lado com menor
    /// `PeerId` inicia a oferta; o outro apenas espera e responde.
    pub async fn ensure_peer(
        &self,
        peer_id: PeerId,
        participant_id: ParticipantId,
    ) -> Result<(), TransportError> {
        if peer_id == self.local_peer_id {
            return Ok(());
        }

        let mut links = self.links.lock().await;
        if links.contains_key(&peer_id) {
            return Ok(());
        }

        let link = match PeerLink::new(
            self.local_peer_id,
            peer_id,
            participant_id,
            &self.config,
            self.event_tx.clone(),
        )
        .await
        {
            Ok(link) => Arc::new(link),
            Err(error) => {
                log::warn!("peer {peer_id}: falha ao criar PeerConnection: {error}");
                return Err(error);
            }
        };

        links.insert(peer_id, Arc::clone(&link));
        drop(links);

        let pending = self
            .pending_candidates
            .lock()
            .await
            .remove(&peer_id)
            .unwrap_or_default();
        for candidate in pending {
            if let Err(error) = link.add_ice_candidate(candidate).await {
                log::warn!("peer {peer_id}: candidato ICE pendente rejeitado: {error}");
            }
        }

        if link.is_offerer() {
            match link.create_offer().await {
                Ok((negotiation_id, sdp)) => {
                    let _ = self.outbound_tx.send(OutboundSignal {
                        to: peer_id,
                        payload: SignalPayload::Offer {
                            negotiation_id,
                            sdp,
                            participant_id,
                        },
                    });
                }
                Err(error) => {
                    log::warn!("peer {peer_id}: falha ao criar oferta: {error}");
                    self.remove_peer(peer_id).await;
                    return Err(error);
                }
            }
        }

        Ok(())
    }

    /// Trata SDP/ICE vindos do servidor Rendezvous. Outros tipos de payload são
    /// responsabilidade da sessão (admissão, roster e host).
    pub async fn handle_signal(
        &self,
        from: PeerId,
        payload: SignalPayload,
    ) -> Result<bool, TransportError> {
        match payload {
            SignalPayload::Offer {
                negotiation_id,
                sdp,
                participant_id,
            } => {
                self.ensure_peer(from, participant_id).await?;
                let link = self.link(from).await.ok_or(TransportError::Closed)?;
                if let Some(answer_sdp) = link.accept_offer(negotiation_id, sdp).await? {
                    let _ = self.outbound_tx.send(OutboundSignal {
                        to: from,
                        payload: SignalPayload::Answer {
                            negotiation_id,
                            sdp: answer_sdp,
                            participant_id: link.participant_id,
                        },
                    });
                }
                Ok(true)
            }
            SignalPayload::Answer {
                negotiation_id,
                sdp,
                participant_id,
            } => {
                self.ensure_peer(from, participant_id).await?;
                if let Some(link) = self.link(from).await {
                    link.accept_answer(negotiation_id, sdp).await?;
                }
                Ok(true)
            }
            SignalPayload::IceCandidate { candidate } => {
                if let Some(link) = self.link(from).await {
                    link.add_ice_candidate(candidate).await?;
                } else {
                    self.pending_candidates
                        .lock()
                        .await
                        .entry(from)
                        .or_default()
                        .push(candidate);
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn link(&self, peer_id: PeerId) -> Option<Arc<PeerLink>> {
        self.links.lock().await.get(&peer_id).cloned()
    }

    /// Divide um pacote Opus codificado entre todas as tracks ativas.
    pub async fn broadcast_audio(&self, opus: &[u8]) {
        let links: Vec<Arc<PeerLink>> = {
            let links = self.links.lock().await;
            links.values().cloned().collect()
        };

        for link in links {
            if let Err(error) = link.send_audio(opus).await {
                log::debug!("peer {}: falha ao enviar áudio: {error}", link.peer_id);
            }
        }
    }

    pub async fn send_control(
        &self,
        peer_id: PeerId,
        message: &ControlMessage,
    ) -> Result<bool, TransportError> {
        match self.link(peer_id).await {
            Some(link) => link.send_control(message).await,
            None => Ok(false),
        }
    }

    pub async fn broadcast_control(&self, message: &ControlMessage) -> usize {
        let links: Vec<Arc<PeerLink>> = {
            let links = self.links.lock().await;
            links.values().cloned().collect()
        };

        let mut sent = 0;
        for link in links {
            if link.send_control(message).await.unwrap_or(false) {
                sent += 1;
            }
        }
        sent
    }

    pub async fn connected_peers(&self) -> Vec<PeerId> {
        let links = self.links.lock().await;
        links
            .iter()
            .filter_map(|(peer_id, link)| link.is_connected().then_some(*peer_id))
            .collect()
    }

    pub async fn remove_peer(&self, peer_id: PeerId) {
        let link = self.links.lock().await.remove(&peer_id);
        self.pending_candidates.lock().await.remove(&peer_id);
        if let Some(link) = link {
            link.close().await;
        }
    }

    pub async fn shutdown(&self) {
        let links: Vec<Arc<PeerLink>> = {
            let mut links = self.links.lock().await;
            links.drain().map(|(_, link)| link).collect()
        };
        for link in links {
            link.close().await;
        }
        self.pending_candidates.lock().await.clear();
    }
}

pub type InboundRtp = rtp::Packet;
