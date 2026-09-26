//! 组队: 邀请制队伍 / 经验分享 / 队友互保。
//!
//! - 队长 + 成员, 上限 11 人; 一人同时只能在一队
//! - 目标栏「组队」发邀请 → 对方接受入队 (发起者无队伍时自动建队任队长)
//! - 队长离队自动移交; 仅剩 1 人解散; 掉线自动离队
//! - 经验: 击杀入账时按 同区且 12 格内、存活在线 的队员均分,
//!   总量 ×(1 + 0.1×(参与人数-1)) 组队加成
//! - 同队玩家互相攻击一律拒绝 (pvp_check 拦截)

use super::*;

/// 队伍人数上限 (经典 11)
const PARTY_MAX: usize = 11;
/// 经验分享半径 (格)
const SHARE_RANGE: f64 = 12.0;

pub(super) struct Party {
    pub(super) leader: String,
    /// 全体成员 (含队长, 按入队序)
    pub(super) members: Vec<String>,
}

impl Game {
    pub(super) fn party_of(&self, char_id: &str) -> Option<usize> {
        self.parties
            .iter()
            .position(|p| p.members.iter().any(|m| m == char_id))
    }

    pub(super) fn same_party(&self, a: &str, b: &str) -> bool {
        match (self.party_of(a), self.party_of(b)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        }
    }

    pub(super) async fn handle_party_invite(&mut self, conn_id: &str, target: &str) {
        let Some(me) = self.char_by_conn(conn_id) else {
            return;
        };
        if me == target {
            return;
        }
        let Some(t) = self.players.get(target) else {
            return self.notify(conn_id, "对方不在线").await;
        };
        if !t.connected {
            return self.notify(conn_id, "对方不在线").await;
        }
        let t_conn = t.conn_id.clone();
        if self.party_of(target).is_some() {
            return self.notify(conn_id, "对方已有队伍").await;
        }
        match self.party_of(&me) {
            Some(pi) => {
                if self.parties[pi].leader != me {
                    return self.notify(conn_id, "只有队长能邀请入队").await;
                }
                if self.parties[pi].members.len() >= PARTY_MAX {
                    return self.notify(conn_id, "队伍已满").await;
                }
            }
            None => {}
        }
        let my_name = self
            .players
            .get(&me)
            .map(|p| p.character.name.clone())
            .unwrap_or_default();
        self.party_invites.insert(target.to_string(), me.clone());
        send_to(
            &self.sessions,
            &t_conn,
            ServerMessage::PartyInvited {
                from_id: me,
                from_name: my_name,
            },
        )
        .await;
        self.notify(conn_id, "组队邀请已发出").await;
    }

    pub(super) async fn handle_party_accept(&mut self, conn_id: &str) {
        let Some(me) = self.char_by_conn(conn_id) else {
            return;
        };
        let Some(from) = self.party_invites.remove(&me) else {
            return;
        };
        if self.party_of(&me).is_some() {
            return self.notify(conn_id, "你已有队伍").await;
        }
        if !self.players.get(&from).is_some_and(|p| p.connected) {
            return self.notify(conn_id, "邀请人已离线").await;
        }
        let pi = match self.party_of(&from) {
            Some(pi) => {
                if self.parties[pi].members.len() >= PARTY_MAX {
                    return self.notify(conn_id, "队伍已满").await;
                }
                pi
            }
            None => {
                self.parties.push(Party {
                    leader: from.clone(),
                    members: vec![from.clone()],
                });
                self.parties.len() - 1
            }
        };
        self.parties[pi].members.push(me.clone());
        let name = self
            .players
            .get(&me)
            .map(|p| p.character.name.clone())
            .unwrap_or_default();
        self.party_broadcast(pi, &format!("{name} 加入了队伍")).await;
        self.sync_party(pi).await;
    }

    pub(super) async fn handle_party_decline(&mut self, conn_id: &str) {
        let Some(me) = self.char_by_conn(conn_id) else {
            return;
        };
        if let Some(from) = self.party_invites.remove(&me) {
            if let Some(p) = self.players.get(&from) {
                let conn = p.conn_id.clone();
                self.notify(&conn, "对方拒绝了组队邀请").await;
            }
        }
    }

    pub(super) async fn handle_party_kick(&mut self, conn_id: &str, member: &str) {
        let Some(me) = self.char_by_conn(conn_id) else {
            return;
        };
        let Some(pi) = self.party_of(&me) else {
            return;
        };
        if self.parties[pi].leader != me || member == me {
            return;
        }
        if !self.parties[pi].members.iter().any(|m| m == member) {
            return;
        }
        if let Some(p) = self.players.get(member) {
            let conn = p.conn_id.clone();
            self.notify(&conn, "你被移出了队伍").await;
        }
        self.remove_from_party(member, "被移出队伍").await;
    }

    pub(super) async fn handle_leave_party(&mut self, conn_id: &str) {
        let Some(me) = self.char_by_conn(conn_id) else {
            return;
        };
        if self.party_of(&me).is_some() {
            self.remove_from_party(&me, "离开了队伍").await;
        }
    }

    /// 摘除成员 (主动离队/被踢/掉线共用): 队长移交, 仅剩 1 人解散
    pub(super) async fn remove_from_party(&mut self, char_id: &str, verb: &str) {
        self.party_invites.remove(char_id);
        self.party_invites.retain(|_, from| from != char_id);
        let Some(pi) = self.party_of(char_id) else {
            return;
        };
        let name = self
            .players
            .get(char_id)
            .map(|p| p.character.name.clone())
            .unwrap_or_else(|| char_id.to_string());
        let p = &mut self.parties[pi];
        p.members.retain(|m| m != char_id);
        if p.leader == char_id {
            if let Some(next) = p.members.first().cloned() {
                p.leader = next;
            }
        }
        // 被摘者的面板清空
        if let Some(pl) = self.players.get(char_id) {
            send_to(
                &self.sessions,
                &pl.conn_id,
                ServerMessage::PartyState {
                    members: Vec::new(),
                },
            )
            .await;
        }
        if self.parties[pi].members.len() <= 1 {
            // 解散: 最后一人也清面板
            let last = self.parties[pi].members.first().cloned();
            self.parties.remove(pi);
            if let Some(l) = last {
                if let Some(pl) = self.players.get(&l) {
                    let conn = pl.conn_id.clone();
                    send_to(
                        &self.sessions,
                        &conn,
                        ServerMessage::PartyState {
                            members: Vec::new(),
                        },
                    )
                    .await;
                    self.notify(&conn, "队伍已解散").await;
                }
            }
            return;
        }
        self.party_broadcast(pi, &format!("{name} {verb}")).await;
        self.sync_party(pi).await;
    }

    async fn party_broadcast(&self, pi: usize, msg: &str) {
        let Some(p) = self.parties.get(pi) else {
            return;
        };
        for m in p.members.clone() {
            if let Some(pl) = self.players.get(&m) {
                if pl.connected {
                    let conn = pl.conn_id.clone();
                    self.notify(&conn, msg).await;
                }
            }
        }
    }

    /// 全量推队伍状态 (成员名/等级/HP/在线/队长标记)
    pub(super) async fn sync_party(&self, pi: usize) {
        let Some(p) = self.parties.get(pi) else {
            return;
        };
        let members: Vec<protocol::PartyMemberInfo> = p
            .members
            .iter()
            .map(|m| {
                let pl = self.players.get(m);
                protocol::PartyMemberInfo {
                    id: m.clone(),
                    name: pl
                        .map(|p| p.character.name.clone())
                        .unwrap_or_else(|| m.clone()),
                    level: pl.map(|p| p.level).unwrap_or(1),
                    hp: pl.map(|p| p.hp).unwrap_or(0),
                    max_hp: pl.map(|p| p.max_hp).unwrap_or(1),
                    online: pl.is_some_and(|p| p.connected),
                    leader: *m == p.leader,
                }
            })
            .collect();
        for m in &p.members {
            if let Some(pl) = self.players.get(m) {
                if pl.connected {
                    send_to(
                        &self.sessions,
                        &pl.conn_id,
                        ServerMessage::PartyState {
                            members: members.clone(),
                        },
                    )
                    .await;
                }
            }
        }
    }

    /// tick 1Hz: 刷队员 HP
    pub(super) async fn sync_parties_periodic(&mut self, now: Instant) {
        if now.duration_since(self.last_party_sync) < Duration::from_secs(1) {
            return;
        }
        self.last_party_sync = now;
        for pi in 0..self.parties.len() {
            self.sync_party(pi).await;
        }
    }

    /// 击杀经验入账: 有队伍按 同区 12 格内存活在线队员 均分,
    /// 总量 ×(1+0.1×(n-1)); 无队伍/无人在范围 → 全归击杀者
    pub(super) async fn award_exp_party(&mut self, killer: &str, base: u64) {
        let Some(pi) = self.party_of(killer) else {
            return self.award_exp(killer, base).await;
        };
        let (kz, kx, ky) = match self.players.get(killer) {
            Some(p) => (p.zone.clone(), p.x, p.y),
            None => return,
        };
        let eligible: Vec<String> = self.parties[pi]
            .members
            .iter()
            .filter(|m| {
                self.players.get(*m).is_some_and(|p| {
                    p.connected
                        && p.dead_until.is_none()
                        && p.zone == kz
                        && ((p.x - kx).powi(2) + (p.y - ky).powi(2)).sqrt() <= SHARE_RANGE
                })
            })
            .cloned()
            .collect();
        if eligible.len() <= 1 {
            return self.award_exp(killer, base).await;
        }
        let n = eligible.len() as u64;
        let total = base + base * (n - 1) / 10; // ×(1 + 0.1×(n-1))
        let each = (total / n).max(1);
        for m in eligible {
            self.award_exp(&m, each).await;
        }
    }
}
