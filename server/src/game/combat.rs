//! 战斗结算: 普攻/技能施放/经验/怪物 AI(自 game.rs 机械拆出,行为不变)。
use super::*;

/// AI 输出: 玩家受击 / 宠物打怪 (归属主人) / 宠物被打
pub(super) struct AiOut {
    pub(super) player_hits: Vec<(String, i32)>,
    /// (主人 char_id, 宠物 id, 敌怪 id, 伤害) — 归属记主人, 仇恨记宠物
    pub(super) pet_attacks: Vec<(String, String, String, i32)>,
    /// (宠物 id, 攻击者怪 id, 伤害) — 宠物记仇优先反打
    pub(super) pet_taken: Vec<(String, String, i32)>,
}

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
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            if now.duration_since(p.last_attack) < PLAYER_ATTACK_CD {
                return;
            }
            p.last_attack = now;
        }
        // 目标怪
        let Some(m) = self
            .monsters
            .iter_mut()
            .find(|m| m.id == target_id && m.zone == zone && m.alive() && m.owner.is_none())
        else {
            return;
        };
        let dist = ((m.x - px).powi(2) + (m.y - py).powi(2)).sqrt();
        if dist > PLAYER_ATTACK_RANGE {
            return;
        }
        let mon_id = m.id.clone();
        let Some(equip) = self.players.get(&char_id).map(|p| p.equip_attack()) else {
            return;
        };
        let dmg = attack_for(level) + equip;
        self.hit_monster(&char_id, &mon_id, dmg).await;
    }

    /// 对怪结算一次伤害: 扣血/飘字广播/击杀 → 尸体+经验。返回是否击杀。
    pub(super) async fn hit_monster(&mut self, char_id: &str, mon_id: &str, dmg: i32) -> bool {
        self.hit_monster_inner(char_id, mon_id, dmg, true, Some(char_id))
            .await
    }

    /// struck=false: 不触发受击顿帧 (毒跳伤 — 经典绿毒掉血不顿);
    /// aggro_source: 仇恨记谁 (宠物代主人出手时记宠物; None = 不改仇恨)
    pub(super) async fn hit_monster_inner(
        &mut self,
        char_id: &str,
        mon_id: &str,
        dmg: i32,
        struck: bool,
        aggro_source: Option<&str>,
    ) -> bool {
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
        } else if struck {
            // 受击硬直: 顿帧 + 受击姿态 (毒跳伤 struck=false 跳过)
            m.struck_until = Some(now + STRUCK_ANIM);
        }
        if !killed {
            if let Some(src) = aggro_source {
                // 记仇: 被动怪凭它反击, 主动怪凭它锁定
                m.aggro_target = Some(src.to_string());
            }
        }
        // 宝宝击杀: 怪物经验同额喂给宝宝 (主人经验照旧, 见下方结算)
        if killed {
            if let Some(src) = aggro_source {
                if src != char_id {
                    self.grant_pet_exp(src, exp_gain).await;
                }
            }
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
        let Some(class) = self.players.get(&char_id).map(|p| p.character.class) else {
            return;
        };
        let skills = skills_for(class);
        let Some(def) = skills.iter().find(|s| s.id == skill_id) else {
            return;
        };
        {
            let Some(p) = self.players.get(&char_id) else {
                return;
            };
            if p.level < def.level {
                let msg = format!("等级不足, 《{}》需 Lv {}", def.name, def.level);
                send_to(
                    &self.sessions,
                    conn_id,
                    ServerMessage::Notification {
                        message: msg,
                        notification_type: "warn".into(),
                    },
                )
                .await;
                return;
            }
            // 冷却中不提示 (高频按键会刷屏, 面板有冷却显示)
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
        // 召唤: 模板必须存在 (扣蓝前拒绝, 配置错误不白扣)
        if let SkillKind::Summon { ref template, .. } = def.kind {
            if !data().monsters.iter().any(|m| m.id == *template) {
                self.notify(conn_id, "召唤模板未配置 (后台「怪物设置」先建)")
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
                Some(m.id.as_str()) == target_id.as_deref()
                    && m.zone == zone
                    && m.alive()
                    && m.owner.is_none()
            }) else {
                self.notify(conn_id, "目标无效").await;
                return;
            };
            let d = ((m.x - px).powi(2) + (m.y - py).powi(2)).sqrt();
            if d > def.range {
                self.notify(conn_id, "目标太远了").await;
                return;
            }
            (m.x, m.y)
        };
        // 校验全过 → 扣蓝 + 进冷却; 顺带积累修炼度 (每次施放 +1, 到量升级)
        let (skill_level, leveled_to) = {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
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
        let equip_bonus = match self.players.get(&char_id) {
            Some(p) => match class {
                protocol::CharacterClass::Warrior => p.equip_attack(),
                protocol::CharacterClass::Mage => p.equip_magic(),
                protocol::CharacterClass::Taoist => p.equip_spirit(),
            },
            None => return,
        };
        let dmg_base = attack_for(level) + equip_bonus;
        // 修炼加成: 每级 +level_bonus (烈火 3 级 ×1.9, 私服 4/5 级更凶)
        let train_mult = 1.0 + def.level_bonus * skill_level as f64;
        let mut hit_ids: Vec<(String, i32)> = Vec::new();
        let mut dot_target: Option<(String, i32, f64)> = None;
        // 召唤: 烟雾特效播在骷髅出生点 (素材语义: 宠物从烟雾中现身);
        // 治愈: 特效播在受疗者身上 — 共用特效落点覆盖
        let mut summon_fx_pos: Option<(f64, f64)> = None;
        // 非伤害类的特效目标 (治愈受疗者), 只进 SkillEffect.targets
        let mut extra_targets: Vec<String> = Vec::new();
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
                    .filter(|m| m.zone == zone && m.alive() && m.owner.is_none())
                    .filter(|m| {
                        ((m.x - center.0).powi(2) + (m.y - center.1).powi(2)).sqrt() <= radius
                    })
                {
                    hit_ids.push((m.id.clone(), (dmg_base as f64 * mult * train_mult) as i32));
                }
            }
            SkillKind::Heal => {
                // 经典治愈术: 可奶其他玩家/自己或他人的宝宝; 无目标或目标
                // 无效 (超程/离区/已死) 一律回落治自己, 不白扣蓝
                let amount = ((30 + level as i32 * 5) as f64 * train_mult) as i32;
                let heal_range = def.range.max(6.0);
                let mut healed: Option<(f64, f64, String)> = None;
                if let Some(tid) = target_id.as_deref().filter(|t| *t != char_id) {
                    if let Some(t) = self.players.get_mut(tid) {
                        let d = ((t.x - px).powi(2) + (t.y - py).powi(2)).sqrt();
                        if t.zone == zone && t.connected && d <= heal_range {
                            t.hp = (t.hp + amount).min(t.max_hp);
                            healed = Some((t.x, t.y, tid.to_string()));
                        }
                    } else if let Some(m) = self
                        .monsters
                        .iter_mut()
                        .find(|m| m.id == tid && m.zone == zone && m.owner.is_some() && m.alive())
                    {
                        let d = ((m.x - px).powi(2) + (m.y - py).powi(2)).sqrt();
                        if d <= heal_range {
                            m.hp = (m.hp + amount).min(m.max_hp);
                            healed = Some((m.x, m.y, m.id.clone()));
                        }
                    }
                }
                match healed {
                    Some((hx, hy, hid)) => {
                        summon_fx_pos = Some((hx, hy));
                        extra_targets.push(hid.clone());
                        // 受疗者是玩家: 即刻推状态 (血条实时回)
                        if self.players.contains_key(&hid) {
                            self.send_player_status(&hid).await;
                        }
                    }
                    None => {
                        if let Some(p) = self.players.get_mut(&char_id) {
                            p.hp = (p.hp + amount).min(p.max_hp);
                        }
                    }
                }
            }
            SkillKind::Dot { tick_mult, secs } => {
                // 命中不打直伤, 到点上毒 (走与命中特效同步的延迟通道)
                if let Some(tid) = &target_id {
                    let tick_dmg = (dmg_base as f64 * tick_mult * train_mult).max(1.0) as i32;
                    dot_target = Some((tid.clone(), tick_dmg, secs));
                }
            }
            SkillKind::Summon {
                ref template,
                count,
                secs,
            } => {
                // 重复施放 = 换新宠 (先消散旧的)
                self.despawn_pets(&char_id).await;
                let spawned = self
                    .spawn_pets(
                        &char_id,
                        &zone,
                        (px, py),
                        template,
                        count,
                        secs,
                        skill_level,
                        train_mult,
                    )
                    .await;
                if let Some(pos) = spawned {
                    summon_fx_pos = Some(pos);
                }
            }
            SkillKind::Charge { mult, stun_secs } => {
                // 冲锋: 朝目标方向进入 dash 态, 位移/撞击在 tick 里逐步推进
                // (center 即目标点, 上方已做射程校验 → 目标必在冲锋距离内)
                let (dx, dy) = (center.0 - px, center.1 - py);
                let len = (dx * dx + dy * dy).sqrt();
                if len > 0.05 {
                    let dmg = (dmg_base as f64 * mult * train_mult) as i32;
                    if let Some(p) = self.players.get_mut(&char_id) {
                        p.dash = Some(DashState {
                            dir: (dx / len, dy / len),
                            remaining: def.range,
                            dmg,
                            stun_secs,
                            skill_id: def.id.clone(),
                            fx: def.fx.clone(),
                            fx_base: def.fx_base,
                            fx_frames: def.fx_frames,
                            skill_level,
                        });
                        p.moving = false;
                    }
                }
            }
        }
        // 特效广播 (客户端按 kind 分流: 冲锋起手包起跟随拖尾,
        // 撞击时刻另发一条命中包)
        let kind_name = match &def.kind {
            SkillKind::Damage(_) => "damage",
            SkillKind::Aoe { .. } => "aoe",
            SkillKind::Heal => "heal",
            SkillKind::Dot { .. } => "dot",
            SkillKind::Charge { .. } => "charge",
            SkillKind::Summon { .. } => "summon",
        };
        let conns = self.zone_conns(&zone);
        broadcast_to(
            &self.sessions,
            &conns,
            ServerMessage::SkillEffect {
                caster_id: char_id.clone(),
                skill_id: def.id.to_string(),
                kind: kind_name.into(),
                position: {
                    let (fx_x, fx_y) = summon_fx_pos.unwrap_or(center);
                    Position { x: fx_x, y: fx_y }
                },
                targets: hit_ids
                    .iter()
                    .map(|(id, _)| id.clone())
                    .chain(dot_target.iter().map(|(id, _, _)| id.clone()))
                    .chain(extra_targets.iter().cloned())
                    .collect(),
                level: skill_level,
                fx: def.fx.clone(),
                fx_base: def.fx_base,
                fx_frames: def.fx_frames,
                anim: def.anim.clone(),
                stages: def.stages.max(1),
                src: Some(Position { x: px, y: py }),
            },
        )
        .await;
        // 延迟上毒: 与命中特效同步 (起手播完才上毒)
        if let Some((tid, tick_dmg, secs)) = dot_target {
            let stages = def.stages.max(1);
            let cast_dur = if stages >= 2 {
                fx_block_len(&def.fx, fxl::CAST as i64) as f64 * 0.1
            } else {
                0.0
            };
            self.pending_poisons.push((
                Instant::now() + Duration::from_secs_f64(cast_dur),
                char_id.clone(),
                tid,
                tick_dmg,
                secs,
            ));
        }
        // 延迟结算: 与客户端特效编排同步 (起手播完、弹体到达才掉血)
        if !hit_ids.is_empty() {
            let stages = def.stages.max(1);
            // 单技能标准文件: 起手固定 @0 (无起手帧则时长为 0)
            let cast_dur = if stages >= 2 {
                fx_block_len(&def.fx, fxl::CAST as i64) as f64 * 0.1
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
            if let Some(conn) = self.players.get(&char_id).map(|p| p.conn_id.clone()) {
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

    /// AI 决策与移动。敌怪索敌玩家与宠物; 宠物 (owner Some) 跟随主人、
    /// 攻击附近敌怪, 击杀归属主人。
    pub(super) fn monster_ai(&mut self, now: Instant) -> AiOut {
        let mut out = AiOut {
            player_hits: Vec::new(),
            pet_attacks: Vec::new(),
            pet_taken: Vec::new(),
        };
        let dt = TICK.as_secs_f64();
        // 决策所需的玩家位置快照 (避免与 monsters 可变借用冲突)
        let players: Vec<(String, String, f64, f64)> = self
            .players
            .iter()
            .filter(|(_, p)| p.connected)
            .map(|(id, p)| (id.clone(), p.zone.clone(), p.x, p.y))
            .collect();
        // 怪物位置快照 (宠物索敌 / 敌怪打宠物 都要跨条目读)
        let mon_snap: Vec<(String, String, f64, f64, bool)> = self
            .monsters
            .iter()
            .filter(|m| m.alive())
            .map(|m| (m.id.clone(), m.zone.clone(), m.x, m.y, m.owner.is_some()))
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
            // 僵直: 不移动/不攻击/不索敌 (统一状态系统)
            if m.stunned(now) {
                continue;
            }
            let is_pet = m.owner.is_some();
            // 攻击动画期间原地不动; 到点结算命中 (目标仍在范围内才算打中)
            if let Some(t) = m.attack_until {
                if now < t {
                    continue;
                }
                m.attack_until = None;
                if let Some(target) = m.pending_hit.take() {
                    if is_pet {
                        // 宠物打怪: 归属主人 (经验/掉落/任务), 仇恨记宠物自身
                        if let Some((_, _, tx, ty, _)) =
                            mon_snap.iter().find(|(id, ..)| id == &target)
                        {
                            let d = ((m.x - tx).powi(2) + (m.y - ty).powi(2)).sqrt();
                            if d <= MONSTER_HIT_RANGE {
                                if let Some(owner) = &m.owner {
                                    out.pet_attacks.push((
                                        owner.clone(),
                                        m.id.clone(),
                                        target,
                                        m.damage,
                                    ));
                                }
                            }
                        }
                    } else if let Some((_, _, px, py)) =
                        players.iter().find(|(id, ..)| id == &target)
                    {
                        let d = ((m.x - px).powi(2) + (m.y - py).powi(2)).sqrt();
                        if d <= MONSTER_HIT_RANGE {
                            out.player_hits.push((target, m.damage));
                        }
                    } else if let Some((_, _, tx, ty, _)) =
                        mon_snap.iter().find(|(id, ..)| id == &target)
                    {
                        // 敌怪打宠物 (宠物记仇优先反打)
                        let d = ((m.x - tx).powi(2) + (m.y - ty).powi(2)).sqrt();
                        if d <= MONSTER_HIT_RANGE {
                            out.pet_taken.push((target, m.id.clone(), m.damage));
                        }
                    }
                }
            }
            // 受击硬直: 顿帧 (决策与移动暂停; 已出手的攻击结算不受影响)
            if m.struck_until.is_some_and(|t| now < t) {
                continue;
            }
            if now >= m.next_decide {
                m.next_decide = now + Duration::from_millis(500);
                if is_pet {
                    // ── 宠物决策: 主人为锚, 敌怪为目标 ──
                    let owner_pos = m.owner.as_ref().and_then(|o| {
                        players
                            .iter()
                            .find(|(id, z, _, _)| id == o && z == &m.zone)
                            .map(|(_, _, x, y)| (*x, *y))
                    });
                    let Some((ox, oy)) = owner_pos else {
                        continue; // 主人不在本区/离线: tick 清理负责消散
                    };
                    let owner_d = ((m.x - ox).powi(2) + (m.y - oy).powi(2)).sqrt();
                    // 仇恨优先: 被哪只怪打就先打回哪只; 失效即清
                    let aggro_enemy = m.aggro_target.as_ref().and_then(|a| {
                        mon_snap
                            .iter()
                            .find(|(id, z, _, _, pet)| id == a && !pet && z == &m.zone)
                            .map(|(id, _, x, y, _)| {
                                let d = ((m.x - x).powi(2) + (m.y - y).powi(2)).sqrt();
                                (d, id.clone(), *x, *y)
                            })
                    });
                    if aggro_enemy.is_none() {
                        m.aggro_target = None;
                    }
                    let nearest_enemy = mon_snap
                        .iter()
                        .filter(|(id, z, _, _, pet)| !pet && z == &m.zone && id != &m.id)
                        .map(|(id, _, x, y, _)| {
                            let d = ((m.x - x).powi(2) + (m.y - y).powi(2)).sqrt();
                            (d, id.clone(), *x, *y)
                        })
                        .min_by(|a, b| a.0.total_cmp(&b.0));
                    if owner_d > LEASH_RANGE {
                        // 离主人太远: 放弃战斗回到主人身边
                        m.chasing = true;
                        m.target = Some((ox, oy));
                        m.aggro_target = None;
                    } else if let Some((d, tid, tx, ty)) =
                        aggro_enemy.or(nearest_enemy.filter(|(d, ..)| *d < AGGRO_RANGE))
                    {
                        if d < ATTACK_RANGE {
                            m.dir = dir8_from(tx - m.x, ty - m.y) as u8;
                            m.target = None;
                            m.chasing = false;
                            if now >= m.next_attack {
                                m.attack_until = Some(now + ATTACK_ANIM);
                                m.next_attack = now + ATTACK_COOLDOWN;
                                m.pending_hit = Some(tid);
                            }
                        } else {
                            m.chasing = true;
                            m.target = Some((tx, ty));
                        }
                    } else if owner_d > 3.0 {
                        // 无敌情: 跟在主人身边
                        m.chasing = true;
                        m.target = Some((ox, oy));
                    } else {
                        m.target = None;
                        m.chasing = false;
                    }
                } else {
                    // ── 敌怪决策: 玩家与宠物都是猎物 ──
                    let home_d = ((m.x - m.home.0).powi(2) + (m.y - m.home.1).powi(2)).sqrt();
                    // 仇恨优先 (被打记仇): 被动怪凭它参战, 主动怪凭它锁定
                    // 不受索敌半径限制 (LEASH 脱战管总闸); 目标失效即清
                    let aggro = m.aggro_target.as_ref().and_then(|a| {
                        players
                            .iter()
                            .filter(|(_, z, _, _)| z == &m.zone)
                            .find(|(id, ..)| id == a)
                            .map(|(id, _, x, y)| (id.clone(), *x, *y))
                            .or_else(|| {
                                mon_snap
                                    .iter()
                                    .find(|(id, z, _, _, pet)| id == a && *pet && z == &m.zone)
                                    .map(|(id, _, x, y, _)| (id.clone(), *x, *y))
                            })
                            .map(|(id, x, y)| {
                                let d = ((m.x - x).powi(2) + (m.y - y).powi(2)).sqrt();
                                (d, id, x, y)
                            })
                    });
                    if aggro.is_none() {
                        m.aggro_target = None;
                    }
                    let nearest = players
                        .iter()
                        .filter(|(_, z, _, _)| z == &m.zone)
                        .map(|(id, _, px, py)| (id.clone(), *px, *py))
                        .chain(
                            mon_snap
                                .iter()
                                .filter(|(_, z, _, _, pet)| *pet && z == &m.zone)
                                .map(|(id, _, x, y, _)| (id.clone(), *x, *y)),
                        )
                        .map(|(id, px, py)| {
                            let d = ((m.x - px).powi(2) + (m.y - py).powi(2)).sqrt();
                            (d, id, px, py)
                        })
                        .min_by(|a, b| a.0.total_cmp(&b.0));
                    if home_d > LEASH_RANGE {
                        // 拉离过远 → 脱战回家 (仇恨一并放下, 被动怪恢复温顺)
                        m.chasing = false;
                        m.target = Some(m.home);
                        m.aggro_target = None;
                    } else if let Some((d, pid, px, py)) =
                        aggro.or(nearest.filter(|(d, ..)| *d < AGGRO_RANGE && !m.passive))
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
        out
    }

    /// 怪物命中玩家: 扣血/飘字/死亡回城
    /// 上毒 (重复施毒刷新时长; 首跳在一个间隔后)
    pub(super) async fn apply_poison(
        &mut self,
        attacker: &str,
        mon_id: &str,
        tick_dmg: i32,
        secs: f64,
    ) {
        let now = Instant::now();
        let mut zone = None;
        if let Some(m) = self
            .monsters
            .iter_mut()
            .find(|m| m.id == mon_id && m.alive())
        {
            m.poison = Some(Poison {
                until: now + Duration::from_secs_f64(secs.max(0.1)),
                next_tick: now + POISON_TICK,
                tick_dmg,
                attacker: attacker.to_string(),
            });
            // 上毒瞬间拉仇恨 (经典施毒拉怪); 之后毒跳不再刷新
            m.aggro_target = Some(attacker.to_string());
            zone = Some((m.zone.clone(), m.id.clone()));
        }
        // 立即广播中毒状态 (客户端变绿)
        if let Some((zone, id)) = zone {
            self.broadcast_poison(&zone, &id, true).await;
        }
    }

    pub(super) async fn broadcast_poison(&self, zone: &str, mon_id: &str, on: bool) {
        let conns = self.zone_conns(zone);
        broadcast_to(
            &self.sessions,
            &conns,
            ServerMessage::StateUpdate {
                entities: vec![EntityUpdate {
                    id: mon_id.to_string(),
                    position: None,
                    hp: None,
                    animation: None,
                    dir: None,
                    removed: None,
                    armour: None,
                    weapon: None,
                    image: None,
                    image_base: None,
                    poisoned: Some(on),
                    statuses: None,
                    owner: None,
                }],
                timestamp: now_ms(),
            },
        )
        .await;
    }

    /// 毒伤步进 (tick 驱动): 到点跳伤, 到期/死亡清毒
    pub(super) async fn tick_poisons(&mut self, now: Instant) {
        let mut ticks: Vec<(String, String, i32)> = Vec::new();
        let mut expired: Vec<(String, String)> = Vec::new();
        for m in self.monsters.iter_mut() {
            if m.poison.is_none() {
                continue;
            }
            let alive = m.alive();
            let p = m.poison.as_mut().unwrap();
            if !alive || now >= p.until {
                expired.push((m.zone.clone(), m.id.clone()));
                m.poison = None;
                continue;
            }
            if now >= p.next_tick {
                p.next_tick = now + POISON_TICK;
                ticks.push((p.attacker.clone(), m.id.clone(), p.tick_dmg));
            }
        }
        for (attacker, mon_id, dmg) in ticks {
            self.hit_monster_inner(&attacker, &mon_id, dmg, false, None)
                .await;
        }
        for (zone, id) in expired {
            self.broadcast_poison(&zone, &id, false).await;
        }
    }

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
                if let Some(p) = self.players.get_mut(&char_id) {
                    p.hp = p.max_hp;
                    p.x = spawn.0;
                    p.y = spawn.1;
                }
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

impl super::Game {
    /// 生成宠物: 主人旁落点, 数值/形态随修炼等级; 返回首只出生点
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn spawn_pets(
        &mut self,
        owner: &str,
        zone_id: &str,
        at: (f64, f64),
        template: &str,
        count: u32,
        secs: f64,
        skill_level: u32,
        grow: f64,
    ) -> Option<(f64, f64)> {
        let d = data();
        let t = d.monsters.iter().find(|m| m.id == template)?;
        let zone = self.zones.get(zone_id)?;
        let def = t.clone();
        let hp = (def.hp as f64 * grow) as i32;
        let damage = (def.damage as f64 * grow) as i32;
        // 形态随宝宝等级换装 (1 级 = 首形态; 升级在 grant_pet_exp 里切换)
        let _ = skill_level;
        let image_base = def.base;
        let now = Instant::now();
        let until = (secs > 0.0).then(|| now + Duration::from_secs_f64(secs));
        let mut first = None;
        for i in 0..count.max(1) {
            let ang = i as f64 * 2.4 + 0.8;
            let want = (at.0 + ang.sin() * 1.6, at.1 + ang.cos() * 1.6);
            let (x, y) = nearest_walkable(&zone.walk, want.0, want.1);
            if first.is_none() {
                first = Some((x, y));
            }
            self.monsters.push(Monster {
                id: format!("pet_{}_{}", owner, i),
                template: def.id.clone(),
                name: def.name.clone(),
                boss: false,
                respawn: Duration::from_secs(3600),
                announce: false,
                image: def.image,
                image_base,
                zone: zone_id.to_string(),
                home: (x, y),
                roam: 1.0,
                x,
                y,
                dir: 4,
                target: None,
                chasing: false,
                attack_until: None,
                pending_hit: None,
                next_attack: now,
                next_decide: now,
                passive: false,
                hp,
                max_hp: hp,
                damage,
                exp: 0,
                drops: Vec::new(),
                dying_until: None,
                corpse_until: None,
                respawn_at: None,
                removed_sent: false,
                poison: None,
                statuses: HashMap::new(),
                statuses_sent: false,
                struck_until: None,
                aggro_target: None,
                owner: Some(owner.to_string()),
                pet_level: 1,
                pet_exp: 0,
                summon_until: until,
            });
        }
        first
    }

    /// 宠物受击: 扣血/飘字/致死进入死亡动画 (不给攻击方任何归属收益);
    /// 宠物记仇优先反打攻击者
    pub(super) async fn damage_pet(&mut self, pet_id: &str, attacker: &str, dmg: i32) {
        let Some(m) = self.monsters.iter_mut().find(|m| m.id == pet_id) else {
            return;
        };
        if !m.alive() {
            return;
        }
        m.hp -= dmg;
        m.aggro_target = Some(attacker.to_string());
        let (zone, dead) = (m.zone.clone(), m.hp <= 0);
        if dead {
            m.dying_until = Some(Instant::now() + DYING_TIME);
            m.target = None;
            m.pending_hit = None;
        } else {
            m.struck_until = Some(Instant::now() + STRUCK_ANIM);
        }
        let conns = self.zone_conns(&zone);
        broadcast_to(
            &self.sessions,
            &conns,
            ServerMessage::DamageNumber {
                target_id: pet_id.to_string(),
                amount: dmg,
                is_critical: false,
            },
        )
        .await;
    }

    /// 宝宝吃经验: 满额升级 (最高 7 级) — 数值 ×1.2/级并回满血,
    /// 形态切下一档 (骷髅库 12 形态, 7 级用前 7 档)
    pub(super) async fn grant_pet_exp(&mut self, pet_id: &str, exp: u64) {
        let d = data();
        let Some(m) = self
            .monsters
            .iter_mut()
            .find(|m| m.id == pet_id && m.owner.is_some() && m.alive())
        else {
            return;
        };
        m.pet_exp += exp;
        let mut leveled = false;
        while m.pet_level < PET_MAX_LEVEL {
            let need = PET_EXP_BASE * m.pet_level as u64;
            if m.pet_exp < need {
                break;
            }
            m.pet_exp -= need;
            m.pet_level += 1;
            m.max_hp = (m.max_hp as f64 * PET_LEVEL_GROW) as i32;
            m.damage = (m.damage as f64 * PET_LEVEL_GROW) as i32;
            leveled = true;
        }
        if leveled {
            m.hp = m.max_hp; // 升级回满
            if let Some(t) = d.monsters.iter().find(|t| t.id == m.template) {
                m.image_base = t.base + (m.pet_level - 1).min(11) * 360;
            }
            let (zone, name, lv) = (m.zone.clone(), m.name.clone(), m.pet_level);
            let owner = m.owner.clone();
            // 升级提示给主人
            if let Some(conn) = owner
                .and_then(|o| self.players.get(&o))
                .map(|p| p.conn_id.clone())
            {
                send_to(
                    &self.sessions,
                    &conn,
                    ServerMessage::Notification {
                        message: format!("{name}升到了 {lv} 级!"),
                        notification_type: "info".into(),
                    },
                )
                .await;
            }
            let _ = zone;
        }
    }

    /// 消散某主人的全部宠物 (重复施放/下线/换区/到期)
    pub(super) async fn despawn_pets(&mut self, owner: &str) {
        let gone: Vec<(String, String)> = self
            .monsters
            .iter()
            .filter(|m| m.owner.as_deref() == Some(owner))
            .map(|m| (m.id.clone(), m.zone.clone()))
            .collect();
        if gone.is_empty() {
            return;
        }
        self.monsters.retain(|m| m.owner.as_deref() != Some(owner));
        self.broadcast_removed(&gone).await;
    }

    /// 广播实体消失 (宠物消散用; (id, zone) 列表按区分发)
    pub(super) async fn broadcast_removed(&self, gone: &[(String, String)]) {
        for (id, zone) in gone {
            let conns = self.zone_conns(zone);
            broadcast_to(
                &self.sessions,
                &conns,
                ServerMessage::StateUpdate {
                    entities: vec![EntityUpdate {
                        id: id.clone(),
                        position: None,
                        hp: None,
                        animation: None,
                        dir: None,
                        removed: Some(true),
                        armour: None,
                        weapon: None,
                        image: None,
                        image_base: None,
                        poisoned: None,
                        statuses: None,
                        owner: None,
                    }],
                    timestamp: now_ms(),
                },
            )
            .await;
        }
    }

    /// 冲锋推进: 每拍前进 DASH_SPEED×dt, 撞墙停 / 撞第一个怪结算
    /// (伤害 + 沿冲向击退 1 格 + 僵直) / 走完即止
    pub(super) async fn step_dashes(&mut self, now: Instant) {
        use super::{DASH_HIT_RANGE, DASH_KNOCKBACK, DASH_SPEED};
        let dt = super::TICK.as_secs_f64();
        let ids: Vec<String> = self
            .players
            .iter()
            .filter(|(_, p)| p.dash.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            let Some((zone_id, x, y, dir, remaining)) = self.players.get(&id).and_then(|p| {
                p.dash
                    .as_ref()
                    .map(|d| (p.zone.clone(), p.x, p.y, d.dir, d.remaining))
            }) else {
                continue;
            };
            let Some(zone) = self.zones.get(&zone_id) else {
                if let Some(p) = self.players.get_mut(&id) {
                    p.dash = None;
                }
                continue;
            };
            let step = (DASH_SPEED * dt).min(remaining);
            let (nx, ny) = (x + dir.0 * step, y + dir.1 * step);
            // 撞墙: 就地停
            if !zone.walk.is_walkable_circle(nx, ny, BODY_RADIUS) {
                if let Some(p) = self.players.get_mut(&id) {
                    p.dash = None;
                }
                continue;
            }
            // 撞怪: 结算后停 (路径上第一个活怪, 不限于施放目标)
            let hit = self
                .monsters
                .iter()
                .filter(|m| m.zone == zone_id && m.alive() && m.owner.is_none())
                .map(|m| {
                    let d = ((m.x - nx).powi(2) + (m.y - ny).powi(2)).sqrt();
                    (m.id.clone(), d)
                })
                .filter(|(_, d)| *d <= DASH_HIT_RANGE)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            // 先推进位置
            if let Some(p) = self.players.get_mut(&id) {
                p.x = nx;
                p.y = ny;
                match &mut p.dash {
                    Some(d) => d.remaining -= step,
                    None => continue,
                }
            }
            let Some((mon_id, _)) = hit else {
                // 走完距离自然结束
                if let Some(p) = self.players.get_mut(&id) {
                    if p.dash.as_ref().is_some_and(|d| d.remaining <= 0.01) {
                        p.dash = None;
                    }
                }
                continue;
            };
            // ── 撞击结算 ──
            let Some(dash) = self.players.get_mut(&id).and_then(|p| p.dash.take()) else {
                continue;
            };
            // 击退: 沿冲向 1 格, 落点不可走则原地; 上僵直
            let stun_until = now + Duration::from_secs_f64(dash.stun_secs.max(0.0));
            let mut impact_pos = None;
            if let Some(m) = self.monsters.iter_mut().find(|m| m.id == mon_id) {
                let (kx, ky) = (
                    m.x + dash.dir.0 * DASH_KNOCKBACK,
                    m.y + dash.dir.1 * DASH_KNOCKBACK,
                );
                if zone.walk.is_walkable_circle(kx, ky, BODY_RADIUS) {
                    m.x = kx;
                    m.y = ky;
                }
                m.target = None;
                m.statuses.insert(
                    super::StatusKind::Stun,
                    super::StatusState { until: stun_until },
                );
                impact_pos = Some((m.x, m.y));
            }
            // 撞击特效 (anim 置空: 客户端不重播施放者动作)
            if let Some((ix, iy)) = impact_pos {
                let conns = self.zone_conns(&zone_id);
                broadcast_to(
                    &self.sessions,
                    &conns,
                    ServerMessage::SkillEffect {
                        caster_id: id.clone(),
                        skill_id: dash.skill_id.clone(),
                        kind: "charge".into(),
                        position: Position { x: ix, y: iy },
                        targets: vec![mon_id.clone()],
                        level: dash.skill_level,
                        fx: dash.fx.clone(),
                        fx_base: dash.fx_base,
                        fx_frames: dash.fx_frames,
                        anim: String::new(),
                        stages: 1,
                        src: None,
                    },
                )
                .await;
            }
            self.hit_monster(&id, &mon_id, dash.dmg).await;
        }
    }
}
