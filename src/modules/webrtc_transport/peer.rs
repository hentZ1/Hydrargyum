use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use rtc::media::Sample;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::RTCConfigurationBuilder;
use rtc::peer_connection::configuration::interceptor_registry::register_default_interceptors;
use rtc::peer_connection::sdp::RTCSessionDescription;
use rtc::rtp_transceiver::SSRC;
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
};
use tokio::sync::Mutex;
use webrtc::data_channel::{DataChannel, DataChannelEvent};
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceCandidateInit,
    RTCPeerConnectionIceEvent, RTCPeerConnectionState, Registry,
};

use crate::modules::audio_types::{FRAME_MS, OPUS_PAYLOAD_TYPE};
use crate::modules::protocol::{CONTROL_CHANNEL_LABEL, ControlMessage, ParticipantId, PeerId};

use super::config::TransportConfig;
use super::control::{ChannelRole, channel_role, decode_control, encode_control_mut};
use super::{TransportError, TransportEvent};

/// Estado compartilhado entre o `PeerLink` e o handler da `PeerConnection`.
struct SharedState {
    control: Mutex<Option<Arc<dyn DataChannel>>>,
    connected: AtomicBool,
    control_ready: AtomicBool,
}

/// Uma conexão WebRTC direta com outro participante.
pub struct PeerLink {
    pub peer_id: PeerId,
    pub participant_id: ParticipantId,
    pub pc: Arc<dyn PeerConnection>,
    local_track: Arc<TrackLocalStaticSample>,
    ssrc: SSRC,
    payload_type: u8,
    shared: Arc<SharedState>,
    polite: bool,
    has_local_offer: AtomicBool,
    remote_description_set: AtomicBool,
    negotiation_id: AtomicU64,
    negotiation_lock: Mutex<()>,
    pending_candidates: Mutex<Vec<RTCIceCandidateInit>>,
}

impl PeerLink {
    pub async fn new(
        local_peer_id: PeerId,
        peer_id: PeerId,
        participant_id: ParticipantId,
        config: &TransportConfig,
        event_tx: tokio::sync::mpsc::UnboundedSender<TransportEvent>,
    ) -> Result<Self, TransportError> {
        let shared = Arc::new(SharedState {
            control: Mutex::new(None),
            connected: AtomicBool::new(false),
            control_ready: AtomicBool::new(false),
        });

        let handler = Arc::new(TransportHandler {
            peer_id,
            event_tx: event_tx.clone(),
            shared: Arc::clone(&shared),
        });

        let mut media_engine = TransportConfig::media_engine()?;
        let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;

        let rtc_config = RTCConfigurationBuilder::new()
            .with_ice_servers(config.ice_servers.clone())
            .build();

        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_configuration(rtc_config)
                .with_media_engine(media_engine)
                .with_interceptor_registry(registry)
                .with_handler(handler)
                .with_udp_addrs(vec![config.udp_bind.clone()])
                .build()
                .await?,
        );

        let ssrc = (uuid::Uuid::new_v4().as_u128() as u32) ^ participant_id.rotate_left(16);
        let codec = TransportConfig::opus_parameters().rtp_codec;
        let media_track = MediaStreamTrack::new(
            format!("hydrargyum-{local_peer_id}"),
            format!("audio-{peer_id}"),
            "hydrargyum microphone".to_owned(),
            RtpCodecKind::Audio,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(ssrc),
                    ..Default::default()
                },
                codec,
                ..Default::default()
            }],
        );
        let local_track = Arc::new(TrackLocalStaticSample::new(Instant::now(), media_track)?);
        pc.add_track(Arc::clone(&local_track) as Arc<dyn TrackLocal>)
            .await?;

        // Apenas o lado com menor PeerId cria o canal de controle. Isso
        // elimina a criação duplicada do canal e complementa o Perfect
        // Negotiation, no qual o lado com maior PeerId é o "polite".
        let is_offerer = local_peer_id < peer_id;
        if is_offerer {
            let channel = pc.create_data_channel(CONTROL_CHANNEL_LABEL, None).await?;
            *shared.control.lock().await = Some(Arc::clone(&channel));
            spawn_data_channel_reader(peer_id, channel, Arc::clone(&shared), event_tx);
        }

        Ok(Self {
            peer_id,
            participant_id,
            pc,
            local_track,
            ssrc,
            payload_type: OPUS_PAYLOAD_TYPE,
            shared,
            polite: local_peer_id > peer_id,
            has_local_offer: AtomicBool::new(false),
            remote_description_set: AtomicBool::new(false),
            negotiation_id: AtomicU64::new(0),
            negotiation_lock: Mutex::new(()),
            pending_candidates: Mutex::new(Vec::new()),
        })
    }

    pub fn is_connected(&self) -> bool {
        self.shared.connected.load(Ordering::Acquire)
    }

    pub fn has_control_channel(&self) -> bool {
        self.shared.control_ready.load(Ordering::Acquire)
    }

    pub fn is_offerer(&self) -> bool {
        !self.polite
    }

    /// Cria a oferta inicial. Retorna `(negotiation_id, sdp)`.
    pub async fn create_offer(&self) -> Result<(u64, String), TransportError> {
        let _guard = self.negotiation_lock.lock().await;
        let offer = self.pc.create_offer(None).await?;
        self.pc.set_local_description(offer.clone()).await?;
        self.has_local_offer.store(true, Ordering::Release);
        let id = self.negotiation_id.fetch_add(1, Ordering::AcqRel) + 1;
        Ok((id, offer.sdp))
    }

    /// Aplica uma oferta remota e devolve a resposta.
    ///
    /// Se já existir uma oferta local, o lado "polite" faz rollback e aceita a
    /// remota; o lado "impolite" ignora a oferta concorrente.
    pub async fn accept_offer(
        &self,
        negotiation_id: u64,
        sdp: String,
    ) -> Result<Option<String>, TransportError> {
        let _guard = self.negotiation_lock.lock().await;

        if self.has_local_offer.load(Ordering::Acquire) {
            if !self.polite {
                log::debug!(
                    "peer {}: oferta concorrente ignorada (impolite)",
                    self.peer_id
                );
                return Ok(None);
            }

            let rollback = RTCSessionDescription::rollback(None)?;
            self.pc.set_local_description(rollback).await?;
            self.has_local_offer.store(false, Ordering::Release);
        }

        let offer = RTCSessionDescription::offer(sdp)?;
        self.pc.set_remote_description(offer).await?;
        self.remote_description_set.store(true, Ordering::Release);
        self.drain_pending_candidates().await;

        let answer = self.pc.create_answer(None).await?;
        self.pc.set_local_description(answer.clone()).await?;
        self.has_local_offer.store(false, Ordering::Release);
        self.negotiation_id.store(negotiation_id, Ordering::Release);
        Ok(Some(answer.sdp))
    }

    pub async fn accept_answer(
        &self,
        _negotiation_id: u64,
        sdp: String,
    ) -> Result<(), TransportError> {
        let _guard = self.negotiation_lock.lock().await;
        let answer = RTCSessionDescription::answer(sdp)?;
        self.pc.set_remote_description(answer).await?;
        self.has_local_offer.store(false, Ordering::Release);
        self.remote_description_set.store(true, Ordering::Release);
        self.drain_pending_candidates().await;
        Ok(())
    }

    pub async fn add_ice_candidate(
        &self,
        candidate: RTCIceCandidateInit,
    ) -> Result<(), TransportError> {
        if self.remote_description_set.load(Ordering::Acquire) {
            self.pc.add_ice_candidate(candidate).await?;
        } else {
            self.pending_candidates.lock().await.push(candidate);
        }
        Ok(())
    }

    async fn drain_pending_candidates(&self) {
        let candidates = {
            let mut pending = self.pending_candidates.lock().await;
            std::mem::take(&mut *pending)
        };

        for candidate in candidates {
            if let Err(error) = self.pc.add_ice_candidate(candidate).await {
                log::warn!(
                    "peer {}: falha ao aplicar candidato ICE pendente: {error}",
                    self.peer_id
                );
            }
        }
    }

    /// Envia um pacote Opus já codificado para este peer.
    pub async fn send_audio(&self, opus: &[u8]) -> Result<(), TransportError> {
        if !self.is_connected() {
            return Ok(());
        }

        let sample = Sample {
            data: Bytes::copy_from_slice(opus),
            timestamp: Instant::now(),
            duration: Duration::from_millis(u64::from(FRAME_MS)),
            ..Sample::new(Instant::now())
        };

        self.local_track
            .sample_writer(self.ssrc, self.payload_type)
            .write_sample(&sample)
            .await?;
        Ok(())
    }

    /// Envia uma mensagem de controle pelo DataChannel. Retorna `false` se o
    /// canal ainda não estiver disponível.
    pub async fn send_control(&self, message: &ControlMessage) -> Result<bool, TransportError> {
        let guard = self.shared.control.lock().await;
        let Some(channel) = guard.as_ref() else {
            return Ok(false);
        };

        if !self.shared.control_ready.load(Ordering::Acquire) {
            return Ok(false);
        }

        let data: BytesMut = encode_control_mut(message)?;
        channel.send(data).await?;
        Ok(true)
    }

    pub async fn close(&self) {
        let _ = self.pc.close().await;
        self.shared.connected.store(false, Ordering::Release);
        self.shared.control_ready.store(false, Ordering::Release);
    }
}

struct TransportHandler {
    peer_id: PeerId,
    event_tx: tokio::sync::mpsc::UnboundedSender<TransportEvent>,
    shared: Arc<SharedState>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for TransportHandler {
    async fn on_ice_candidate(&self, event: RTCPeerConnectionIceEvent) {
        if event.candidate.address.is_empty() {
            return;
        }

        match event.candidate.to_json() {
            Ok(candidate) => {
                let _ = self.event_tx.send(TransportEvent::LocalIceCandidate {
                    peer_id: self.peer_id,
                    candidate,
                });
            }
            Err(error) => {
                log::warn!(
                    "peer {}: falha ao serializar candidato ICE: {error}",
                    self.peer_id
                );
            }
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        let connected = state == RTCPeerConnectionState::Connected;
        self.shared.connected.store(connected, Ordering::Release);
        let _ = self.event_tx.send(TransportEvent::ConnectionStateChanged {
            peer_id: self.peer_id,
            state,
        });
    }

    async fn on_data_channel(&self, data_channel: Arc<dyn DataChannel>) {
        let label = match data_channel.label().await {
            Ok(label) => label,
            Err(error) => {
                log::warn!(
                    "peer {}: falha ao ler label do DataChannel: {error}",
                    self.peer_id
                );
                return;
            }
        };

        if channel_role(&label) != Some(ChannelRole::Control) {
            log::debug!("peer {}: DataChannel ignorado ({label})", self.peer_id);
            return;
        }

        *self.shared.control.lock().await = Some(Arc::clone(&data_channel));
        spawn_data_channel_reader(
            self.peer_id,
            data_channel,
            Arc::clone(&self.shared),
            self.event_tx.clone(),
        );
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        if track.kind().await != RtpCodecKind::Audio {
            return;
        }

        let peer_id = self.peer_id;
        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            while let Some(event) = track.poll().await {
                match event {
                    TrackRemoteEvent::OnRtpPacket(packet) => {
                        if event_tx
                            .send(TransportEvent::InboundRtp { peer_id, packet })
                            .is_err()
                        {
                            break;
                        }
                    }
                    TrackRemoteEvent::OnEnded => {
                        let _ = event_tx.send(TransportEvent::RemoteTrackEnded { peer_id });
                        break;
                    }
                    _ => {}
                }
            }
        });
    }
}

fn spawn_data_channel_reader(
    peer_id: PeerId,
    channel: Arc<dyn DataChannel>,
    shared: Arc<SharedState>,
    event_tx: tokio::sync::mpsc::UnboundedSender<TransportEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = channel.poll().await {
            match event {
                DataChannelEvent::OnOpen => {
                    shared.control_ready.store(true, Ordering::Release);
                    let _ = event_tx.send(TransportEvent::ControlChannelOpen { peer_id });
                }
                DataChannelEvent::OnMessage(message) => {
                    match decode_control(message.data.as_ref()) {
                        Ok(message) => {
                            let _ =
                                event_tx.send(TransportEvent::ControlMessage { peer_id, message });
                        }
                        Err(error) => {
                            log::warn!("peer {peer_id}: controle inválido: {error}");
                        }
                    }
                }
                DataChannelEvent::OnClose | DataChannelEvent::OnClosing => {
                    shared.control_ready.store(false, Ordering::Release);
                    break;
                }
                DataChannelEvent::OnError => {
                    log::warn!("peer {peer_id}: erro no DataChannel de controle");
                }
                _ => {}
            }
        }
    });
}

/// Compatibilidade com o WIP anterior de `main.rs`. A implementação real da
/// malha está em `MeshTransport`; esta estrutura é mantida apenas como API
/// mínima de criação de uma `PeerConnection` isolada.
pub struct WebRtcPeer {
    pub peer_connection: Arc<dyn PeerConnection>,
}

impl WebRtcPeer {
    pub async fn new() -> anyhow::Result<Self> {
        let config = RTCConfigurationBuilder::default()
            .with_ice_servers(vec![webrtc::peer_connection::RTCIceServer {
                urls: vec!["stun:stun.cloudflare.com:3478".to_owned()],
                ..Default::default()
            }])
            .build();
        let peer_connection = PeerConnectionBuilder::new()
            .with_configuration(config)
            .with_udp_addrs(vec!["0.0.0.0:0"])
            .build()
            .await?;

        Ok(Self {
            peer_connection: Arc::new(peer_connection),
        })
    }

    pub async fn create_offer(&self) -> anyhow::Result<RTCSessionDescription> {
        let offer = self.peer_connection.create_offer(None).await?;
        self.peer_connection
            .set_local_description(offer.clone())
            .await?;
        Ok(offer)
    }

    pub async fn handle_answer(&self, answer: RTCSessionDescription) -> anyhow::Result<()> {
        self.peer_connection.set_remote_description(answer).await?;
        Ok(())
    }
}

pub async fn peer_builder() -> anyhow::Result<WebRtcPeer> {
    WebRtcPeer::new().await
}
