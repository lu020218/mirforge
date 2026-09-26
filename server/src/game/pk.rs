//! PK 体系: 攻击模式 / 善恶名色 / PvP 结算 / 玩家死亡复活 / 安全区。
//!
//! 规则 (经典传奇式):
//! - 和平模式无法把玩家作为攻击目标; 全体模式可攻击任何玩家 (Ctrl+H 切换)
//! - 打中白名玩家 → 灰名 60 秒 (再打刷新); 杀死白名 → PK 值 +100;
//!   PK 值 ≥100 红名; 在线每 2 分钟 -1; 杀灰名/红名不加
//! - 攻击者或目标任一方在安全区内 → PvP 一律拒绝
//! - 玩家死亡: 尸体 5 秒 → 回本区出生点满血复活 (v1 不掉经验不掉装)

use super::*;

/// 灰名持续秒数 (打中白名刷新)
const GREY_SECS: u64 = 60;
/// 红名阈值
pub(super) const PK_RED: u32 = 100;
/// 杀死白名玩家的 PK 值
const PK_KILL: u32 = 100;
/// PK 值在线衰减间隔 (每次 -1)
const PK_DECAY: Duration = Duration::from_secs(120);
/// 死亡到复活的尸体时间
const DEATH_SECS: f64 = 5.0;
/// 玩家中毒跳伤间隔 (与怪物同款)
const PLAYER_POISON_TICK: Duration = Duration::from_secs(2);

impl Game {
    /// 名色: 灰名 (打人临时) > 红名 (PK 值) > 白名
    pub(super) fn pk_color(p: &PlayerState, now: Instant) -> &'static str {
        if p.grey_until.is_some_and(|t| now < t) {
            "grey"
        } else if p.pk_points >= PK_RED {
            "red"
        } else {
            "white"
        }
    }

    /// 坐标是否落在该区域的安全区内
    pub(super) fn in_safe_zone(&self, zone_id: &str, x: f64, y: f64) -> bool {
        self.zones.get(zone_id).is_some_and(|z| {
            z.sidecar
                .safe_zones
                .iter()
                .any(|(sx, sy, r)| ((x - sx).powi(2) + (y - sy).powi(2)).sqrt() <= *r)
        })
    }

    pub(super) async fn handle_set_pk_mode(&mut self, conn_id: &str, mode: &str) {
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        let all = mode == "all";
        if let Some(p) = self.players.get_mut(&char_id) {
            p.pk_all = all;
        }
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::PkMode { mode: mode.into() },
        )
        .await;
        self.notify(
            conn_id,
            if all {
                "已切换到全体模式: 可攻击任何玩家"
            } else {
                "已切换到和平模式"
            },
        )
        .await;
    }

    /// PvP 前置校验: 模式/安全区/目标有效。通过返回目标位置 (施法中心用)。
    /// verbose = 是否把拒绝原因发给攻击者 (范围技能扫到的目标静默跳过)
    pub(super) async fn pvp_check(
        &self,
        attacker: &str,
        target: &str,
        range: f64,
        verbose: bool,
    ) -> Option<(f64, f64)> {
        let res: Result<(f64, f64), &'static str> = (|| {
            if attacker == target {
                return Err("");
            }
            let a = self.players.get(attacker).ok_or("")?;
            let t = self.players.get(target).ok_or("目标无效")?;
            if !a.pk_all {
                return Err("和平模式下无法攻击玩家 (Ctrl+H 切换)");
            }
            if self.same_party(attacker, target) {
                return Err("不能攻击队友");
            }
            if !t.connected || t.dead_until.is_some() || t.zone != a.zone {
                return Err("目标无效");
            }
            let d = ((t.x - a.x).powi(2) + (t.y - a.y).powi(2)).sqrt();
            if d > range {
                return Err("目标太远了");
            }
            if self.in_safe_zone(&a.zone, a.x, a.y) || self.in_safe_zone(&t.zone, t.x, t.y) {
                return Err("安全区内禁止攻击");
            }
            Ok((t.x, t.y))
        })();
        match res {
            Ok(pos) => Some(pos),
            Err(msg) => {
                if verbose && !msg.is_empty() {
                    if let Some(a) = self.players.get(attacker) {
                        let conn = a.conn_id.clone();
                        self.notify(&conn, msg).await;
                    }
                }
                None
            }
        }
    }

    /// PvP 出手命中: 标灰 (打白名) + 伤害结算
    pub(super) async fn hit_player_pvp(&mut self, attacker: &str, target: &str, dmg: i32) {
        let now = Instant::now();
        let victim_white = self
            .players
            .get(target)
            .is_some_and(|t| Self::pk_color(t, now) == "white");
        if victim_white {
            if let Some(a) = self.players.get_mut(attacker) {
                a.grey_until = Some(now + Duration::from_secs(GREY_SECS));
            }
        }
        self.damage_player(Some(attacker.to_string()), target, dmg)
            .await;
    }

    /// 玩家受伤统一入口 (怪物攻击 source=None, PvP/毒 source=攻击者)。
    /// 扣血/飘字, 打死进入死亡状态 (5 秒后 tick 复活)。
    pub(super) async fn damage_player(&mut self, source: Option<String>, target: &str, raw: i32) {
        let now = Instant::now();
        // 击杀 PK 值按受害者受击前名色判定
        let victim_white = self
            .players
            .get(target)
            .is_some_and(|t| Self::pk_color(t, now) == "white");
        let Some(p) = self.players.get_mut(target) else {
            return;
        };
        if p.dead_until.is_some() {
            return; // 尸体不再吃伤害
        }
        let dmg = (raw - p.equip_defense()).max(1);
        p.hp -= dmg;
        let (zone, conn, dead) = (p.zone.clone(), p.conn_id.clone(), p.hp <= 0);
        let conns = self.zone_conns(&zone);
        broadcast_to(
            &self.sessions,
            &conns,
            ServerMessage::DamageNumber {
                target_id: target.to_string(),
                amount: dmg,
                is_critical: false,
            },
        )
        .await;
        if !dead {
            self.wear_armor(target).await; // 被击防具损耗 (致死那下不磨, 尸体免伤)
        }
        if dead {
            if let Some(p) = self.players.get_mut(target) {
                p.hp = 0;
                p.moving = false;
                p.dead_until = Some(now + Duration::from_secs_f64(DEATH_SECS));
                p.poison = None;
            }
            self.cancel_trade_of(target, "对方已死亡, 交易取消").await;
            self.notify(&conn, "你死了, 稍后在出生点复活").await;
            // 击杀者善恶结算: 杀白名 +PK 值, 可能转红
            if let Some(killer) = source {
                let mut turned_red = false;
                let mut killer_conn = None;
                if victim_white {
                    if let Some(k) = self.players.get_mut(&killer) {
                        let was_red = k.pk_points >= PK_RED;
                        k.pk_points += PK_KILL;
                        turned_red = !was_red && k.pk_points >= PK_RED;
                        killer_conn = Some(k.conn_id.clone());
                        let pts = k.pk_points;
                        let _ = self.db.save_pk(&killer, pts).await;
                    }
                }
                if let Some(kc) = killer_conn {
                    if turned_red {
                        self.notify(&kc, "你已恶名昭著 (红名)!").await;
                    } else if victim_white {
                        self.notify(&kc, "你杀死了善良玩家, PK 值 +100").await;
                    }
                }
            }
        }
        self.send_player_status(target).await;
    }

    /// tick: 到点复活 (回本区出生点满血)
    pub(super) async fn process_revives(&mut self, now: Instant) {
        let due: Vec<String> = self
            .players
            .iter()
            .filter(|(_, p)| p.dead_until.is_some_and(|t| now >= t))
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            let Some(p) = self.players.get(&id) else {
                continue;
            };
            let zone = p.zone.clone();
            let conn = p.conn_id.clone();
            let spawn = self
                .zones
                .get(&zone)
                .map(|z| z.spawn)
                .unwrap_or((330.5, 150.5));
            if let Some(p) = self.players.get_mut(&id) {
                p.dead_until = None;
                p.hp = p.max_hp;
                p.x = spawn.0;
                p.y = spawn.1;
            }
            self.send_enter_zone_only(&conn, &id, spawn.0, spawn.1).await;
            self.send_player_status(&id).await;
        }
    }

    /// tick: PK 值在线衰减 (每 2 分钟全员 -1)
    pub(super) async fn decay_pk(&mut self, now: Instant) {
        if now.duration_since(self.last_pk_decay) < PK_DECAY {
            return;
        }
        self.last_pk_decay = now;
        let mut dirty = Vec::new();
        for (id, p) in self.players.iter_mut() {
            if p.pk_points > 0 && p.connected {
                p.pk_points -= 1;
                dirty.push((id.clone(), p.pk_points));
            }
        }
        for (id, pts) in dirty {
            let _ = self.db.save_pk(&id, pts).await;
        }
    }

    /// tick: 玩家中毒跳伤结算 (延迟上毒 → 每 2 秒一跳)
    pub(super) async fn settle_player_poisons(&mut self, now: Instant) {
        // 延迟上毒到点
        let due: Vec<(String, String, i32, f64)> = {
            let mut out = Vec::new();
            self.pending_player_poisons.retain(|(at, by, target, dmg, secs)| {
                if now >= *at {
                    out.push((by.clone(), target.clone(), *dmg, *secs));
                    false
                } else {
                    true
                }
            });
            out
        };
        for (by, target, dmg, secs) in due {
            if let Some(p) = self.players.get_mut(&target) {
                if p.dead_until.is_none() {
                    p.poison = Some((
                        now + Duration::from_secs_f64(secs),
                        dmg,
                        now, // 立刻掉第一跳
                        by,
                    ));
                }
            }
        }
        // 跳伤
        let ticks: Vec<(String, String, i32)> = self
            .players
            .iter_mut()
            .filter_map(|(id, p)| {
                let (until, dmg, next, by) = p.poison.clone()?;
                if now >= until {
                    p.poison = None;
                    return None;
                }
                if now >= next {
                    if let Some(t) = p.poison.as_mut() {
                        t.2 = now + PLAYER_POISON_TICK;
                    }
                    return Some((by, id.clone(), dmg));
                }
                None
            })
            .collect();
        for (by, target, dmg) in ticks {
            self.damage_player(Some(by), &target, dmg).await;
        }
    }
}
