//! 怪物物化与配置重刷(自 game.rs 机械拆出,行为不变)。
use super::*;

/// 按刷新点物化一个区域的怪物 (出生位置吸附可走格)
/// `avoid` 是要让开的位置 (在场玩家); 开服时为空
pub(super) fn materialize_monsters(
    zone: &Zone,
    rng: &mut u64,
    avoid: &[(f64, f64)],
) -> Vec<Monster> {
    let mut next = |limit: f64| {
        *rng ^= *rng << 13;
        *rng ^= *rng >> 7;
        *rng ^= *rng << 17;
        (*rng >> 11) as f64 / (1u64 << 53) as f64 * limit
    };
    let now = Instant::now();
    let mut out = Vec::new();
    let d = data();
    for (si, sp) in zone.monster_spawns.iter().enumerate() {
        // 数值以怪物模板为准; 旧边车的内联字段仅在模板缺失时兜底
        let def = d.monsters.iter().find(|m| m.id == sp.template);
        let image = def.map(|m| m.image).unwrap_or(sp.image);
        let image_base = def.map(|m| m.base).unwrap_or(0);
        let (hp, damage, exp) = def
            .map(|m| (m.hp, m.damage, m.exp))
            .unwrap_or((sp.hp, sp.damage, sp.exp));
        let passive = def.map(|m| m.passive).unwrap_or(sp.passive);
        let drops: Vec<DropEntry> = def
            .map(|m| {
                m.drops
                    .iter()
                    .map(|d| DropEntry {
                        item: d.item.clone(),
                        chance: d.chance,
                    })
                    .collect()
            })
            .unwrap_or_else(|| sp.drops.clone());
        for i in 0..sp.count {
            let want = (
                sp.x + next(sp.radius * 2.0) - sp.radius,
                sp.y + next(sp.radius * 2.0) - sp.radius,
            );
            let (x, y) = nearest_free(&zone.walk, want.0, want.1, avoid);
            out.push(Monster {
                id: format!("mon_{}_{}_{}_{}", zone.id, image, si, i),
                template: sp.template.clone(),
                name: def.map(|m| m.name.clone()).unwrap_or_default(),
                boss: false,
                respawn: RESPAWN_TIME,
                announce: false,
                image,
                image_base,
                zone: zone.id.clone(),
                home: (x, y),
                roam: sp.radius,
                x,
                y,
                dir: 4,
                target: None,
                chasing: false,
                attack_until: None,
                pending_hit: None,
                next_attack: now,
                next_decide: now,
                passive,
                hp,
                max_hp: hp,
                damage,
                exp,
                drops: drops.clone(),
                dying_until: None,
                corpse_until: None,
                respawn_at: None,
                removed_sent: false,
                poison: None,
                statuses: std::collections::HashMap::new(),
                statuses_sent: false,
                struck_until: None,
                aggro_target: None,
                owner: None,
                pet_level: 0,
                pet_exp: 0,
                summon_until: None,
            });
        }
    }
    out
}

/// 把本区的 BOSS 配置落成 Monster (与普通刷新点走同一套 AI/生命周期)
pub(super) fn materialize_bosses(zone: &Zone, avoid: &[(f64, f64)]) -> Vec<Monster> {
    let now = Instant::now();
    data()
        .bosses
        .iter()
        .filter(|b| b.enabled && b.map == zone.id)
        .map(|b| {
            let (x, y) = nearest_free(&zone.walk, b.x, b.y, avoid);
            Monster {
                id: format!("boss_{}_{}", zone.id, b.id),
                image_base: 0,
                template: b.id.clone(),
                name: b.name.clone(),
                boss: true,
                respawn: Duration::from_secs(b.respawn_secs.max(1)),
                announce: b.announce,
                image: b.image,
                zone: zone.id.clone(),
                home: (x, y),
                roam: b.roam,
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
                hp: b.hp,
                max_hp: b.hp,
                damage: b.damage,
                exp: b.exp,
                drops: b.drops.clone(),
                dying_until: None,
                corpse_until: None,
                respawn_at: None,
                removed_sent: false,
                poison: None,
                statuses: std::collections::HashMap::new(),
                statuses_sent: false,
                struck_until: None,
                aggro_target: None,
                owner: None,
                pet_level: 0,
                pet_exp: 0,
                summon_until: None,
            }
        })
        .collect()
}

impl Game {
    /// 配置改动后重刷 BOSS: 撤掉旧的, 按新配置重新落地
    ///
    /// 代价是在场的 BOSS 会被重置 (满血回原位), 但配置改了本来就该以新配置
    /// 为准; 普通刷新点不受影响。
    /// 怪物模板配置变更后重刷所有普通刷新点怪 (BOSS 由 refresh_bosses 管)
    pub(super) async fn refresh_spawns(&mut self) {
        let removed: Vec<protocol::EntityUpdate> = self
            .monsters
            .iter()
            .filter(|m| !m.boss)
            .map(|m| protocol::EntityUpdate {
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
                poisoned: None,
                statuses: None,
                owner: None,
            })
            .collect();
        self.monsters.retain(|m| m.boss);
        let mut rng = self.rng | 1;
        for zone in self.zones.values() {
            let here = Self::player_positions(&self.players, &zone.id);
            let fresh = materialize_monsters(zone, &mut rng, &here);
            self.monsters.extend(fresh);
        }
        if !removed.is_empty() {
            let conns: Vec<String> = self
                .players
                .values()
                .filter(|p| p.connected)
                .map(|p| p.conn_id.clone())
                .collect();
            broadcast_to(
                &self.sessions,
                &conns,
                ServerMessage::StateUpdate {
                    entities: removed,
                    timestamp: now_ms(),
                },
            )
            .await;
        }
    }

    pub(super) async fn refresh_bosses(&mut self) {
        let removed: Vec<protocol::EntityUpdate> = self
            .monsters
            .iter()
            .filter(|m| m.boss)
            .map(|m| protocol::EntityUpdate {
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
                poisoned: None,
                statuses: None,
                owner: None,
            })
            .collect();
        self.monsters.retain(|m| !m.boss);
        for zone in self.zones.values() {
            let here = Self::player_positions(&self.players, &zone.id);
            let fresh = materialize_bosses(zone, &here);
            self.monsters.extend(fresh);
        }
        if !removed.is_empty() {
            let conns: Vec<String> = self
                .players
                .values()
                .filter(|p| p.connected)
                .map(|p| p.conn_id.clone())
                .collect();
            broadcast_to(
                &self.sessions,
                &conns,
                ServerMessage::StateUpdate {
                    entities: removed,
                    timestamp: now_ms(),
                },
            )
            .await;
        }
    }

    /// 任务目标校验: 击杀目标必须是某区的刷新点模板或某个 BOSS
    ///
    /// 目标写错不会报错, 只会让任务永远推不动 —— 与商店没有入口是同一类
    /// 静默失败, 所以在保存时拦下。刷新点在区域表里, 故与走格校验放一起。
    pub(super) fn check_quest_targets(&self, quests: &[QuestDef]) -> Vec<String> {
        let d = data();
        let mut known: std::collections::HashSet<&str> = self
            .zones
            .values()
            .flat_map(|z| z.monster_spawns.iter().map(|s| s.template.as_str()))
            .collect();
        known.extend(d.bosses.iter().map(|b| b.id.as_str()));
        let mut errs = Vec::new();
        for q in quests {
            for (target, need) in &q.objectives {
                if !known.contains(target.as_str()) {
                    errs.push(format!(
                        "任务 {} 的击杀目标不存在于任何刷新点或 BOSS: {target}",
                        q.id
                    ));
                }
                if *need == 0 {
                    errs.push(format!("任务 {} 的目标 {target} 数量不能为 0", q.id));
                }
            }
        }
        errs
    }

    /// BOSS 落位校验: 地图已接入 / 坐标可走
    pub(super) fn check_boss_placement(&self, bosses: &[BossDef]) -> Vec<String> {
        let mut errs = Vec::new();
        for b in bosses {
            let Some(z) = self.zones.get(&b.map) else {
                errs.push(format!("BOSS {} 的地图未接入为区域: {}", b.id, b.map));
                continue;
            };
            if !z.walk.is_walkable_circle(b.x, b.y, BODY_RADIUS) {
                errs.push(format!(
                    "BOSS {} 的坐标不可站立 ({:.1},{:.1})",
                    b.id, b.x, b.y
                ));
            }
        }
        errs
    }
}
