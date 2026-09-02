//! 20Hz 主循环步进(自 game.rs 机械拆出,行为不变)。
use super::*;

impl Game {
    pub(super) async fn tick(&mut self) {
        let now = Instant::now();
        // 技能延迟结算到点 (怪物已死/离场由 hit_monster 自然无效化)
        if self.pending_hits.iter().any(|(at, ..)| now >= *at) {
            let mut due = Vec::new();
            let mut i = 0;
            while i < self.pending_hits.len() {
                if now >= self.pending_hits[i].0 {
                    due.push(self.pending_hits.remove(i));
                } else {
                    i += 1;
                }
            }
            for (_, char_id, hits) in due {
                for (mon_id, dmg) in hits {
                    self.hit_monster(&char_id, &mon_id, dmg).await;
                }
            }
        }
        // 地面物品过期清理 (按区广播变化)
        if self.ground.iter().any(|g| now >= g.expire) {
            let zones: Vec<String> = self
                .ground
                .iter()
                .filter(|g| now >= g.expire)
                .map(|g| g.zone.clone())
                .collect();
            self.ground.retain(|g| now < g.expire);
            for z in zones {
                self.broadcast_ground(&z).await;
            }
        }
        // 停止判定: 200ms 没有移动包即站立
        for p in self.players.values_mut() {
            if p.moving && now.duration_since(p.last_move) > Duration::from_millis(200) {
                p.moving = false;
            }
        }
        // 每 2s 自然回复 HP/MP
        if now.duration_since(self.last_regen) > Duration::from_secs(2) {
            self.last_regen = now;
            let mut changed = Vec::new();
            for (id, p) in self.players.iter_mut() {
                if !p.connected {
                    continue;
                }
                let (hp0, mp0) = (p.hp, p.mp);
                p.hp = (p.hp + (p.max_hp * 3 / 100).max(1)).min(p.max_hp);
                p.mp = (p.mp + (p.max_mp * 8 / 100).max(1)).min(p.max_mp);
                if p.hp != hp0 || p.mp != mp0 {
                    changed.push(id.clone());
                }
            }
            for id in changed {
                self.send_player_status(&id).await;
            }
        }
        // 过窗清理 + 存档
        let expired: Vec<String> = self
            .players
            .iter()
            .filter(|(_, p)| {
                p.disconnected_at
                    .is_some_and(|t| now.duration_since(t) > RECONNECT_WINDOW)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(p) = self.players.remove(&id) {
                let _ = self.db.save_position(&id, &p.zone, p.x, p.y).await;
                let _ = self.db.save_progress(&id, p.level, p.exp).await;
                let _ = self.db.save_items(&id, &p.inventory, &p.equipment).await;
                let _ = self.db.save_quests(&id, &p.quests).await;
                info!("重连窗过期, 存档并移除: {id}");
            }
        }
        if now.duration_since(self.last_save) > SAVE_EVERY {
            self.last_save = now;
            self.save_all().await;
        }
        // 怪物 AI (有玩家在线才跑)
        if !self.players.is_empty() {
            let hits = self.monster_ai(now);
            self.apply_monster_hits(hits).await;
        }
        // 重生落点要避开玩家, 先抄一份位置快照 (下面 monsters 是可变借用)
        let alive_players: Vec<(String, f64, f64)> = self
            .players
            .values()
            .filter(|p| p.connected)
            .map(|p| (p.zone.clone(), p.x, p.y))
            .collect();
        let zones = &self.zones;
        // 怪物生命周期: 死亡动画到点 → 等重生; 重生到点 → 回家满血复活
        for m in self.monsters.iter_mut() {
            if m.dying_until.is_some_and(|t| now >= t) {
                m.dying_until = None;
                m.respawn_at = Some(now + m.respawn);
                // 尸体躺到重生时刻 (上限 CORPSE_CAP), 期间继续广播 die 姿态
                m.corpse_until = Some(now + m.respawn.min(CORPSE_CAP));
            }
            if m.corpse_until.is_some_and(|t| now >= t) {
                m.corpse_until = None;
            }
            if m.respawn_at.is_some_and(|t| now >= t) {
                m.respawn_at = None;
                m.hp = m.max_hp;
                // 老家被人占着就挪开一点刷, 免得一出生就和玩家重叠
                let here: Vec<(f64, f64)> = alive_players
                    .iter()
                    .filter(|(z, _, _)| z == &m.zone)
                    .map(|(_, x, y)| (*x, *y))
                    .collect();
                let (hx, hy) = match zones.get(&m.zone) {
                    Some(z) => nearest_free(&z.walk, m.home.0, m.home.1, &here),
                    None => m.home,
                };
                m.x = hx;
                m.y = hy;
                m.dir = 4;
                m.removed_sent = false;
            }
        }
        // 20Hz 广播, 按区域分组 (只看得见同区域的人)
        if self.players.is_empty() {
            return;
        }
        let ts = now_ms();
        for zone_id in self.zones.keys() {
            let mut entities: Vec<EntityUpdate> = self
                .players
                .iter()
                .filter(|(_, p)| &p.zone == zone_id)
                .map(|(id, p)| EntityUpdate {
                    id: id.clone(),
                    position: Some(Position { x: p.x, y: p.y }),
                    hp: None,
                    animation: Some(
                        match (p.moving, p.running) {
                            (true, true) => "run",
                            (true, false) => "walk",
                            _ => "stand",
                        }
                        .into(),
                    ),
                    dir: None,
                    removed: None,
                    // 外观: 衣甲缺省 0 (基础模), 武器无则不带
                    armour: Some(p.equipment.get("armor").map(|i| i.shape).unwrap_or(0)),
                    weapon: p.equipment.get("weapon").map(|i| i.shape),
                    image: None, // 玩家走 CArmour/CWeapon, 不用怪物图库
                    image_base: None,
                })
                .collect();
            if entities.is_empty() {
                continue;
            }
            entities.extend(
                self.monsters
                    .iter_mut()
                    .filter(|m| &m.zone == zone_id)
                    .filter_map(|m| {
                        // 等重生: removed 只广播一次
                        if m.respawn_at.is_some() && m.corpse_until.is_none() {
                            if m.removed_sent {
                                return None;
                            }
                            m.removed_sent = true;
                            return Some(EntityUpdate {
                                id: m.id.clone(),
                                position: None,
                                hp: None,
                                animation: None,
                                dir: None,
                                removed: Some(true),
                                armour: None,
                                weapon: None,
                                image: None,
                                image_base: None,
                            });
                        }
                        let anim = if m.dying_until.is_some() || m.corpse_until.is_some() {
                            "die"
                        } else if m.attack_until.is_some() {
                            "attack"
                        } else if m.target.is_some() {
                            "walk"
                        } else {
                            "stand"
                        };
                        Some(EntityUpdate {
                            id: m.id.clone(),
                            position: Some(Position { x: m.x, y: m.y }),
                            hp: Some(m.hp.max(0)),
                            animation: Some(anim.into()),
                            dir: Some(m.dir),
                            removed: None,
                            armour: None,
                            weapon: None,
                            image: Some(m.image),
                            image_base: Some(m.image_base),
                        })
                    }),
            );
            let targets: Vec<String> = self
                .players
                .values()
                .filter(|p| p.connected && &p.zone == zone_id)
                .map(|p| p.conn_id.clone())
                .collect();
            broadcast_to(
                &self.sessions,
                &targets,
                ServerMessage::StateUpdate {
                    entities,
                    timestamp: ts,
                },
            )
            .await;
        }
    }
}
