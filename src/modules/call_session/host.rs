//! Liveness e eleição determinística de host.
//!
//! Não existe votação: a ordem FIFO `(join_timestamp, peer_id)` é replicada em
//! todos os peers. A eleição apenas escolhe o menor membro vivo do roster.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tokio::time::Instant;

use crate::modules::protocol::{Member, PeerId, RosterSnapshot};

/// Tempo extra para um peer recém-admitido abrir o DataChannel e enviar o
/// primeiro heartbeat.
pub const NEW_PEER_GRACE: Duration = Duration::from_secs(10);

/// Rastreia a última vez que cada peer deu sinal de vida.
#[derive(Debug)]
pub struct Liveness {
    timeout: Duration,
    expires_at: HashMap<PeerId, Instant>,
}

impl Liveness {
    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            expires_at: HashMap::new(),
        }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Registra um peer recém-admitido, dando a ele uma janela maior para
    /// estabelecer o DataChannel.
    pub fn seed(&mut self, peer_id: PeerId, now: Instant) {
        self.expires_at
            .insert(peer_id, now + self.timeout.max(NEW_PEER_GRACE));
    }

    pub fn mark_seen(&mut self, peer_id: PeerId, now: Instant) {
        self.expires_at.insert(peer_id, now + self.timeout);
    }

    pub fn forget(&mut self, peer_id: PeerId) {
        self.expires_at.remove(&peer_id);
    }

    /// Garante que todos os membros conhecidos tenham uma entrada inicial.
    pub fn seed_members(&mut self, roster: &RosterSnapshot, now: Instant) {
        for member in &roster.members {
            self.expires_at
                .entry(member.peer_id)
                .or_insert(now + self.timeout.max(NEW_PEER_GRACE));
        }
    }

    pub fn is_alive(&self, peer_id: PeerId, now: Instant) -> bool {
        self.expires_at
            .get(&peer_id)
            .is_some_and(|expires_at| *expires_at >= now)
    }

    /// `true` apenas quando o peer já foi visto e o prazo expirou. Um peer
    /// nunca visto não é declarado morto por esta função.
    pub fn timed_out(&self, peer_id: PeerId, now: Instant) -> bool {
        self.expires_at
            .get(&peer_id)
            .is_some_and(|expires_at| *expires_at < now)
    }

    /// Conjunto de membros vivos, excluindo o host antigo quando informado.
    /// O peer local é sempre considerado vivo.
    pub fn alive_members(
        &self,
        roster: &RosterSnapshot,
        now: Instant,
        self_peer_id: PeerId,
        exclude: Option<PeerId>,
    ) -> HashSet<PeerId> {
        roster
            .members
            .iter()
            .filter(|member| {
                Some(member.peer_id) != exclude
                    && (member.peer_id == self_peer_id || self.is_alive(member.peer_id, now))
            })
            .map(|member| member.peer_id)
            .collect()
    }
}

/// Escolhe o membro vivo mais antigo, com desempate estável por `PeerId`.
pub fn elect_oldest(
    roster: &RosterSnapshot,
    alive: &HashSet<PeerId>,
    exclude: Option<PeerId>,
) -> Option<Member> {
    roster
        .sorted_members()
        .into_iter()
        .find(|member| Some(member.peer_id) != exclude && alive.contains(&member.peer_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster(members: &[(PeerId, u64)]) -> RosterSnapshot {
        RosterSnapshot {
            epoch: 0,
            revision: 0,
            host_id: members[0].0,
            next_join_timestamp: members.len() as u64 + 1,
            next_participant_id: members.len() as u32 + 1,
            members: members
                .iter()
                .map(|(peer_id, join_timestamp)| Member {
                    peer_id: *peer_id,
                    participant_id: *join_timestamp as u32,
                    join_timestamp: *join_timestamp,
                })
                .collect(),
        }
    }

    #[test]
    fn elege_o_membro_vivo_mais_antigo() {
        let [host, oldest, newer] = std::array::from_fn(|_| PeerId::new());
        let roster = roster(&[(host, 1), (oldest, 2), (newer, 3)]);
        let alive = HashSet::from([oldest, newer]);

        let winner = elect_oldest(&roster, &alive, Some(host)).unwrap();
        assert_eq!(winner.peer_id, oldest);
    }

    #[test]
    fn pula_membros_mortos() {
        let [host, dead, survivor] = std::array::from_fn(|_| PeerId::new());
        let roster = roster(&[(host, 1), (dead, 2), (survivor, 3)]);
        let alive = HashSet::from([survivor]);

        let winner = elect_oldest(&roster, &alive, Some(host)).unwrap();
        assert_eq!(winner.peer_id, survivor);
    }

    #[test]
    fn empate_de_timestamp_desempata_por_peer_id() {
        let [host, lower, higher] = {
            let mut peers: [PeerId; 3] = std::array::from_fn(|_| PeerId::new());
            peers.sort();
            peers
        };
        let roster = roster(&[(host, 1), (higher, 2), (lower, 2)]);
        let alive = HashSet::from([higher, lower]);

        let winner = elect_oldest(&roster, &alive, Some(host)).unwrap();
        assert_eq!(winner.peer_id, lower);
    }

    #[test]
    fn liveness_expira_no_timeout() {
        let peer = PeerId::new();
        let now = Instant::now();
        let mut liveness = Liveness::new(Duration::from_millis(3500));

        liveness.seed(peer, now);
        assert!(liveness.is_alive(peer, now + Duration::from_secs(4)));
        assert!(!liveness.timed_out(peer, now + Duration::from_secs(4)));

        liveness.mark_seen(peer, now);
        assert!(!liveness.is_alive(peer, now + Duration::from_secs(4)));
        assert!(liveness.timed_out(peer, now + Duration::from_secs(4)));
    }

    #[test]
    fn self_e_sempre_considerado_vivo() {
        let [host, peer] = std::array::from_fn(|_| PeerId::new());
        let roster = roster(&[(host, 1), (peer, 2)]);
        let liveness = Liveness::new(Duration::from_secs(1));
        let now = Instant::now();

        let alive = liveness.alive_members(&roster, now, peer, Some(host));
        assert_eq!(alive, HashSet::from([peer]));
    }
}
