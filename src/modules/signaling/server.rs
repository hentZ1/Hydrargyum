use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use crate::modules::protocol::{ClientMessage, PeerId, ServerMessage, SignalPayload};

#[derive(Clone)]
struct PeerHandle {
    tx: mpsc::UnboundedSender<ServerMessage>,
}

#[derive(Default)]
struct Room {
    peers: HashMap<PeerId, PeerHandle>,
}

#[derive(Default)]
struct ServerState {
    rooms: Mutex<HashMap<String, Room>>,
}

/// Servidor Rendezvous em memória.
///
/// Não há persistência, autenticação ou conhecimento de host. O servidor só
/// registra sockets por sala e encaminha envelopes direcionados ou broadcast.
pub struct RendezvousServer {
    state: Arc<ServerState>,
}

impl Default for RendezvousServer {
    fn default() -> Self {
        Self::new()
    }
}

impl RendezvousServer {
    pub fn new() -> Self {
        Self {
            state: Arc::new(ServerState::default()),
        }
    }

    /// Associa o servidor a um endereço local e devolve a porta efetiva.
    pub async fn bind(addr: impl tokio::net::ToSocketAddrs) -> std::io::Result<(Self, SocketAddr)> {
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        let server = Self::new();
        let state = Arc::clone(&server.state);
        tokio::spawn(async move {
            if let Err(error) = accept_loop(listener, state).await {
                log::error!("rendezvous: erro no accept loop: {error}");
            }
        });
        Ok((server, local_addr))
    }

    /// Executa o servidor em um listener já aberto (útil para testes).
    pub async fn serve(self, listener: TcpListener) -> std::io::Result<()> {
        accept_loop(listener, self.state).await
    }
}

/// Atalho para binários: abre o listener e roda até ser cancelado.
pub async fn serve(listener: TcpListener) -> std::io::Result<()> {
    accept_loop(listener, Arc::new(ServerState::default())).await
}

async fn accept_loop(listener: TcpListener, state: Arc<ServerState>) -> std::io::Result<()> {
    loop {
        let (stream, address) = listener.accept().await?;
        log::debug!("rendezvous: conexão de {address}");
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, state).await {
                log::debug!("rendezvous: conexão encerrada: {error}");
            }
        });
    }
}

async fn handle_connection(stream: TcpStream, state: Arc<ServerState>) -> anyhow::Result<()> {
    let mut websocket = accept_async(stream).await?;

    let first = match websocket.next().await {
        Some(Ok(Message::Text(text))) => serde_json::from_str::<ClientMessage>(&text)?,
        Some(Ok(_)) => anyhow::bail!("primeira mensagem deve ser Register em texto"),
        Some(Err(error)) => return Err(error.into()),
        None => return Ok(()),
    };

    let (room, peer_id, display_name) = match first {
        ClientMessage::Register {
            room,
            peer_id,
            display_name,
        } => (room, peer_id, display_name),
        _ => {
            let _ = websocket
                .send(Message::Text(
                    serde_json::to_string(&ServerMessage::Error {
                        message: "primeira mensagem deve ser Register".to_owned(),
                    })?
                    .into(),
                ))
                .await;
            return Ok(());
        }
    };

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ServerMessage>();

    {
        let mut rooms = state.rooms.lock().await;
        let room_state = rooms.entry(room.clone()).or_default();
        if room_state.peers.contains_key(&peer_id) {
            let _ = websocket
                .send(Message::Text(
                    serde_json::to_string(&ServerMessage::Error {
                        message: "peer_id duplicado".to_owned(),
                    })?
                    .into(),
                ))
                .await;
            return Ok(());
        }

        let existing: Vec<PeerId> = room_state.peers.keys().copied().collect();
        let _ = out_tx.send(ServerMessage::Registered { peers: existing });
        room_state.peers.insert(peer_id, PeerHandle { tx: out_tx });

        for (other_id, handle) in room_state.peers.iter() {
            if *other_id != peer_id {
                let _ = handle.tx.send(ServerMessage::PeerJoined {
                    peer_id,
                    display_name: display_name.clone(),
                });
            }
        }
    }

    let result = loop {
        tokio::select! {
            incoming = websocket.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let message = match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(message) => message,
                            Err(error) => {
                                log::warn!("rendezvous: mensagem inválida de {peer_id}: {error}");
                                continue;
                            }
                        };
                        match message {
                            ClientMessage::Relay { to, payload } => {
                                relay(&state, &room, peer_id, to, payload).await;
                            }
                            ClientMessage::Leave => break Ok(()),
                            ClientMessage::Register { .. } => {
                                log::warn!("rendezvous: Register duplicado de {peer_id}");
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break Ok(()),
                    Some(Ok(_)) => {}
                    Some(Err(error)) => break Err(error.into()),
                }
            }
            outgoing = out_rx.recv() => {
                match outgoing {
                    Some(message) => {
                        let text = serde_json::to_string(&message)?;
                        if websocket.send(Message::Text(text.into())).await.is_err() {
                            break Ok(());
                        }
                    }
                    None => break Ok(()),
                }
            }
        }
    };

    remove_peer(&state, &room, peer_id).await;
    let _ = websocket.close(None).await;
    result
}

async fn relay(
    state: &ServerState,
    room: &str,
    from: PeerId,
    to: Option<PeerId>,
    payload: SignalPayload,
) {
    let rooms = state.rooms.lock().await;
    let Some(room_state) = rooms.get(room) else {
        return;
    };

    match to {
        Some(target) => {
            if let Some(handle) = room_state.peers.get(&target) {
                let _ = handle.tx.send(ServerMessage::Relay {
                    from,
                    payload: payload.clone(),
                });
            }
        }
        None => {
            for (peer_id, handle) in room_state.peers.iter() {
                if *peer_id != from {
                    let _ = handle.tx.send(ServerMessage::Relay {
                        from,
                        payload: payload.clone(),
                    });
                }
            }
        }
    }
}

async fn remove_peer(state: &ServerState, room: &str, peer_id: PeerId) {
    let mut rooms = state.rooms.lock().await;
    let Some(room_state) = rooms.get_mut(room) else {
        return;
    };

    room_state.peers.remove(&peer_id);
    for handle in room_state.peers.values() {
        let _ = handle.tx.send(ServerMessage::PeerLeft { peer_id });
    }

    if room_state.peers.is_empty() {
        rooms.remove(room);
    }
}
