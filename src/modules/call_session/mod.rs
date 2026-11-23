//! Sessão de chamada: liga sinalização, roster, malha WebRTC e áudio.
//!
//! O host coordena apenas admissão, capacidade, roster, heartbeat e eleição.
//! A mídia continua fluindo por conexões diretas mesmo quando o host muda.

pub mod audio_bridge;
pub mod host;
pub mod membership;

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior, interval};

use crate::modules::audio_dsp::jitter_buffer::{AudioReceiverHandle, make_audio_packet};
use crate::modules::protocol::{
    ControlMessage, ParticipantId, PeerId, RejectReason, RosterSnapshot, SignalPayload,
};
use crate::modules::signaling::{SignalingClient, SignalingError, SignalingEvent, wait_registered};
use crate::modules::webrtc_transport::{
    MeshTransport, OutboundSignal, TransportConfig, TransportError, TransportEvent,
};

pub use audio_bridge::AudioBridge;
pub use host::{Liveness, elect_oldest};
pub use membership::{Admission, Membership, RosterDiff};

const SWEEP_INTERVAL: Duration = Duration::from_millis(250);
const JOIN_RETRY_INTERVAL: Duration = Duration::from_secs(1);
const CLAIM_RETRY_INTERVAL: Duration = Duration::from_secs(2);
const SHUTDOWN_FLUSH: Duration = Duration::from_millis(150);
const MAX_PENDING_SIGNALS: usize = 64;

/// Configuração de uma sessão de chamada.
#[derive(Clone, Debug)]
pub struct SessionConfig {
    pub rendezvous_url: String,
    pub room: String,
    pub display_name: String,
    pub enable_audio: bool,
    pub transport: TransportConfig,
    pub signaling_timeout: Duration,
    pub join_timeout: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            rendezvous_url: "ws://127.0.0.1:9000".to_owned(),
            room: "geral".to_owned(),
            display_name: "Hydrargyum".to_owned(),
            enable_audio: true,
            transport: TransportConfig::default(),
            signaling_timeout: Duration::from_secs(5),
            join_timeout: Duration::from_secs(20),
        }
    }
}

/// Visão observável da sessão, útil para UI e testes de integração.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionStatus {
    pub peer_id: PeerId,
    pub participant_id: Option<ParticipantId>,
    pub roster: Option<RosterSnapshot>,
    pub host_id: Option<PeerId>,
    pub connected_peers: Vec<PeerId>,
    pub admitted: bool,
}

/// Handle para solicitar encerramento gracioso de fora do loop da sessão.
#[derive(Clone)]
pub struct ShutdownHandle {
    tx: watch::Sender<bool>,
}

impl ShutdownHandle {
    pub fn request(&self) {
        let _ = self.tx.send(true);
    }
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("sinalização: {0}")]
    Signaling(#[from] SignalingError),

    #[error("transporte: {0}")]
    Transport(#[from] TransportError),

    #[error("áudio: {0}")]
    Audio(#[from] audio_bridge::AudioBridgeError),

    #[error("entrada rejeitada: {0}")]
    JoinRejected(RejectReason),

    #[error("timeout aguardando admissão do host")]
    JoinTimeout,

    #[error("conexão de sinalização encerrada: {0}")]
    SignalingClosed(String),

    #[error("roster inválido: {0}")]
    InvalidRoster(String),

    #[error("o peer local foi removido da sala")]
    RemovedFromRoom,
}

#[derive(Clone, Debug)]
enum JoinState {
    Waiting {
        deadline: Instant,
        next_retry: Instant,
    },
    Admitted,
    Rejected(RejectReason),
}

/// Sessão completa de uma chamada P2P.
pub struct CallSession {
    config: SessionConfig,
    peer_id: PeerId,
    signaling: SignalingClient,
    signaling_rx: mpsc::UnboundedReceiver<SignalingEvent>,
    mesh: MeshTransport,
    transport_rx: mpsc::UnboundedReceiver<TransportEvent>,
    outbound_rx: mpsc::UnboundedReceiver<OutboundSignal>,
    membership: Membership,
    liveness: Liveness,
    audio: audio_bridge::AudioBridge,
    audio_rx: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    audio_handle: Option<AudioReceiverHandle>,
    join_state: JoinState,
    shutdown_rx: watch::Receiver<bool>,
    shutdown_requested: bool,
    status_tx: watch::Sender<SessionStatus>,
    last_claim_attempt: Option<(u64, Instant)>,
    pending_webrtc_signals: Vec<(PeerId, SignalPayload)>,
}

impl CallSession {
    /// Conecta ao Rendezvous, registra a sala e inicia a sessão.
    ///
    /// Se a sala estiver vazia, este peer se torna host imediatamente. Caso
    /// contrário, envia `JoinRequest` e aguarda a decisão do host no loop de
    /// eventos.
    pub async fn connect(config: SessionConfig) -> Result<(Self, ShutdownHandle), SessionError> {
        let peer_id = PeerId::new();
        let (signaling, mut signaling_rx) = SignalingClient::connect(
            &config.rendezvous_url,
            &config.room,
            peer_id,
            &config.display_name,
        )
        .await?;

        let existing = wait_registered(&mut signaling_rx, config.signaling_timeout).await?;

        let (audio, audio_rx) = if config.enable_audio {
            let (audio, rx) = audio_bridge::AudioBridge::start()?;
            (audio, Some(rx))
        } else {
            (audio_bridge::AudioBridge::disabled(), None)
        };
        let audio_handle = audio.receiver_handle();

        let (mesh, transport_rx, outbound_rx) =
            MeshTransport::new(peer_id, config.transport.clone());

        let now = Instant::now();
        let (membership, join_state) = if existing.is_empty() {
            (
                Membership::bootstrap(peer_id, config.transport.max_participants),
                JoinState::Admitted,
            )
        } else {
            signaling.broadcast(SignalPayload::JoinRequest {
                peer_id,
                display_name: config.display_name.clone(),
            })?;
            (
                Membership::new_unjoined(peer_id, config.transport.max_participants),
                JoinState::Waiting {
                    deadline: now + config.join_timeout,
                    next_retry: now + JOIN_RETRY_INTERVAL,
                },
            )
        };

        let mut liveness = Liveness::new(config.transport.peer_timeout);
        if let Some(roster) = membership.snapshot() {
            liveness.seed_members(roster, now);
        }

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let status = SessionStatus {
            peer_id,
            participant_id: membership.participant_id(),
            roster: membership.snapshot().cloned(),
            host_id: membership.host_id(),
            connected_peers: Vec::new(),
            admitted: membership.admitted(),
        };
        let (status_tx, _) = watch::channel(status);

        let session = Self {
            config,
            peer_id,
            signaling,
            signaling_rx,
            mesh,
            transport_rx,
            outbound_rx,
            membership,
            liveness,
            audio,
            audio_rx,
            audio_handle,
            join_state,
            shutdown_rx,
            shutdown_requested: false,
            status_tx,
            last_claim_attempt: None,
            pending_webrtc_signals: Vec::new(),
        };

        Ok((session, ShutdownHandle { tx: shutdown_tx }))
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    pub fn status_receiver(&self) -> watch::Receiver<SessionStatus> {
        self.status_tx.subscribe()
    }

    pub fn audio_enabled(&self) -> bool {
        self.audio.is_enabled()
    }

    pub async fn status(&self) -> SessionStatus {
        self.snapshot_status().await
    }

    /// Executa até shutdown externo, falha de sinalização ou remoção da sala.
    pub async fn run(mut self) -> Result<(), SessionError> {
        let result = self.event_loop().await;
        self.graceful_shutdown().await;

        if let JoinState::Rejected(reason) = self.join_state {
            return Err(SessionError::JoinRejected(reason));
        }

        result
    }

    async fn event_loop(&mut self) -> Result<(), SessionError> {
        let mut heartbeat = interval(self.config.transport.heartbeat_interval);
        heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut sweep = interval(SWEEP_INTERVAL);
        sweep.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut join_retry = interval(JOIN_RETRY_INTERVAL);
        join_retry.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                changed = self.shutdown_rx.changed(), if !self.shutdown_requested => {
                    match changed {
                        Ok(()) if *self.shutdown_rx.borrow() => {
                            self.shutdown_requested = true;
                            return Ok(());
                        }
                        // Sem handle não existe mais como pedir shutdown;
                        // desabilita o ramo para não girar em falso.
                        Err(_) => self.shutdown_requested = true,
                        Ok(()) => {}
                    }
                }
                event = self.signaling_rx.recv() => {
                    match event {
                        Some(event) => self.on_signaling(event).await?,
                        None => {
                            return Err(SessionError::SignalingClosed(
                                "canal de eventos encerrado".to_owned(),
                            ));
                        }
                    }
                }
                event = self.transport_rx.recv() => {
                    match event {
                        Some(event) => self.on_transport(event).await?,
                        None => return Ok(()),
                    }
                }
                outbound = self.outbound_rx.recv() => {
                    match outbound {
                        Some(outbound) => {
                            self.signaling.relay(Some(outbound.to), outbound.payload)?;
                        }
                        None => return Ok(()),
                    }
                }
                packet = recv_audio(&mut self.audio_rx) => {
                    match packet {
                        Some(packet) => self.mesh.broadcast_audio(&packet).await,
                        None => self.audio_rx = None,
                    }
                }
                _ = heartbeat.tick() => {
                    self.send_heartbeat().await;
                }
                _ = sweep.tick() => {
                    self.on_sweep().await?;
                }
                _ = join_retry.tick() => {
                    self.on_join_retry()?;
                }
            }
        }
    }

    async fn on_signaling(&mut self, event: SignalingEvent) -> Result<(), SessionError> {
        match event {
            SignalingEvent::Registered { .. } => {}
            SignalingEvent::PeerJoined {
                peer_id,
                display_name,
            } => {
                log::debug!("peer {peer_id} ({display_name}) entrou na sala de sinalização");
            }
            SignalingEvent::PeerLeft { peer_id } => {
                self.on_peer_left(peer_id).await?;
            }
            SignalingEvent::Relay { from, payload } => {
                // Barreira de admissão: nenhum SDP/ICE de peer fora do roster
                // pode criar uma PeerConnection. Isso impede que o nono peer,
                // rejeitado pelo host, contorne a capacidade da sala.
                let webrtc_signal = matches!(
                    payload,
                    SignalPayload::Offer { .. }
                        | SignalPayload::Answer { .. }
                        | SignalPayload::IceCandidate { .. }
                );
                if webrtc_signal && self.membership.member(from).is_none() {
                    // Antes do JoinAccepted o roster local ainda não é
                    // autoritativo. Bufferizamos SDP/ICE para não perder a
                    // oferta que chega junto com a admissão; o dreno filtra
                    // pelos membros confirmados em seguida.
                    if !self.membership.admitted()
                        && self.pending_webrtc_signals.len() < MAX_PENDING_SIGNALS
                    {
                        log::debug!("peer {from}: sinal WebRTC pré-admissão bufferizado");
                        self.pending_webrtc_signals.push((from, payload));
                        return Ok(());
                    }
                    log::warn!("peer {from}: sinalização WebRTC ignorada (fora do roster)");
                    return Ok(());
                }

                match self.mesh.handle_signal(from, payload.clone()).await {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(error) => {
                        // Uma negociação defeituosa com um peer não deve
                        // derrubar a chamada inteira; o próximo roster/sync
                        // pode recriar o link.
                        log::warn!("peer {from}: falha de negociação: {error}");
                        return Ok(());
                    }
                }
                self.on_membership_signal(from, payload).await?;
            }
            SignalingEvent::Closed { reason } => {
                return Err(SessionError::SignalingClosed(reason));
            }
        }
        Ok(())
    }

    async fn on_membership_signal(
        &mut self,
        from: PeerId,
        payload: SignalPayload,
    ) -> Result<(), SessionError> {
        match payload {
            SignalPayload::JoinRequest { peer_id, .. } => {
                if peer_id != from {
                    log::warn!("peer {from}: JoinRequest com peer_id divergente ({peer_id})");
                    return Ok(());
                }
                self.on_join_request(peer_id).await?;
            }
            SignalPayload::JoinAccepted {
                self_participant_id,
                roster,
            } => {
                self.on_join_accepted(self_participant_id, roster).await?;
            }
            SignalPayload::JoinRejected { reason } => {
                self.join_state = JoinState::Rejected(reason.clone());
                return Err(SessionError::JoinRejected(reason));
            }
            SignalPayload::RosterUpdate { roster } => {
                if !self.membership.admitted() {
                    // A resposta autoritativa é `JoinAccepted`. Uma atualização
                    // que chegue antes dela será substituída em seguida.
                    return Ok(());
                }
                if from != roster.host_id {
                    log::warn!(
                        "peer {from}: RosterUpdate ignorado; host anunciado é {}",
                        roster.host_id
                    );
                    return Ok(());
                }
                let diff = self.membership.apply_snapshot(roster);
                if diff.accepted {
                    self.apply_roster_diff(diff).await?;
                }
            }
            SignalPayload::HostClaim {
                host_id,
                epoch,
                roster,
            } => {
                if host_id != roster.host_id || epoch != roster.epoch || from != host_id {
                    log::warn!(
                        "peer {from}: HostClaim inconsistente (host={host_id}, epoch={epoch})"
                    );
                    return Ok(());
                }
                let diff = self.membership.apply_snapshot(roster);
                if diff.accepted {
                    log::info!("novo host: {host_id} (epoch {epoch}), anunciado por {from}");
                    self.liveness.mark_seen(host_id, Instant::now());
                    self.apply_roster_diff(diff).await?;
                }
            }
            SignalPayload::HostTransfer {
                host_id,
                epoch,
                roster,
            } => {
                // O host que sai difunde o mesmo HostTransfer pela sinalização
                // e pelo DataChannel. Depois que o primeiro é aplicado, o
                // segundo chega de um remetente que já não é o host atual;
                // nesse caso ele é apenas um duplicado idempotente.
                let already_applied =
                    self.membership.host_id() == Some(host_id) && self.membership.epoch() == epoch;
                let expected_sender = self.membership.host_id();
                if host_id != roster.host_id
                    || epoch != roster.epoch
                    || (!already_applied && from != host_id && expected_sender != Some(from))
                {
                    log::warn!(
                        "peer {from}: HostTransfer inconsistente (host={host_id}, epoch={epoch})"
                    );
                    return Ok(());
                }
                let diff = self.membership.apply_snapshot(roster);
                if diff.accepted {
                    log::info!("host transferido para {host_id} (epoch {epoch}) por {from}");
                    self.liveness.mark_seen(host_id, Instant::now());
                    self.apply_roster_diff(diff).await?;
                }
            }
            SignalPayload::Leave { peer_id } => {
                if peer_id != from || peer_id == self.peer_id {
                    log::warn!("peer {from}: Leave inválido");
                    return Ok(());
                }
                self.on_peer_left(peer_id).await?;
            }
            SignalPayload::Offer { .. }
            | SignalPayload::Answer { .. }
            | SignalPayload::IceCandidate { .. } => {}
        }
        Ok(())
    }

    async fn on_join_request(&mut self, peer_id: PeerId) -> Result<(), SessionError> {
        if !self.membership.admitted() || !self.membership.is_host() {
            return Ok(());
        }

        match self.membership.admit(peer_id) {
            Ok(Admission::Existing(member, roster)) => {
                self.signaling.relay(
                    Some(peer_id),
                    SignalPayload::JoinAccepted {
                        self_participant_id: member.participant_id,
                        roster,
                    },
                )?;
            }
            Ok(Admission::New(member, roster)) => {
                self.liveness.seed(peer_id, Instant::now());
                self.signaling.relay(
                    Some(peer_id),
                    SignalPayload::JoinAccepted {
                        self_participant_id: member.participant_id,
                        roster: roster.clone(),
                    },
                )?;
                self.signaling.broadcast(SignalPayload::RosterUpdate {
                    roster: roster.clone(),
                })?;

                if let Err(error) = self.mesh.ensure_peer(peer_id, member.participant_id).await {
                    log::warn!("peer {peer_id}: falha ao iniciar conexão direta: {error}");
                }

                self.mesh
                    .broadcast_control(&ControlMessage::RosterSnapshot(roster))
                    .await;
                log::info!("peer {peer_id} admitido");
                self.publish_status().await;
            }
            Err(RejectReason::RoomFull) => {
                log::info!("peer {peer_id} rejeitado: sala cheia");
                self.signaling.relay(
                    Some(peer_id),
                    SignalPayload::JoinRejected {
                        reason: RejectReason::RoomFull,
                    },
                )?;
            }
            Err(reason) => {
                log::debug!("peer {peer_id} não admitido: {reason}");
            }
        }
        Ok(())
    }

    async fn on_join_accepted(
        &mut self,
        self_participant_id: ParticipantId,
        roster: RosterSnapshot,
    ) -> Result<(), SessionError> {
        if self.membership.admitted() {
            let diff = self.membership.apply_snapshot(roster);
            if diff.accepted {
                self.apply_roster_diff(diff).await?;
            }
            return Ok(());
        }

        let membership = Membership::from_join(
            self.peer_id,
            self_participant_id,
            roster,
            self.config.transport.max_participants,
        )
        .map_err(SessionError::InvalidRoster)?;

        self.membership = membership;
        self.join_state = JoinState::Admitted;
        if let Some(snapshot) = self.membership.snapshot() {
            self.liveness.seed_members(snapshot, Instant::now());
        }

        self.publish_status().await;
        self.drain_pending_webrtc_signals().await;
        self.sync_mesh().await;
        log::info!(
            "entrada aceita como participante {self_participant_id} na sala {}",
            self.config.room
        );
        Ok(())
    }

    async fn on_transport(&mut self, event: TransportEvent) -> Result<(), SessionError> {
        match event {
            TransportEvent::LocalIceCandidate { peer_id, candidate } => {
                self.signaling
                    .relay(Some(peer_id), SignalPayload::IceCandidate { candidate })?;
            }
            TransportEvent::InboundRtp { peer_id, packet } => {
                let Some(participant_id) = self.membership.participant_of(peer_id) else {
                    return Ok(());
                };
                let Some(handle) = &self.audio_handle else {
                    return Ok(());
                };

                let header = &packet.header;
                let audio_packet = make_audio_packet(
                    header.sequence_number,
                    header.timestamp,
                    header.ssrc,
                    header.payload_type,
                    header.marker,
                    packet.payload.to_vec(),
                );
                if !handle.push_packet(participant_id, audio_packet) {
                    log::debug!(
                        "participante {participant_id}: fila de áudio cheia; pacote descartado"
                    );
                }
            }
            TransportEvent::ControlMessage { peer_id, message } => {
                self.on_control(peer_id, message).await?;
            }
            TransportEvent::ControlChannelOpen { peer_id } => {
                self.liveness.mark_seen(peer_id, Instant::now());
                self.send_hello(peer_id).await;
                if self.membership.is_host() {
                    self.send_roster_snapshot(peer_id).await;
                }
            }
            TransportEvent::ConnectionStateChanged { peer_id, state } => {
                log::debug!("peer {peer_id}: estado da conexão = {state:?}");
                if state == webrtc::peer_connection::RTCPeerConnectionState::Connected {
                    self.liveness.mark_seen(peer_id, Instant::now());
                }
                self.publish_status().await;
            }
            TransportEvent::RemoteTrackEnded { peer_id } => {
                log::debug!("peer {peer_id}: track remota encerrada");
            }
        }
        Ok(())
    }

    async fn on_control(
        &mut self,
        sender: PeerId,
        message: ControlMessage,
    ) -> Result<(), SessionError> {
        match message {
            ControlMessage::Heartbeat {
                peer_id,
                host_id,
                epoch,
                revision,
                ..
            } => {
                if peer_id != sender {
                    log::warn!("peer {sender}: heartbeat com identidade divergente");
                    return Ok(());
                }
                self.liveness.mark_seen(sender, Instant::now());
                self.reconcile_host_view(sender, host_id, epoch, revision)
                    .await;
            }
            ControlMessage::Hello {
                peer_id,
                host_id,
                epoch,
                revision,
                ..
            } => {
                if peer_id != sender {
                    log::warn!("peer {sender}: hello com identidade divergente");
                    return Ok(());
                }
                self.liveness.mark_seen(sender, Instant::now());
                self.reconcile_host_view(sender, host_id, epoch, revision)
                    .await;
                if self.membership.is_host()
                    && (epoch < self.membership.epoch() || revision < self.membership.revision())
                {
                    self.send_roster_snapshot(sender).await;
                }
            }
            ControlMessage::RosterSnapshot(roster) => {
                if self.membership.host_id() != Some(sender) {
                    log::warn!(
                        "peer {sender}: RosterSnapshot ignorado; host atual é {:?}",
                        self.membership.host_id()
                    );
                    return Ok(());
                }
                let diff = self.membership.apply_snapshot(roster);
                if diff.accepted {
                    self.apply_roster_diff(diff).await?;
                }
            }
            ControlMessage::SyncRequest { .. } => {
                if self.membership.is_host() {
                    self.send_roster_snapshot(sender).await;
                }
            }
            ControlMessage::HostClaim {
                host_id,
                epoch,
                roster,
            } => {
                if host_id != roster.host_id || epoch != roster.epoch || sender != host_id {
                    log::warn!("peer {sender}: HostClaim inconsistente");
                    return Ok(());
                }
                let diff = self.membership.apply_snapshot(roster);
                if diff.accepted {
                    log::info!("novo host: {host_id} (epoch {epoch}), via DataChannel");
                    self.liveness.mark_seen(host_id, Instant::now());
                    self.apply_roster_diff(diff).await?;
                }
            }
            ControlMessage::HostTransfer {
                host_id,
                epoch,
                roster,
            } => {
                // Duplicado do mesmo anúncio feito pela sinalização: o host
                // antigo já transferiu e não é mais o host local.
                let already_applied =
                    self.membership.host_id() == Some(host_id) && self.membership.epoch() == epoch;
                if host_id != roster.host_id
                    || epoch != roster.epoch
                    || (!already_applied
                        && sender != host_id
                        && self.membership.host_id() != Some(sender))
                {
                    log::warn!("peer {sender}: HostTransfer inconsistente");
                    return Ok(());
                }
                let diff = self.membership.apply_snapshot(roster);
                if diff.accepted {
                    log::info!("host transferido para {host_id} (epoch {epoch}) via DataChannel");
                    self.liveness.mark_seen(host_id, Instant::now());
                    self.apply_roster_diff(diff).await?;
                }
            }
            ControlMessage::Leave { peer_id } => {
                if peer_id != sender || peer_id == self.peer_id {
                    log::warn!("peer {sender}: Leave inválido");
                    return Ok(());
                }
                self.on_peer_left(peer_id).await?;
            }
        }
        Ok(())
    }

    async fn reconcile_host_view(
        &mut self,
        sender: PeerId,
        host_id: PeerId,
        epoch: u64,
        revision: u64,
    ) {
        if !self.membership.admitted() {
            return;
        }

        if self.membership.adopt_host_epoch(epoch, host_id) {
            self.last_claim_attempt = None;
            log::info!("peer {sender}: adotado host {host_id} no epoch {epoch}");
            self.publish_status().await;
            if host_id != self.peer_id {
                self.request_sync(host_id).await;
            }
            return;
        }

        if host_id == self.membership.host_id().unwrap_or(self.peer_id)
            && revision > self.membership.revision()
            && !self.membership.is_host()
        {
            self.request_sync(host_id).await;
        }
    }

    async fn on_peer_left(&mut self, peer_id: PeerId) -> Result<(), SessionError> {
        self.liveness.forget(peer_id);
        let was_host = self.membership.host_id() == Some(peer_id);

        let Some(member) = self.membership.member(peer_id).cloned() else {
            self.mesh.remove_peer(peer_id).await;
            if was_host {
                self.begin_failover().await?;
            }
            return Ok(());
        };

        if self.membership.is_host() {
            if let Some(roster) = self.membership.remove_as_host(peer_id) {
                self.audio_leave(member.participant_id);
                self.mesh.remove_peer(peer_id).await;
                self.signaling.broadcast(SignalPayload::RosterUpdate {
                    roster: roster.clone(),
                })?;
                self.mesh
                    .broadcast_control(&ControlMessage::RosterSnapshot(roster))
                    .await;
                self.publish_status().await;
            }
        } else {
            self.membership.forget_member(peer_id);
            self.audio_leave(member.participant_id);
            self.mesh.remove_peer(peer_id).await;
            self.publish_status().await;
            if was_host {
                self.begin_failover().await?;
            }
        }

        Ok(())
    }

    async fn begin_failover(&mut self) -> Result<(), SessionError> {
        if !self.membership.admitted() || self.membership.is_host() {
            return Ok(());
        }

        let Some(roster) = self.membership.snapshot().cloned() else {
            return Ok(());
        };
        let Some(host_id) = self.membership.host_id() else {
            return Ok(());
        };
        if host_id == self.peer_id {
            return Ok(());
        }

        let now = Instant::now();
        let host_removed = self.membership.member(host_id).is_none();
        if !host_removed && !self.liveness.timed_out(host_id, now) {
            return Ok(());
        }

        let alive = self
            .liveness
            .alive_members(&roster, now, self.peer_id, Some(host_id));
        let Some(winner) = elect_oldest(&roster, &alive, Some(host_id)) else {
            return Ok(());
        };

        if winner.peer_id != self.peer_id {
            self.request_sync(winner.peer_id).await;
            return Ok(());
        }

        let current_epoch = self.membership.epoch();
        if self.last_claim_attempt.is_some_and(|(epoch, at)| {
            epoch == current_epoch && at.elapsed() < CLAIM_RETRY_INTERVAL
        }) {
            return Ok(());
        }

        let old_members = self.membership.members().to_vec();
        let Some(snapshot) = self.membership.claim_host(&alive) else {
            return Ok(());
        };
        self.last_claim_attempt = Some((snapshot.epoch, Instant::now()));

        let remaining: HashSet<PeerId> = snapshot
            .members
            .iter()
            .map(|member| member.peer_id)
            .collect();
        for member in old_members {
            if !remaining.contains(&member.peer_id) {
                self.liveness.forget(member.peer_id);
                self.audio_leave(member.participant_id);
                self.mesh.remove_peer(member.peer_id).await;
            }
        }

        log::info!(
            "assumindo como host no epoch {} após falha do host {host_id}",
            snapshot.epoch
        );
        self.signaling.broadcast(SignalPayload::HostClaim {
            host_id: self.peer_id,
            epoch: snapshot.epoch,
            roster: snapshot.clone(),
        })?;
        self.mesh
            .broadcast_control(&ControlMessage::HostClaim {
                host_id: self.peer_id,
                epoch: snapshot.epoch,
                roster: snapshot,
            })
            .await;

        self.publish_status().await;
        self.sync_mesh().await;
        Ok(())
    }

    async fn on_sweep(&mut self) -> Result<(), SessionError> {
        let now = Instant::now();

        if let JoinState::Waiting { deadline, .. } = self.join_state
            && now >= deadline
        {
            return Err(SessionError::JoinTimeout);
        }

        if !self.membership.admitted() {
            return Ok(());
        }

        let Some(host_id) = self.membership.host_id() else {
            return Ok(());
        };

        if host_id == self.peer_id {
            let expired: Vec<_> = self
                .membership
                .members()
                .iter()
                .filter(|member| {
                    member.peer_id != self.peer_id && self.liveness.timed_out(member.peer_id, now)
                })
                .cloned()
                .collect();

            if expired.is_empty() {
                return Ok(());
            }

            let mut latest = None;
            for member in expired {
                log::info!(
                    "host: removendo participante {} por timeout de heartbeat",
                    member.peer_id
                );
                self.liveness.forget(member.peer_id);
                self.audio_leave(member.participant_id);
                self.mesh.remove_peer(member.peer_id).await;
                latest = self.membership.remove_as_host(member.peer_id);
            }

            if let Some(roster) = latest {
                self.signaling.broadcast(SignalPayload::RosterUpdate {
                    roster: roster.clone(),
                })?;
                self.mesh
                    .broadcast_control(&ControlMessage::RosterSnapshot(roster))
                    .await;
                self.publish_status().await;
            }
        } else if self.liveness.timed_out(host_id, now) {
            log::warn!(
                "host {host_id} não respondeu por {:?}; iniciando eleição",
                self.config.transport.peer_timeout
            );
            self.begin_failover().await?;
        }

        Ok(())
    }

    fn on_join_retry(&mut self) -> Result<(), SessionError> {
        let JoinState::Waiting {
            deadline,
            next_retry,
        } = &mut self.join_state
        else {
            return Ok(());
        };

        let now = Instant::now();
        if now >= *deadline {
            return Err(SessionError::JoinTimeout);
        }
        if now >= *next_retry {
            self.signaling.broadcast(SignalPayload::JoinRequest {
                peer_id: self.peer_id,
                display_name: self.config.display_name.clone(),
            })?;
            *next_retry = now + JOIN_RETRY_INTERVAL;
        }

        Ok(())
    }

    async fn send_heartbeat(&mut self) {
        if !self.membership.admitted() {
            return;
        }

        let Some(host_id) = self.membership.host_id() else {
            return;
        };
        let sent_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as u64);

        self.mesh
            .broadcast_control(&ControlMessage::Heartbeat {
                peer_id: self.peer_id,
                host_id,
                epoch: self.membership.epoch(),
                revision: self.membership.revision(),
                sent_at_ms,
            })
            .await;
    }

    async fn send_hello(&self, peer_id: PeerId) {
        let Some(participant_id) = self.membership.participant_id() else {
            return;
        };
        let Some(host_id) = self.membership.host_id() else {
            return;
        };
        let message = ControlMessage::Hello {
            peer_id: self.peer_id,
            participant_id,
            host_id,
            epoch: self.membership.epoch(),
            revision: self.membership.revision(),
        };
        if let Err(error) = self.mesh.send_control(peer_id, &message).await {
            log::debug!("peer {peer_id}: falha ao enviar Hello: {error}");
        }
    }

    async fn send_roster_snapshot(&self, peer_id: PeerId) {
        let Some(roster) = self.membership.snapshot().cloned() else {
            return;
        };
        if let Err(error) = self
            .mesh
            .send_control(peer_id, &ControlMessage::RosterSnapshot(roster))
            .await
        {
            log::debug!("peer {peer_id}: falha ao enviar roster: {error}");
        }
    }

    async fn request_sync(&self, peer_id: PeerId) {
        let message = ControlMessage::SyncRequest {
            epoch: self.membership.epoch(),
            revision: self.membership.revision(),
        };
        let _ = self.mesh.send_control(peer_id, &message).await;
    }

    async fn apply_roster_diff(&mut self, diff: RosterDiff) -> Result<(), SessionError> {
        for member in diff.removed {
            self.liveness.forget(member.peer_id);
            self.audio_leave(member.participant_id);
            self.mesh.remove_peer(member.peer_id).await;
        }

        for member in diff.added {
            self.liveness.seed(member.peer_id, Instant::now());
        }

        if diff.self_removed {
            self.publish_status().await;
            return Err(SessionError::RemovedFromRoom);
        }

        self.publish_status().await;
        self.sync_mesh().await;
        Ok(())
    }

    async fn drain_pending_webrtc_signals(&mut self) {
        let pending = std::mem::take(&mut self.pending_webrtc_signals);
        for (from, payload) in pending {
            if self.membership.member(from).is_none() {
                continue;
            }
            if let Err(error) = self.mesh.handle_signal(from, payload).await {
                log::warn!("peer {from}: sinal pendente rejeitado: {error}");
            }
        }
    }

    /// Garante que cada membro do roster tenha uma conexão direta e que não
    /// existam links para peers fora do roster.
    async fn sync_mesh(&mut self) {
        let members = self.membership.members().to_vec();
        for member in &members {
            if member.peer_id == self.peer_id {
                continue;
            }
            if let Err(error) = self
                .mesh
                .ensure_peer(member.peer_id, member.participant_id)
                .await
            {
                log::warn!(
                    "peer {}: falha ao sincronizar conexão: {error}",
                    member.peer_id
                );
            }
        }

        let desired: HashSet<PeerId> = members.iter().map(|member| member.peer_id).collect();
        for peer_id in self.mesh.peer_ids().await {
            if !desired.contains(&peer_id) {
                self.mesh.remove_peer(peer_id).await;
            }
        }
    }

    fn audio_leave(&self, participant_id: ParticipantId) {
        if let Some(handle) = &self.audio_handle {
            handle.leave(participant_id);
        }
    }

    async fn snapshot_status(&self) -> SessionStatus {
        SessionStatus {
            peer_id: self.peer_id,
            participant_id: self.membership.participant_id(),
            roster: self.membership.snapshot().cloned(),
            host_id: self.membership.host_id(),
            connected_peers: self.mesh.connected_peers().await,
            admitted: self.membership.admitted(),
        }
    }

    async fn publish_status(&self) {
        let status = self.snapshot_status().await;
        self.status_tx.send_replace(status);
    }

    async fn graceful_shutdown(&mut self) {
        if self.membership.admitted() {
            if self.membership.is_host() {
                let now = Instant::now();
                let Some(roster) = self.membership.snapshot().cloned() else {
                    return;
                };
                let alive =
                    self.liveness
                        .alive_members(&roster, now, self.peer_id, Some(self.peer_id));
                if let Some(transferred) = self.membership.transfer_away(&alive) {
                    log::info!(
                        "transferindo host para {} antes de sair",
                        transferred.host_id
                    );
                    let _ = self.signaling.broadcast(SignalPayload::HostTransfer {
                        host_id: transferred.host_id,
                        epoch: transferred.epoch,
                        roster: transferred.clone(),
                    });
                    let _ = self
                        .mesh
                        .broadcast_control(&ControlMessage::HostTransfer {
                            host_id: transferred.host_id,
                            epoch: transferred.epoch,
                            roster: transferred,
                        })
                        .await;
                }
            } else {
                let _ = self
                    .mesh
                    .broadcast_control(&ControlMessage::Leave {
                        peer_id: self.peer_id,
                    })
                    .await;
            }

            let _ = self.signaling.leave();
            tokio::time::sleep(SHUTDOWN_FLUSH).await;
        }

        self.mesh.shutdown().await;
        self.publish_status().await;
    }
}

async fn recv_audio(rx: &mut Option<mpsc::UnboundedReceiver<Vec<u8>>>) -> Option<Vec<u8>> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_padrao_do_host_e_maior_que_tres_segundos() {
        assert!(TransportConfig::default().peer_timeout > Duration::from_secs(3));
    }

    #[test]
    fn sessao_limita_a_oito_participantes() {
        let config = SessionConfig::default();
        assert_eq!(
            config.transport.max_participants,
            crate::modules::webrtc_transport::MAX_PARTICIPANTS
        );
        assert_eq!(crate::modules::webrtc_transport::MAX_PARTICIPANTS, 8);
    }
}
