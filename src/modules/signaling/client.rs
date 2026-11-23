use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

use crate::modules::protocol::{ClientMessage, PeerId, ServerMessage, SignalPayload};

use super::SignalingError;

/// Eventos entregues pelo servidor Rendezvous ao restante da aplicação.
#[derive(Debug)]
pub enum SignalingEvent {
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
    Closed {
        reason: String,
    },
}

/// Cliente WebSocket do Rendezvous.
///
/// A conexão tem duas tasks: uma escreve comandos e outra lê eventos. A mídia
/// nunca passa por aqui.
pub struct SignalingClient {
    tx: mpsc::UnboundedSender<ClientMessage>,
    peer_id: PeerId,
}

impl SignalingClient {
    pub async fn connect(
        url: &str,
        room: &str,
        peer_id: PeerId,
        display_name: &str,
    ) -> Result<(Self, mpsc::UnboundedReceiver<SignalingEvent>), SignalingError> {
        let (socket, _) = connect_async(url).await?;
        let (mut sink, mut stream) = socket.split();

        let register = ClientMessage::Register {
            room: room.to_owned(),
            peer_id,
            display_name: display_name.to_owned(),
        };
        sink.send(Message::Text(serde_json::to_string(&register)?.into()))
            .await?;

        let (command_tx, mut command_rx) = mpsc::unbounded_channel::<ClientMessage>();
        let (event_tx, event_rx) = mpsc::unbounded_channel::<SignalingEvent>();

        tokio::spawn(async move {
            while let Some(command) = command_rx.recv().await {
                let serialized = match serde_json::to_string(&command) {
                    Ok(value) => value,
                    Err(error) => {
                        log::warn!("sinalização: falha ao serializar comando: {error}");
                        continue;
                    }
                };

                if sink.send(Message::Text(serialized.into())).await.is_err() {
                    break;
                }
            }

            let _ = sink.close().await;
        });

        let event_tx_closed = event_tx;
        tokio::spawn(async move {
            loop {
                match stream.next().await {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ServerMessage>(&text) {
                            Ok(ServerMessage::Registered { peers }) => {
                                if event_tx_closed
                                    .send(SignalingEvent::Registered { peers })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Ok(ServerMessage::PeerJoined {
                                peer_id,
                                display_name,
                            }) => {
                                if event_tx_closed
                                    .send(SignalingEvent::PeerJoined {
                                        peer_id,
                                        display_name,
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Ok(ServerMessage::PeerLeft { peer_id }) => {
                                if event_tx_closed
                                    .send(SignalingEvent::PeerLeft { peer_id })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Ok(ServerMessage::Relay { from, payload }) => {
                                if event_tx_closed
                                    .send(SignalingEvent::Relay { from, payload })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Ok(ServerMessage::Error { message }) => {
                                log::warn!("sinalização: erro do servidor: {message}");
                            }
                            Err(error) => {
                                log::warn!("sinalização: mensagem inválida: {error}");
                            }
                        }
                    }
                    Some(Ok(Message::Close(frame))) => {
                        let reason = frame
                            .map(|frame| frame.reason.to_string())
                            .unwrap_or_else(|| "conexão fechada pelo servidor".to_owned());
                        let _ = event_tx_closed.send(SignalingEvent::Closed { reason });
                        break;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        let _ = event_tx_closed.send(SignalingEvent::Closed {
                            reason: error.to_string(),
                        });
                        break;
                    }
                    None => {
                        let _ = event_tx_closed.send(SignalingEvent::Closed {
                            reason: "conexão encerrada".to_owned(),
                        });
                        break;
                    }
                }
            }
        });

        Ok((
            Self {
                tx: command_tx,
                peer_id,
            },
            event_rx,
        ))
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    pub fn relay(&self, to: Option<PeerId>, payload: SignalPayload) -> Result<(), SignalingError> {
        self.tx
            .send(ClientMessage::Relay { to, payload })
            .map_err(|_| SignalingError::Closed)
    }

    pub fn broadcast(&self, payload: SignalPayload) -> Result<(), SignalingError> {
        self.relay(None, payload)
    }

    pub fn leave(&self) -> Result<(), SignalingError> {
        self.tx
            .send(ClientMessage::Leave)
            .map_err(|_| SignalingError::Closed)
    }
}

/// Aguarda a confirmação de registro sem consumir o receiver de eventos.
pub async fn wait_registered(
    events: &mut mpsc::UnboundedReceiver<SignalingEvent>,
    timeout: Duration,
) -> Result<Vec<PeerId>, SignalingError> {
    while let Ok(Some(event)) = tokio::time::timeout(timeout, events.recv()).await {
        match event {
            SignalingEvent::Registered { peers } => return Ok(peers),
            SignalingEvent::Closed { reason } => {
                return Err(SignalingError::Transport(reason));
            }
            _ => {}
        }
    }

    Err(SignalingError::Timeout)
}

// Mantém o tipo público do WebSocket nomeável em logs e testes, sem expor
// detalhes do tungstenite no restante da aplicação.
pub type SignalingSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
