//! 玩家面对面交易: 邀请 → 接受 → 双托管区放物/设金币 → 双确认 → 原子交换。
//!
//! 防骗与防复制要点:
//! - 物品放入交易即移出背包进托管区 (单一数据源), 取消原路退回
//! - 任一方改动内容 (增减物品/改金币), 双方确认状态立即重置
//! - 成交前校验双方在线/同区/近距/金币足/背包容量, 通过才交换并立即落库

use super::*;

/// 交易距离上限 (格): 发起与成交时都要求双方贴近
const TRADE_RANGE: f64 = 7.0;
/// 单方托管区容量
pub(super) const TRADE_SLOTS: usize = 6;

pub(super) struct Trade {
    /// 双方 char_id ([0]=发起方)
    pub(super) who: [String; 2],
    pub(super) items: [Vec<protocol::ItemInfo>; 2],
    pub(super) gold: [u64; 2],
    pub(super) ok: [bool; 2],
}

impl Game {
    fn trade_idx(&self, char_id: &str) -> Option<(usize, usize)> {
        self.trades.iter().enumerate().find_map(|(ti, t)| {
            t.who
                .iter()
                .position(|w| w == char_id)
                .map(|side| (ti, side))
        })
    }

    /// 双方是否仍满足交易条件 (在线/同区/近距)
    fn trade_pair_ok(&self, a: &str, b: &str) -> bool {
        let (Some(pa), Some(pb)) = (self.players.get(a), self.players.get(b)) else {
            return false;
        };
        pa.connected
            && pb.connected
            && pa.zone == pb.zone
            && ((pa.x - pb.x).powi(2) + (pa.y - pb.y).powi(2)).sqrt() <= TRADE_RANGE
    }

    pub(super) async fn handle_trade_request(&mut self, conn_id: &str, target: &str) {
        let Some(me) = self.char_by_conn(conn_id) else {
            return;
        };
        if me == target {
            return;
        }
        if self.players.get(target).is_none() {
            return self.notify(conn_id, "对方不在线").await;
        }
        if !self.trade_pair_ok(&me, target) {
            return self.notify(conn_id, "距离太远, 无法交易").await;
        }
        if self.trade_idx(&me).is_some() || self.trade_idx(target).is_some() {
            return self.notify(conn_id, "对方或你正在交易中").await;
        }
        let my_name = self
            .players
            .get(&me)
            .map(|p| p.character.name.clone())
            .unwrap_or_default();
        // 一人只挂一份待回应邀请, 新邀请覆盖旧的
        self.trade_invites.insert(target.to_string(), me.clone());
        if let Some(t) = self.players.get(target) {
            send_to(
                &self.sessions,
                &t.conn_id,
                ServerMessage::TradeInvited {
                    from_id: me,
                    from_name: my_name,
                },
            )
            .await;
        }
        self.notify(conn_id, "交易请求已发出").await;
    }

    pub(super) async fn handle_trade_accept(&mut self, conn_id: &str) {
        let Some(me) = self.char_by_conn(conn_id) else {
            return;
        };
        let Some(from) = self.trade_invites.remove(&me) else {
            return;
        };
        if !self.trade_pair_ok(&from, &me)
            || self.trade_idx(&from).is_some()
            || self.trade_idx(&me).is_some()
        {
            return self.notify(conn_id, "交易发起方已离开").await;
        }
        self.trades.push(Trade {
            who: [from, me],
            items: [Vec::new(), Vec::new()],
            gold: [0, 0],
            ok: [false, false],
        });
        self.sync_trade(self.trades.len() - 1).await;
    }

    pub(super) async fn handle_trade_decline(&mut self, conn_id: &str) {
        let Some(me) = self.char_by_conn(conn_id) else {
            return;
        };
        if let Some(from) = self.trade_invites.remove(&me) {
            if let Some(p) = self.players.get(&from) {
                let conn = p.conn_id.clone();
                self.notify(&conn, "对方拒绝了交易").await;
            }
        }
    }

    pub(super) async fn handle_trade_place(&mut self, conn_id: &str, item_id: &str) {
        let Some(me) = self.char_by_conn(conn_id) else {
            return;
        };
        let Some((ti, side)) = self.trade_idx(&me) else {
            return;
        };
        if self.trades[ti].items[side].len() >= TRADE_SLOTS {
            return self.notify(conn_id, "交易栏已满").await;
        }
        let Some(p) = self.players.get_mut(&me) else {
            return;
        };
        let Some(pos) = p.inventory.iter().position(|i| i.id == item_id) else {
            return;
        };
        let item = p.inventory.remove(pos);
        let t = &mut self.trades[ti];
        t.items[side].push(item);
        t.ok = [false, false]; // 内容变动: 双方重新确认
        self.send_inventory(&me).await;
        self.sync_trade(ti).await;
    }

    pub(super) async fn handle_trade_take(&mut self, conn_id: &str, item_id: &str) {
        let Some(me) = self.char_by_conn(conn_id) else {
            return;
        };
        let Some((ti, side)) = self.trade_idx(&me) else {
            return;
        };
        let t = &mut self.trades[ti];
        let Some(pos) = t.items[side].iter().position(|i| i.id == item_id) else {
            return;
        };
        let item = t.items[side].remove(pos);
        t.ok = [false, false];
        if let Some(p) = self.players.get_mut(&me) {
            p.inventory.push(item);
        }
        self.send_inventory(&me).await;
        self.sync_trade(ti).await;
    }

    pub(super) async fn handle_trade_set_gold(&mut self, conn_id: &str, gold: i64) {
        let Some(me) = self.char_by_conn(conn_id) else {
            return;
        };
        let Some((ti, side)) = self.trade_idx(&me) else {
            return;
        };
        let want = gold.max(0) as u64;
        let owned = self.players.get(&me).map(|p| p.gold).unwrap_or(0);
        if want > owned {
            return self.notify(conn_id, "金币不足").await;
        }
        let t = &mut self.trades[ti];
        if t.gold[side] != want {
            t.gold[side] = want;
            t.ok = [false, false];
        }
        self.sync_trade(ti).await;
    }

    pub(super) async fn handle_trade_confirm(&mut self, conn_id: &str) {
        let Some(me) = self.char_by_conn(conn_id) else {
            return;
        };
        let Some((ti, side)) = self.trade_idx(&me) else {
            return;
        };
        self.trades[ti].ok[side] = true;
        if !(self.trades[ti].ok[0] && self.trades[ti].ok[1]) {
            return self.sync_trade(ti).await;
        }
        // 双确认: 成交校验
        let [a, b] = [self.trades[ti].who[0].clone(), self.trades[ti].who[1].clone()];
        if !self.trade_pair_ok(&a, &b) {
            return self.cancel_trade_of(&a, "双方距离太远, 交易取消").await;
        }
        let t = &self.trades[ti];
        let fail = {
            let pa = &self.players[&a];
            let pb = &self.players[&b];
            if pa.gold < t.gold[0] || pb.gold < t.gold[1] {
                Some("金币不足, 请重新确认")
            } else if pa.inventory.len() + t.items[1].len() > MAX_INVENTORY
                || pb.inventory.len() + t.items[0].len() > MAX_INVENTORY
            {
                Some("背包空间不足, 请先清理")
            } else {
                None
            }
        };
        if let Some(msg) = fail {
            self.trades[ti].ok = [false, false];
            for w in [a, b] {
                if let Some(p) = self.players.get(&w) {
                    let conn = p.conn_id.clone();
                    self.notify(&conn, msg).await;
                }
            }
            return self.sync_trade(ti).await;
        }
        // 原子交换 (内存) → 立即落库双方
        let t = self.trades.remove(ti);
        for (side, me) in t.who.iter().enumerate() {
            let other = 1 - side;
            if let Some(p) = self.players.get_mut(me) {
                p.gold = p.gold - t.gold[side] + t.gold[other];
                p.inventory.extend(t.items[other].iter().cloned());
            }
        }
        for w in &t.who {
            self.persist_items_gold(w).await;
            self.send_inventory(w).await;
            if let Some(p) = self.players.get(w) {
                let (conn, gold) = (p.conn_id.clone(), p.gold);
                self.send_gold(&conn, gold).await;
                send_to(
                    &self.sessions,
                    &conn,
                    ServerMessage::TradeClosed {
                        reason: "交易成功".into(),
                        done: true,
                    },
                )
                .await;
            }
        }
    }

    /// 取消 char_id 在途的交易 (主动取消/掉线/切区共用): 托管物原路退回。
    /// 退回不做容量检查 —— 物品不丢是底线, 超出的格子等腾出空间后自然可见。
    pub(super) async fn cancel_trade_of(&mut self, char_id: &str, reason: &str) {
        self.trade_invites.remove(char_id);
        self.trade_invites.retain(|_, from| from != char_id);
        let Some((ti, _)) = self.trade_idx(char_id) else {
            return;
        };
        let t = self.trades.remove(ti);
        for (side, who) in t.who.iter().enumerate() {
            if let Some(p) = self.players.get_mut(who) {
                p.inventory.extend(t.items[side].iter().cloned());
            }
            self.send_inventory(who).await;
            if let Some(p) = self.players.get(who) {
                let conn = p.conn_id.clone();
                send_to(
                    &self.sessions,
                    &conn,
                    ServerMessage::TradeClosed {
                        reason: reason.into(),
                        done: false,
                    },
                )
                .await;
            }
        }
    }

    /// 双方各自视角的全量状态推送
    async fn sync_trade(&self, ti: usize) {
        let Some(t) = self.trades.get(ti) else {
            return;
        };
        for side in 0..2 {
            let other = 1 - side;
            let Some(p) = self.players.get(&t.who[side]) else {
                continue;
            };
            let partner = self
                .players
                .get(&t.who[other])
                .map(|o| o.character.name.clone())
                .unwrap_or_default();
            send_to(
                &self.sessions,
                &p.conn_id,
                ServerMessage::TradeState {
                    partner,
                    my_items: t.items[side].clone(),
                    their_items: t.items[other].clone(),
                    my_gold: t.gold[side],
                    their_gold: t.gold[other],
                    my_ok: t.ok[side],
                    their_ok: t.ok[other],
                },
            )
            .await;
        }
    }

    /// 背包+装备+金币一并落库 (交易/仓库共用)
    pub(super) async fn persist_items_gold(&self, char_id: &str) {
        if let Some(p) = self.players.get(char_id) {
            let _ = self
                .db
                .save_items(char_id, &p.inventory, &p.equipment)
                .await;
            let _ = self.db.save_gold(char_id, p.gold).await;
        }
    }
}
