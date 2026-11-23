//! Lógica pura de roster, admissão e sucessão de host.
//!
//! Nada aqui abre sockets, toca áudio ou depende de `tokio`. O objetivo é que
//! as regras determinísticas de FIFO, capacidade e epoch possam ser testadas
//! isoladamente.

use std::collections::HashSet;

use crate::modules::protocol::{Member, ParticipantId, PeerId, RejectReason, RosterSnapshot};

/// Resultado de um pedido de entrada.
#[derive(Clone, Debug)]
pub enum Admission {
    /// O peer ainda não participava e recebeu um assento novo.
    New(Member, RosterSnapshot),
    /// O peer já participava; a resposta é idempotente para retries.
    Existing(Member, RosterSnapshot),
}

/// Diferença observada ao aplicar uma fotografia do roster.
#[derive(Clone, Debug, Default)]
pub struct RosterDiff {
    pub accepted: bool,
    pub host_changed: bool,
    pub self_removed: bool,
    pub added: Vec<Member>,
    pub removed: Vec<Member>,
    pub self_participant_id: Option<ParticipantId>,
}

/// Estado local da participação no roster.
///
/// O host é a única autoridade de admissão. Peers comuns mantêm uma cópia e
/// aceitam apenas versões mais novas `(epoch, revision)`.
#[derive(Clone, Debug)]
pub struct Membership {
    self_peer_id: PeerId,
    participant_id: Option<ParticipantId>,
    roster: Option<RosterSnapshot>,
    max_participants: usize,
}

impl Membership {
    /// Estado antes de ser admitido por um host existente.
    pub fn new_unjoined(self_peer_id: PeerId, max_participants: usize) -> Self {
        Self {
            self_peer_id,
            participant_id: None,
            roster: None,
            max_participants,
        }
    }

    /// Primeiro peer da sala: cria a sala e assume a coordenação.
    pub fn bootstrap(self_peer_id: PeerId, max_participants: usize) -> Self {
        let member = Member {
            peer_id: self_peer_id,
            participant_id: 1,
            join_timestamp: 1,
        };
        let roster = RosterSnapshot {
            epoch: 0,
            revision: 0,
            host_id: self_peer_id,
            next_join_timestamp: 2,
            next_participant_id: 2,
            members: vec![member],
        };

        Self {
            self_peer_id,
            participant_id: Some(1),
            roster: Some(roster),
            max_participants,
        }
    }

    /// Reconstrói o estado a partir de um `JoinAccepted`.
    pub fn from_join(
        self_peer_id: PeerId,
        self_participant_id: ParticipantId,
        roster: RosterSnapshot,
        max_participants: usize,
    ) -> Result<Self, String> {
        let member = roster
            .member(self_peer_id)
            .ok_or_else(|| "roster de entrada não contém o peer local".to_owned())?;
        if member.participant_id != self_participant_id {
            return Err(format!(
                "participant_id divergente: esperado {}, recebido {}",
                self_participant_id, member.participant_id
            ));
        }
        validate_roster(&roster)?;

        let mut roster = roster;
        roster.members.sort_by_key(Member::order_key);

        Ok(Self {
            self_peer_id,
            participant_id: Some(self_participant_id),
            roster: Some(roster),
            max_participants,
        })
    }

    pub fn self_peer_id(&self) -> PeerId {
        self.self_peer_id
    }

    pub fn participant_id(&self) -> Option<ParticipantId> {
        self.participant_id
    }

    pub fn admitted(&self) -> bool {
        self.participant_id.is_some() && self.roster.is_some()
    }

    pub fn is_host(&self) -> bool {
        self.roster
            .as_ref()
            .is_some_and(|roster| roster.host_id == self.self_peer_id)
    }

    pub fn host_id(&self) -> Option<PeerId> {
        self.roster.as_ref().map(|roster| roster.host_id)
    }

    pub fn epoch(&self) -> u64 {
        self.roster.as_ref().map_or(0, |roster| roster.epoch)
    }

    pub fn revision(&self) -> u64 {
        self.roster.as_ref().map_or(0, |roster| roster.revision)
    }

    pub fn snapshot(&self) -> Option<&RosterSnapshot> {
        self.roster.as_ref()
    }

    pub fn members(&self) -> &[Member] {
        self.roster
            .as_ref()
            .map_or(&[], |roster| roster.members.as_slice())
    }

    pub fn len(&self) -> usize {
        self.members().len()
    }

    pub fn is_empty(&self) -> bool {
        self.members().is_empty()
    }

    pub fn has_capacity(&self) -> bool {
        self.len() < self.max_participants
    }

    pub fn member(&self, peer_id: PeerId) -> Option<&Member> {
        self.roster
            .as_ref()
            .and_then(|roster| roster.member(peer_id))
    }

    pub fn participant_of(&self, peer_id: PeerId) -> Option<ParticipantId> {
        self.member(peer_id).map(|member| member.participant_id)
    }

    /// Adiciona um peer à sala. Só o host pode admitir.
    pub fn admit(&mut self, peer_id: PeerId) -> Result<Admission, RejectReason> {
        let roster = self.roster.as_mut().ok_or(RejectReason::NotHost)?;
        if roster.host_id != self.self_peer_id {
            return Err(RejectReason::NotHost);
        }

        if let Some(member) = roster.member(peer_id).cloned() {
            return Ok(Admission::Existing(member, roster.clone()));
        }

        if roster.members.len() >= self.max_participants {
            return Err(RejectReason::RoomFull);
        }

        let participant_id = roster.next_participant_id;
        let join_timestamp = roster.next_join_timestamp;
        roster.next_participant_id = participant_id
            .checked_add(1)
            .ok_or(RejectReason::ShuttingDown)?;
        roster.next_join_timestamp = join_timestamp
            .checked_add(1)
            .ok_or(RejectReason::ShuttingDown)?;

        let member = Member {
            peer_id,
            participant_id,
            join_timestamp,
        };
        roster.members.push(member.clone());
        roster.members.sort_by_key(Member::order_key);
        roster.revision = roster.revision.saturating_add(1);

        Ok(Admission::New(member, roster.clone()))
    }

    /// Remove um membro como host, invalidando o roster anterior.
    pub fn remove_as_host(&mut self, peer_id: PeerId) -> Option<RosterSnapshot> {
        let roster = self.roster.as_mut()?;
        if roster.host_id != self.self_peer_id || peer_id == self.self_peer_id {
            return None;
        }

        let index = roster
            .members
            .iter()
            .position(|member| member.peer_id == peer_id)?;
        roster.members.remove(index);
        roster.revision = roster.revision.saturating_add(1);
        Some(roster.clone())
    }

    /// Remove um membro na visão local de um peer comum, sem alegar ser host.
    ///
    /// A revisão não muda: a próxima fotografia vinda do host continuará mais
    /// nova e substituirá a visão local.
    pub fn forget_member(&mut self, peer_id: PeerId) -> Option<Member> {
        let roster = self.roster.as_mut()?;
        if peer_id == self.self_peer_id {
            return None;
        }
        let index = roster
            .members
            .iter()
            .position(|member| member.peer_id == peer_id)?;
        Some(roster.members.remove(index))
    }

    /// Assume a coordenação após a morte do host.
    ///
    /// Remove o host antigo e qualquer membro que o chamador considere morto.
    /// O novo epoch invalida mensagens do host anterior.
    pub fn claim_host(&mut self, alive: &HashSet<PeerId>) -> Option<RosterSnapshot> {
        let roster = self.roster.as_mut()?;
        if roster.host_id == self.self_peer_id || !roster.contains(self.self_peer_id) {
            return None;
        }

        let old_host = roster.host_id;
        roster.members.retain(|member| {
            member.peer_id == self.self_peer_id
                || (member.peer_id != old_host && alive.contains(&member.peer_id))
        });

        roster.host_id = self.self_peer_id;
        roster.epoch = roster.epoch.saturating_add(1);
        roster.revision = roster.revision.saturating_add(1);
        roster.members.sort_by_key(Member::order_key);
        Some(roster.clone())
    }

    /// Prepara a saída graciosa do host.
    ///
    /// O sucessor é o membro vivo mais antigo, excluindo o próprio host. Se
    /// não houver ninguém vivo, não existe transferência.
    pub fn transfer_away(&mut self, alive: &HashSet<PeerId>) -> Option<RosterSnapshot> {
        let roster = self.roster.as_mut()?;
        if roster.host_id != self.self_peer_id {
            return None;
        }

        roster.members.retain(|member| {
            member.peer_id != self.self_peer_id
                && member.peer_id != roster.host_id
                && alive.contains(&member.peer_id)
        });
        roster.members.sort_by_key(Member::order_key);

        let successor = roster.members.first()?.peer_id;
        roster.host_id = successor;
        roster.epoch = roster.epoch.saturating_add(1);
        roster.revision = roster.revision.saturating_add(1);
        Some(roster.clone())
    }

    /// Aplica uma fotografia de roster recebida do host ou de uma eleição.
    ///
    /// Precedência: epoch maior sempre vence. No mesmo epoch, o host atual
    /// vence pela revisão; empates de host diferente são resolvidos pelo menor
    /// `PeerId` para convergência determinística.
    pub fn apply_snapshot(&mut self, snapshot: RosterSnapshot) -> RosterDiff {
        let mut snapshot = snapshot;
        snapshot.members.sort_by_key(Member::order_key);

        if validate_roster(&snapshot).is_err() {
            return RosterDiff::default();
        }

        let accepted = match &self.roster {
            None => snapshot.contains(self.self_peer_id),
            Some(current) => {
                if snapshot.epoch > current.epoch {
                    true
                } else if snapshot.epoch < current.epoch {
                    false
                } else if snapshot.host_id == current.host_id {
                    snapshot.revision > current.revision
                } else {
                    snapshot.host_id < current.host_id
                }
            }
        };

        if !accepted {
            return RosterDiff::default();
        }

        let previous = self.roster.take();
        let mut diff = RosterDiff {
            accepted: true,
            host_changed: previous
                .as_ref()
                .is_some_and(|old| old.host_id != snapshot.host_id),
            ..RosterDiff::default()
        };

        if let Some(old) = previous {
            for member in &old.members {
                if !snapshot.contains(member.peer_id) {
                    diff.removed.push(member.clone());
                }
            }
            for member in &snapshot.members {
                if !old.contains(member.peer_id) {
                    diff.added.push(member.clone());
                }
            }
        } else {
            diff.added = snapshot.members.clone();
        }

        self.participant_id = snapshot
            .member(self.self_peer_id)
            .map(|member| member.participant_id);
        diff.self_participant_id = self.participant_id;
        diff.self_removed = self.participant_id.is_none();
        self.roster = Some(snapshot);

        diff
    }

    /// Atualiza apenas o epoch/host a partir de um heartbeat.
    ///
    /// Isso impede que um host antigo continue admitindo peers depois de já
    /// existir um epoch mais novo. A fotografia completa chega em seguida por
    /// `HostClaim`/`HostTransfer`.
    pub fn adopt_host_epoch(&mut self, epoch: u64, host_id: PeerId) -> bool {
        let Some(roster) = self.roster.as_mut() else {
            return false;
        };
        if !roster.contains(host_id) {
            return false;
        }

        let accept = epoch > roster.epoch
            || (epoch == roster.epoch && host_id < roster.host_id && host_id != roster.host_id);
        if !accept {
            return false;
        }

        roster.epoch = epoch;
        // A revisão é relativa ao epoch. Ao adotar um epoch novo sem a
        // fotografia completa, zeramos a revisão para que o `HostClaim`
        // seguinte (mesma revisão lógica) não seja descartado como duplicado.
        roster.revision = 0;
        roster.host_id = host_id;
        true
    }
}

fn validate_roster(roster: &RosterSnapshot) -> Result<(), String> {
    if roster.members.is_empty() {
        return Err("roster vazio".to_owned());
    }
    if !roster.contains(roster.host_id) {
        return Err("host não está no roster".to_owned());
    }

    let mut peer_ids = HashSet::with_capacity(roster.members.len());
    let mut participant_ids = HashSet::with_capacity(roster.members.len());
    for member in &roster.members {
        if !peer_ids.insert(member.peer_id) {
            return Err(format!("peer duplicado no roster: {}", member.peer_id));
        }
        if !participant_ids.insert(member.participant_id) {
            return Err(format!(
                "participant_id duplicado no roster: {}",
                member.participant_id
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids<const N: usize>() -> [PeerId; N] {
        std::array::from_fn(|_| PeerId::new())
    }

    #[test]
    fn bootstrap_torna_o_primeiro_peer_host() {
        let host_id = PeerId::new();
        let membership = Membership::bootstrap(host_id, 8);

        assert!(membership.admitted());
        assert!(membership.is_host());
        assert_eq!(membership.participant_id(), Some(1));
        assert_eq!(membership.len(), 1);
        assert_eq!(membership.host_id(), Some(host_id));
        assert_eq!(membership.epoch(), 0);
    }

    #[test]
    fn admissao_atribui_ids_e_incrementa_revisao() {
        let host_id = PeerId::new();
        let [first, second] = ids();
        let mut membership = Membership::bootstrap(host_id, 8);

        let Admission::New(first_member, _) = membership.admit(first).unwrap() else {
            panic!("primeira admissão deveria ser nova");
        };
        assert_eq!(first_member.participant_id, 2);
        assert_eq!(first_member.join_timestamp, 2);

        let Admission::New(second_member, _) = membership.admit(second).unwrap() else {
            panic!("segunda admissão deveria ser nova");
        };
        assert_eq!(second_member.participant_id, 3);
        assert_eq!(second_member.join_timestamp, 3);
        assert_eq!(membership.len(), 3);
        assert_eq!(membership.revision(), 2);
        assert_eq!(membership.snapshot().unwrap().next_participant_id, 4);
    }

    #[test]
    fn admissao_repetida_e_idempotente() {
        let host_id = PeerId::new();
        let [peer] = ids();
        let mut membership = Membership::bootstrap(host_id, 8);

        membership.admit(peer).unwrap();
        let revision = membership.revision();
        let Admission::Existing(member, roster) = membership.admit(peer).unwrap() else {
            panic!("retry deveria retornar existente");
        };

        assert_eq!(member.peer_id, peer);
        assert_eq!(membership.revision(), revision);
        assert_eq!(roster.len(), membership.len());
    }

    #[test]
    fn nono_participante_e_rejeitado() {
        let host_id = PeerId::new();
        let mut membership = Membership::bootstrap(host_id, 8);
        let peers: [PeerId; 8] = std::array::from_fn(|_| PeerId::new());

        for peer in peers.iter().take(7) {
            membership.admit(*peer).unwrap();
        }
        assert_eq!(membership.len(), 8);

        assert!(matches!(
            membership.admit(peers[7]),
            Err(RejectReason::RoomFull)
        ));
    }

    #[test]
    fn peer_comum_nao_admite() {
        let host_id = PeerId::new();
        let [peer] = ids();
        let mut host = Membership::bootstrap(host_id, 8);
        host.admit(peer).unwrap();

        let mut common =
            Membership::from_join(peer, 2, host.snapshot().unwrap().clone(), 8).unwrap();
        let candidate = PeerId::new();
        assert!(matches!(
            common.admit(candidate),
            Err(RejectReason::NotHost)
        ));
    }

    #[test]
    fn remocao_pelo_host_invalida_o_roster() {
        let host_id = PeerId::new();
        let [peer] = ids();
        let mut membership = Membership::bootstrap(host_id, 8);
        membership.admit(peer).unwrap();
        let before = membership.revision();

        let roster = membership.remove_as_host(peer).unwrap();
        assert_eq!(roster.revision, before + 1);
        assert!(roster.member(peer).is_none());
        assert_eq!(membership.len(), 1);
    }

    #[test]
    fn snapshot_antigo_e_ignorado_e_claim_maior_vence() {
        let host_id = PeerId::new();
        let [peer] = ids();
        let mut host = Membership::bootstrap(host_id, 8);
        host.admit(peer).unwrap();
        let joined = host.snapshot().unwrap().clone();

        let mut common = Membership::from_join(peer, 2, joined.clone(), 8).unwrap();

        let mut stale = joined.clone();
        stale.revision = stale.revision.saturating_sub(1);
        assert!(!common.apply_snapshot(stale).accepted);

        let mut claim = joined;
        claim.epoch += 1;
        claim.host_id = peer;
        claim.revision += 1;
        let diff = common.apply_snapshot(claim);

        assert!(diff.accepted);
        assert!(diff.host_changed);
        assert!(common.is_host());
        assert_eq!(common.epoch(), 1);
    }

    #[test]
    fn heartbeat_de_epoch_novo_nao_descarta_o_claim_seguinte() {
        let host_id = PeerId::new();
        let [peer] = ids();
        let mut host = Membership::bootstrap(host_id, 8);
        host.admit(peer).unwrap();
        let joined = host.snapshot().unwrap().clone();
        let mut common = Membership::from_join(peer, 2, joined.clone(), 8).unwrap();

        // Heartbeat chega antes do HostClaim, apenas com epoch/host.
        assert!(common.adopt_host_epoch(1, peer));
        assert_eq!(common.epoch(), 1);

        // O HostClaim completo traz a mesma revisão que o heartbeat; precisa
        // ser aceito para substituir a lista antiga de membros.
        let mut claim = joined;
        claim.epoch = 1;
        claim.host_id = peer;
        claim.revision += 1;
        claim.members.retain(|member| member.peer_id != host_id);
        let diff = common.apply_snapshot(claim);

        assert!(diff.accepted);
        assert_eq!(common.len(), 1);
        assert!(common.is_host());
    }

    #[test]
    fn claim_host_remove_host_morto_e_incrementa_epoch() {
        let host_id = PeerId::new();
        let [peer, dead_peer] = ids();
        let mut host = Membership::bootstrap(host_id, 8);
        host.admit(peer).unwrap();
        host.admit(dead_peer).unwrap();
        let joined = host.snapshot().unwrap().clone();
        let mut common = Membership::from_join(peer, 2, joined, 8).unwrap();

        let alive = HashSet::from([peer]);
        let claim = common.claim_host(&alive).unwrap();

        assert_eq!(claim.host_id, peer);
        assert_eq!(claim.epoch, 1);
        assert_eq!(claim.len(), 1);
        assert!(claim.member(dead_peer).is_none());
        assert!(common.is_host());
    }

    #[test]
    fn transferencia_escolhe_o_membro_vivo_mais_antigo() {
        let host_id = PeerId::new();
        let [oldest, newer] = ids();
        let mut host = Membership::bootstrap(host_id, 8);
        host.admit(oldest).unwrap();
        host.admit(newer).unwrap();

        let alive = HashSet::from([newer, oldest]);
        let transferred = host.transfer_away(&alive).unwrap();

        assert_eq!(transferred.host_id, oldest);
        assert_eq!(transferred.epoch, 1);
        assert_eq!(transferred.len(), 2);
        assert!(transferred.member(host_id).is_none());
    }

    #[test]
    fn from_join_valida_participant_id() {
        let host_id = PeerId::new();
        let [peer] = ids();
        let mut host = Membership::bootstrap(host_id, 8);
        host.admit(peer).unwrap();
        let roster = host.snapshot().unwrap().clone();

        assert!(Membership::from_join(peer, 99, roster.clone(), 8).is_err());
        assert!(Membership::from_join(peer, 2, roster, 8).is_ok());
    }
}
