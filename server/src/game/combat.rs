//! 战斗结算: 普攻/技能施放/经验/怪物 AI(自 game.rs 机械拆出,行为不变)。
use super::*;

impl Game {
    /// 普攻结算：射程/冷却校验 → 扣血 → 飘字广播 → 击杀经验/升级/尸体与重生
    pub(super) async fn handle_attack(&mut self, conn_id: &str, target_id: &str) {
        let now = Instant::now();
        // 攻击者
        let Some((char_id, zone, px, py, level)) = self
            .players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id)
            .map(|(id, p)| (id.clone(), p.zone.clone(), p.x, p.y, p.level))
        else {
            return;
        };
        {
            let p = self.players.get_mut(&char_id).unwrap();
            if now.duration_since(p.last_attack) < PLAYER_ATTACK_CD {
                return;
            }
            p.last_attack = now;
        }
        // 目标怪
        let Some(m) = self
            .monsters
            .iter_mut()
            .find(|m| m.id == target_id && m.zone == zone && m.alive())
        else {
            return;
        };
        let dist = ((m.x - px).powi(2) + (m.y - py).powi(2)).sqrt();
        if dist > PLAYER_ATTACK_RANGE {
            return;
        }
        let mon_id = m.id.clone();
        let dmg = attack_for(level) + self.players[&char_id].equip_attack();
        self.hit_monster(&char_id, &mon_id, dmg).await;
    }

    /// 对怪结算一次伤害: 扣血/飘字广播/击杀 → 尸体+经验。返回是否击杀。
    pub(super) async fn hit_monster(&mut self, char_id: &str, mon_id: &str, dmg: i32) -> bool {
        let now = Instant::now();
        let Some(m) = self
            .monsters
            .iter_mut()
            .find(|m| m.id == mon_id && m.alive())
        else {
            return false;
        };
        m.hp -= dmg;
        let (zone, killed, exp_gain) = (m.zone.clone(), m.hp <= 0, m.exp);
        if killed {
            m.dying_until = Some(now + DYING_TIME);
            m.target = None;
            m.attack_until = None;
            m.pending_hit = None;
        }
        let conns = self.zone_conns(&zone);
        broadcast_to(
            &self.sessions,
            &conns,
            ServerMessage::DamageNumber {
                target_id: mon_id.to_string(),
                amount: dmg,
                is_critical: false,
            },
        )
        .await;
        if killed {
            let (template, boss_name) = self
                .monsters
                .iter()
                .find(|m| m.id == mon_id)
                .map(|m| {
                    (
                        m.template.clone(),
                        (m.boss && m.announce).then(|| m.name.clone()),
                    )
                })
                .unwrap_or_default();
            self.award_exp(char_id, exp_gain).await;
            self.roll_drops(char_id, mon_id).await;
            self.progress_quests(char_id, &template).await;
            if let Some(name) = boss_name {
                let who = self
                    .players
                    .get(char_id)
                    .map(|p| p.character.name.clone())
                    .unwrap_or_default();
                self.broadcast_all(&format!("勇士 {who} 击杀了 {name}！"))
                    .await;
            }
        }
        killed
    }

    /// 技能施放: 等级/MP/冷却/射程校验 → 按类型结算 → SkillEffect 广播
    pub(super) async fn handle_use_skill(
        &mut self,
        conn_id: &str,
        skill_id: &str,
        target_id: Option<String>,
    ) {
        let now = Instant::now();
        // 施法者快照 + 校验
        let Some((char_id, zone, px, py, level)) = self
            .players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id)
            .map(|(id, p)| (id.clone(), p.zone.clone(), p.x, p.y, p.level))
        else {
            return;
        };
        let class = self.players[&char_id].character.class;
        let skills = skills_for(class);
        let Some(def) = skills.iter().find(|s| s.id == skill_id) else {
            return;
        };
        {
            let p = &self.players[&char_id];
            if p.level < def.level {
                return;
            }
            if p.cooldowns.get(&def.id).is_some_and(|&t| now < t) {
                return;
            }
            if p.mp < def.mp {
                send_to(
                    &self.sessions,
                    conn_id,
                    ServerMessage::Notification {
                        message: "魔法值不足".into(),
                        notification_type: "warn".into(),
                    },
                )
                .await;
                return;
            }
        }
        // 施法中心: 自我施法 = 自身; 否则目标怪 (射程校验)。
        // 目标/射程无效在扣费之前拒绝 —— 白扣蓝进冷却是 bug
        let center = if def.self_cast {
            (px, py)
        } else {
            let Some(m) = self.monsters.iter().find(|m| {
                Some(m.id.as_str()) == target_id.as_deref() && m.zone == zone && m.alive()
            }) else {
                return;
            };
            let d = ((m.x - px).powi(2) + (m.y - py).powi(2)).sqrt();
            if d > def.range {
                return;
            }
            (m.x, m.y)
        };
        // 校验全过 → 扣蓝 + 进冷却; 顺带积累修炼度 (每次施放 +1, 到量升级)
        let (skill_level, leveled_to) = {
            let p = self.players.get_mut(&char_id).unwrap();
            p.mp -= def.mp;
            p.cooldowns.insert(def.id.clone(), now + def.cd());
            let sp = p.skills.entry(def.id.clone()).or_default();
            let mut leveled = None;
            if sp.level < def.max_level {
                sp.train += 1;
                if sp.train >= def.train_need(sp.level) {
                    sp.train = 0;
                    sp.level += 1;
                    leveled = Some(sp.level);
                }
            }
            (sp.level, leveled)
        };
        // 结算: 技能按职业吃对应攻击系 —— 战士=物理, 法师=魔法, 道士=道术
        // (skills_for 已按职业过滤, 技能职业即角色职业)
        let equip_bonus = match class {
            protocol::CharacterClass::Warrior => self.players[&char_id].equip_attack(),
            protocol::CharacterClass::Mage => self.players[&char_id].equip_magic(),
            protocol::CharacterClass::Taoist => self.players[&char_id].equip_spirit(),
        };
        let dmg_base = attack_for(level) + equip_bonus;
        // 修炼加成: 每级 +level_bonus (烈火 3 级 ×1.9, 私服 4/5 级更凶)
        let train_mult = 1.0 + def.level_bonus * skill_level as f64;
        let mut hit_ids: Vec<(String, i32)> = Vec::new();
        match def.kind {
            SkillKind::Damage(mult) => {
                if let Some(tid) = &target_id {
                    hit_ids.push((tid.clone(), (dmg_base as f64 * mult * train_mult) as i32));
                }
            }
            SkillKind::Aoe { radius, mult } => {
                for m in self
                    .monsters
                    .iter()
                    .filter(|m| m.zone == zone && m.alive())
                    .filter(|m| {
                        ((m.x - center.0).powi(2) + (m.y - center.1).powi(2)).sqrt() <= radius
                    })
                {
                    hit_ids.push((m.id.clone(), (dmg_base as f64 * mult * train_mult) as i32));
                }
            }
            SkillKind::Heal => {
                let p = self.players.get_mut(&char_id).unwrap();
                let amount = ((30 + level as i32 * 5) as f64 * train_mult) as i32;
                p.hp = (p.hp + amount).min(p.max_hp);
            }
        }
        // 特效广播 (客户端按 skill_id 播放)
        let conns = self.zone_conns(&zone);
        broadcast_to(
            &self.sessions,
            &conns,
            ServerMessage::SkillEffect {
                caster_id: char_id.clone(),
                skill_id: def.id.to_string(),
                position: Position {
                    x: center.0,
                    y: center.1,
                },
                targets: hit_ids.iter().map(|(id, _)| id.clone()).collect(),
                level: skill_level,
                fx_lib: def.fx_lib,
                fx_base: def.fx_base,
                fx_frames: def.fx_frames,
                anim: def.anim.clone(),
                stages: def.stages.max(1),
                src: Some(Position { x: px, y: py }),
            },
        )
        .await;
        // 延迟结算: 与客户端特效编排同步 (起手播完、弹体到达才掉血)
        if !hit_ids.is_empty() {
            let stages = def.stages.max(1);
            // 单技能标准文件: 起手固定 @0 (无起手帧则时长为 0)
            let cast_dur = if stages >= 2 {
                fx_block_len(def.fx_lib, fxl::CAST as i64) as f64 * 0.1
            } else {
                0.0
            };
            let flight = if stages >= 3 {
                ((center.0 - px).powi(2) + (center.1 - py).powi(2)).sqrt() / fxl::FLY_SPEED
            } else {
                0.0
            };
            let delay = cast_dur + flight;
            if delay < 0.05 {
                for (mon_id, dmg) in hit_ids {
                    self.hit_monster(&char_id, &mon_id, dmg).await;
                }
            } else {
                self.pending_hits.push((
                    Instant::now() + Duration::from_secs_f64(delay),
                    char_id.clone(),
                    hit_ids,
                ));
            }
        }
        // 升级: 通知 + 重发技能表; 修炼度落库 (每次施放都存, 掉线不丢练度)
        if let Some(new_lv) = leveled_to {
            let conn = self.players[&char_id].conn_id.clone();
            send_to(
                &self.sessions,
                &conn,
                ServerMessage::Notification {
                    message: format!("《{}》修炼至 {} 级!", def.name, new_lv),
                    notification_type: "levelup".into(),
                },
            )
            .await;
        }
        self.send_skill_list(&char_id).await;
        if let Some(p) = self.players.get(&char_id) {
            let _ = self.db.save_skill_progress(&char_id, &p.skills).await;
        }
        self.send_player_status(&char_id).await;
    }

    /// 经验入账 + 升级结算
    pub(super) async fn award_exp(&mut self, char_id: &str, gain: u64) {
        let Some(p) = self.players.get_mut(char_id) else {
            return;
        };
        p.exp += gain;
        let mut leveled = false;
        while p.exp >= exp_required(p.level) {
            p.exp -= exp_required(p.level);
            p.level += 1;
            p.recalc();
            p.hp = p.max_hp;
            p.mp = p.max_mp;
            leveled = true;
        }
        let (conn, level) = (p.conn_id.clone(), p.level);
        send_to(
            &self.sessions,
            &conn,
            ServerMessage::Notification {
                message: format!("获得经验 {gain}"),
                notification_type: "exp".into(),
            },
        )
        .await;
        if leveled {
            send_to(
                &self.sessions,
                &conn,
                ServerMessage::Notification {
                    message: format!("升级! 现在 Lv.{level}"),
                    notification_type: "levelup".into(),
                },
            )
            .await;
        }
        self.send_player_status(char_id).await;
    }

    /// 怪物 AI: 0.5s 决策 (仇恨/追击/拴绳/游荡) + 每 tick 连续移动。
    /// 返回攻击动画到点的命中结算 (角色 id, 伤害)。
    pub(super) fn monster_ai(&mut self, now: Instant) -> Vec<(String, i32)> {
        let mut hits = Vec::new();
        let dt = TICK.as_secs_f64();
        // 决策所需的玩家位置快照 (避免与 monsters 可变借用冲突)
        let players: Vec<(String, String, f64, f64)> = self
            .players
            .iter()
            .filter(|(_, p)| p.connected)
            .map(|(id, p)| (id.clone(), p.zone.clone(), p.x, p.y))
            .collect();
        // NPC 位置快照 (同样避免借用冲突)
        let npc_pos: Vec<(String, f64, f64)> = data()
            .npcs
            .iter()
            .filter(|n| n.enabled)
            .map(|n| (n.map.clone(), n.x, n.y))
            .collect();
        let mut rolls: Vec<f64> = Vec::with_capacity(self.monsters.len());
        for _ in 0..self.monsters.len() {
            let r = self.rand01();
            rolls.push(r);
        }
        for (mi, m) in self.monsters.iter_mut().enumerate() {
            if !m.alive() {
                continue;
            }
            let Some(zone) = self.zones.get(&m.zone) else {
                continue;
            };
            // 攻击动画期间原地不动; 到点结算命中 (目标仍在范围内才算打中)
            if let Some(t) = m.attack_until {
                if now < t {
                    continue;
                }
                m.attack_until = None;
                if let Some(target) = m.pending_hit.take() {
                    if let Some((_, _, px, py)) = players.iter().find(|(id, ..)| id == &target) {
                        let d = ((m.x - px).powi(2) + (m.y - py).powi(2)).sqrt();
                        if d <= MONSTER_HIT_RANGE {
                            hits.push((target, m.damage));
                        }
                    }
                }
            }
            if now >= m.next_decide {
                m.next_decide = now + Duration::from_millis(500);
                let home_d = ((m.x - m.home.0).powi(2) + (m.y - m.home.1).powi(2)).sqrt();
                let nearest = players
                    .iter()
                    .filter(|(_, z, _, _)| z == &m.zone)
                    .map(|(id, _, px, py)| {
                        let d = ((m.x - px).powi(2) + (m.y - py).powi(2)).sqrt();
                        (d, id.clone(), *px, *py)
                    })
                    .min_by(|a, b| a.0.total_cmp(&b.0));
                if home_d > LEASH_RANGE {
                    // 拉离过远 → 脱战回家
                    m.chasing = false;
                    m.target = Some(m.home);
                } else if let Some((d, pid, px, py)) =
                    nearest.filter(|(d, ..)| *d < AGGRO_RANGE && !m.passive)
                {
                    if d < ATTACK_RANGE {
                        m.dir = dir8_from(px - m.x, py - m.y) as u8;
                        m.target = None;
                        m.chasing = false;
                        if now >= m.next_attack {
                            m.attack_until = Some(now + ATTACK_ANIM);
                            m.next_attack = now + ATTACK_COOLDOWN;
                            m.pending_hit = Some(pid);
                        }
                    } else {
                        m.chasing = true;
                        m.target = Some((px, py));
                    }
                } else if m.target.is_none() && rolls[mi] < 0.15 {
                    // 游荡: 家附近随机踱步 (同一随机数派生角度, 低质量即可)
                    let ang = rolls[mi] * 41.0;
                    let want = (m.home.0 + ang.sin() * m.roam, m.home.1 + ang.cos() * m.roam);
                    m.chasing = false;
                    m.target = Some(want);
                }
            }
            // 连续移动 (滑行走 sim, 与玩家同源)
            if let Some((tx, ty)) = m.target {
                let (dx, dy) = (tx - m.x, ty - m.y);
                let dist = (dx * dx + dy * dy).sqrt();
                if dist < 0.15 {
                    m.target = None;
                    continue;
                }
                let speed = if m.chasing { CHASE_SPEED } else { WANDER_SPEED };
                let step = (speed * dt).min(dist);
                // 8 向量化移动 (原版语义): 位移严格沿朝向轴。转向贪心懒惰:
                // 沿当前朝向仍有进展就不换向 (走完该轴分量才重选主导方向),
                // 一段斜线最多转向一两次 — 避免逐 tick 重算导致的高频摆头
                let cur = (m.dir as usize) % 8;
                let (cvx, cvy) = sim::DIR8[cur];
                let dir = if dx * cvx + dy * cvy > 1e-6 {
                    cur
                } else {
                    dir8_from(dx, dy)
                };
                m.dir = dir as u8;
                let (vx, vy) = sim::DIR8[dir];
                let adv = step.min(dx * vx + dy * vy);
                let (sx, sy) = (vx * adv, vy * adv);
                // 怪也不许穿人穿摊主 (怪之间不互挡, 见 blockers_for 注记)
                let blockers: Vec<(f64, f64)> = players
                    .iter()
                    .filter(|(_, z, _, _)| z == &m.zone)
                    .map(|(_, _, x, y)| (*x, *y))
                    .chain(
                        npc_pos
                            .iter()
                            .filter(|(z, _, _)| z == &m.zone)
                            .map(|(_, x, y)| (*x, *y)),
                    )
                    .collect();
                let (nx, ny) =
                    sim::resolve_move(&zone.walk, (m.x, m.y), (sx, sy), BODY_RADIUS, &blockers);
                if (nx - m.x).abs() < 1e-9 && (ny - m.y).abs() < 1e-9 {
                    m.target = None; // 完全卡死则放弃本次目标
                } else {
                    m.x = nx;
                    m.y = ny;
                }
            }
        }
        hits
    }

    /// 怪物命中玩家: 扣血/飘字/死亡回城
    pub(super) async fn apply_monster_hits(&mut self, hits: Vec<(String, i32)>) {
        for (char_id, dmg) in hits {
            let Some(p) = self.players.get_mut(&char_id) else {
                continue;
            };
            let dmg = (dmg - p.equip_defense()).max(1);
            p.hp -= dmg;
            let (zone, conn, dead) = (p.zone.clone(), p.conn_id.clone(), p.hp <= 0);
            let conns = self.zone_conns(&zone);
            broadcast_to(
                &self.sessions,
                &conns,
                ServerMessage::DamageNumber {
                    target_id: char_id.clone(),
                    amount: dmg,
                    is_critical: false,
                },
            )
            .await;
            if dead {
                // 死亡: 回本区域出生点满血复活 (惩罚机制后续)
                let spawn = self
                    .zones
                    .get(&zone)
                    .map(|z| z.spawn)
                    .unwrap_or((330.5, 150.5));
                let p = self.players.get_mut(&char_id).unwrap();
                p.hp = p.max_hp;
                p.x = spawn.0;
                p.y = spawn.1;
                send_to(
                    &self.sessions,
                    &conn,
                    ServerMessage::Notification {
                        message: "你死了, 已在出生点复活".into(),
                        notification_type: "death".into(),
                    },
                )
                .await;
                self.send_enter_zone_only(&conn, &char_id, spawn.0, spawn.1)
                    .await;
            }
            self.send_player_status(&char_id).await;
        }
    }
}
