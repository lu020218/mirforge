//! 集成测试(自 game.rs 机械拆出)。
use super::*;
use protocol::CharacterClass;

/// 合成测试世界: 20×20 全可走 z1 (带 z2 传送门 + 1 只稻草人) + 10×10 z2
fn test_zones() -> HashMap<String, Zone> {
    let mut zones = HashMap::new();
    zones.insert(
        "z1".to_string(),
        Zone {
            id: "z1".into(),
            name: "测试区".into(),
            walk: WalkGrid::from_cells(40, 40, |_, _| false),
            spawn: (5.5, 5.5),
            portals: vec![Portal {
                x: 15.5,
                y: 5.5,
                to_zone: "z2".into(),
                to_x: None,
                to_y: None,
            }],
            monster_spawns: vec![MonsterSpawn {
                // 故意用不存在于怪物模板表的名字 — 覆盖"旧边车内联数值兜底"路径
                template: "test_dummy".into(),
                image: 5,
                x: 10.0,
                y: 10.0,
                count: 1,
                radius: 0.5,
                passive: false,
                hp: 12,
                damage: 4,
                exp: 20,
                drops: vec![DropEntry {
                    item: "iron_sword".into(),
                    chance: 1.0,
                }],
            }],
            sidecar: ZoneSidecar::default(),
        },
    );
    zones.insert(
        "z2".to_string(),
        Zone {
            id: "z2".into(),
            name: "测试区2".into(),
            walk: WalkGrid::from_cells(10, 10, |_, _| false),
            spawn: (5.5, 5.5),
            portals: vec![],
            monster_spawns: vec![],
            sidecar: ZoneSidecar::default(),
        },
    );
    zones
}

async fn test_game() -> Game {
    let db = Db::open(":memory:").await.unwrap();
    let (gw, _rx) = crate::gateway::Gateway::new();
    Game::new(test_zones(), "z1".into(), db, gw.sessions(), HashMap::new())
}

fn test_player(conn: &str, zone: &str, x: f64, y: f64) -> PlayerState {
    let level = 1;
    PlayerState {
        conn_id: conn.into(),
        account_id: "acc".into(),
        character: CharacterRow {
            id: "char1".into(),
            name: "测试".into(),
            class: CharacterClass::Warrior,
            gender: "male".into(),
            level,
            exp: 0,
            gold: 0,
            zone: zone.into(),
            inventory: Vec::new(),
            equipment: HashMap::new(),
            quests: HashMap::new(),
            skills: HashMap::new(),
            x,
            y,
        },
        zone: zone.into(),
        x,
        y,
        gold: 0,
        moving: false,
        running: false,
        last_move: Instant::now(),
        connected: true,
        disconnected_at: None,
        hp: max_hp_for(level),
        max_hp: max_hp_for(level),
        mp: max_mp_for(level),
        max_mp: max_mp_for(level),
        level,
        exp: 0,
        last_attack: Instant::now() - PLAYER_ATTACK_CD,
        cooldowns: HashMap::new(),
        inventory: Vec::new(),
        equipment: HashMap::new(),
        quests: HashMap::new(),
        skills: HashMap::new(),
        dash: None,
    }
}

#[test]
fn stat_formulas() {
    assert_eq!(max_hp_for(1), 52);
    assert_eq!(max_mp_for(1), 38);
    assert_eq!(attack_for(1), 6);
    assert_eq!(exp_required(1), 100);
    assert!(max_hp_for(10) > max_hp_for(1));
}

#[test]
fn static_tables_consistent() {
    // 三职业各 3 技能, id 全局唯一, 等级门槛非降序
    let mut ids = std::collections::HashSet::new();
    for class in [
        CharacterClass::Warrior,
        CharacterClass::Mage,
        CharacterClass::Taoist,
    ] {
        let skills = skills_for(class);
        assert_eq!(skills.len(), 3);
        let mut last_level = 0;
        for s in skills {
            assert!(ids.insert(s.id.clone()), "技能 id 重复: {}", s.id);
            assert!(s.level >= last_level);
            last_level = s.level;
        }
    }
    // 物品模板唯一 + 槽位合法
    let slots = ["weapon", "armor", "helmet", "necklace", "ring"];
    let mut templates = std::collections::HashSet::new();
    let dref = data();
    for d in &dref.items {
        assert!(
            templates.insert(d.template.clone()),
            "物品模板重复: {}",
            d.template
        );
        assert!(slots.contains(&d.slot.as_str()), "非法槽位: {}", d.slot);
    }
    // 任务前置指向存在的任务
    for q in &dref.quests {
        if let Some(pr) = q.prereq.as_deref() {
            assert!(quest_def(pr).is_some(), "任务 {} 前置 {pr} 不存在", q.id);
        }
    }
}

#[test]
fn parse_teleport_accepts_map_x_y() {
    assert_eq!(
        super::parse_teleport("2.map:300:300.5"),
        Some(("2.map".into(), 300.0, 300.5))
    );
    // 地图名带 . 不影响; 空白容忍
    assert_eq!(
        super::parse_teleport(" 0.map : 12 : 34 "),
        Some(("0.map".into(), 12.0, 34.0))
    );
    // 段数不对 / 非数字 / 空地图名一律拒
    for bad in ["2.map:300", "2.map:300:300:1", "2.map:a:b", ":1:2", ""] {
        assert!(super::parse_teleport(bad).is_none(), "应拒绝: {bad}");
    }
}

#[test]
fn nearest_free_dodges_players() {
    let walk = WalkGrid::from_cells(20, 20, |_, _| false);
    // 老家上正站着人 → 该挪开, 且挪到的位置与人保持碰撞间距
    let here = [(5.5, 5.5)];
    let (x, y) = nearest_free(&walk, 5.5, 5.5, &here);
    let d = ((x - 5.5f64).powi(2) + (y - 5.5f64).powi(2)).sqrt();
    assert!(d >= sim::ENTITY_CLEARANCE, "刷点应让开玩家, 实际距离 {d}");
    assert!(walk.is_walkable_circle(x, y, BODY_RADIUS));
    // 没人时原样返回
    assert_eq!(nearest_free(&walk, 5.5, 5.5, &[]), (5.5, 5.5));
}

#[test]
fn nearest_free_falls_back_when_surrounded() {
    let walk = WalkGrid::from_cells(20, 20, |_, _| false);
    // 周围 12 圈全被占满: 退回"可站立"的原位, 宁可重叠也要刷出来
    let mut here = Vec::new();
    for dy in -13..=13 {
        for dx in -13..=13 {
            here.push((5.5 + dx as f64, 5.5 + dy as f64));
        }
    }
    let (x, y) = nearest_free(&walk, 5.5, 5.5, &here);
    assert!(
        walk.is_walkable_circle(x, y, BODY_RADIUS),
        "兜底也必须站得住"
    );
}

#[test]
fn nearest_free_still_avoids_walls() {
    // 目标格是墙且旁边站着人: 两个条件都要满足
    let walk = WalkGrid::from_cells(10, 10, |x, y| x == 5 && y == 5);
    let here = [(4.5, 5.5)];
    let (x, y) = nearest_free(&walk, 5.5, 5.5, &here);
    assert!(walk.is_walkable_circle(x, y, BODY_RADIUS), "不能落在墙里");
    let d = ((x - 4.5f64).powi(2) + (y - 5.5f64).powi(2)).sqrt();
    assert!(d >= sim::ENTITY_CLEARANCE, "也不能压着人");
}

#[test]
fn nearest_walkable_snaps_out_of_walls() {
    // 中心格阻挡的 5×5
    let walk = WalkGrid::from_cells(5, 5, |x, y| x == 2 && y == 2);
    let (x, y) = nearest_walkable(&walk, 2.5, 2.5);
    assert!(walk.is_walkable_circle(x, y, BODY_RADIUS));
    assert!((x - 2.5).abs() + (y - 2.5).abs() > 0.4, "应吸附到邻格");
    // 本就可走则原样返回
    assert_eq!(nearest_walkable(&walk, 0.5, 0.5), (0.5, 0.5));
}

#[tokio::test]
async fn move_speed_clamped() {
    let mut g = test_game().await;
    let mut p = test_player("c1", "z1", 5.5, 5.5);
    p.last_move = Instant::now() - Duration::from_secs(1);
    g.players.insert("char1".into(), p);
    // 一包要求瞬移 50 格 → 按 0.5s 窗 × 上限 4.08 限幅
    g.handle_move("c1", Position { x: 50.0, y: 0.0 });
    let p = &g.players["char1"];
    assert!(p.x < 5.5 + 2.1, "超速未限幅: {}", p.x);
    assert!(p.x > 5.5 + 1.9);
}

#[tokio::test]
async fn portal_switches_zone() {
    let mut g = test_game().await;
    let mut p = test_player("c1", "z1", 15.2, 5.5);
    p.last_move = Instant::now() - Duration::from_millis(200);
    g.players.insert("char1".into(), p);
    let hit = g.handle_move("c1", Position { x: 0.2, y: 0.0 });
    let (_, _, x, y) = hit.expect("应触发传送门");
    let p = &g.players["char1"];
    assert_eq!(p.zone, "z2");
    assert_eq!((p.x, p.y), (x, y));
    assert_eq!((x, y), (5.5, 5.5)); // 落在 z2 出生点
}

#[tokio::test]
async fn monster_ai_aggro_and_leash() {
    let mut g = test_game().await;
    // 距怪 ~3 格 (仇恨 6 格内, 出手 1.6 格外) → 追击
    g.players
        .insert("char1".into(), test_player("c1", "z1", 13.0, 10.0));
    let now = Instant::now();
    g.monsters[0].next_decide = now;
    g.monster_ai(now);
    assert!(g.monsters[0].chasing, "仇恨范围内玩家应触发追击");
    // 拉离 12 格 → 回家
    g.monsters[0].x = g.monsters[0].home.0 + 15.0;
    g.monsters[0].attack_until = None;
    g.monsters[0].next_decide = now;
    g.monster_ai(now);
    assert!(!g.monsters[0].chasing);
    assert_eq!(g.monsters[0].target, Some(g.monsters[0].home));
    // 被动怪不追击
    g.monsters[0].x = g.monsters[0].home.0;
    g.monsters[0].passive = true;
    g.monsters[0].target = None;
    g.monsters[0].attack_until = None;
    g.monsters[0].next_decide = now;
    g.monster_ai(now);
    assert!(!g.monsters[0].chasing, "被动怪不应追击");
}

#[tokio::test]
async fn monster_moves_along_dir8() {
    let mut g = test_game().await;
    // 玩家在斜向偏 10° 处 (非 8 向轴) → 追击位移仍须严格沿 8 向
    g.players
        .insert("char1".into(), test_player("c1", "z1", 14.0, 10.7));
    let now = Instant::now();
    g.monsters[0].next_decide = now;
    let (x0, y0) = (g.monsters[0].x, g.monsters[0].y);
    g.monster_ai(now);
    let m = &g.monsters[0];
    let (dx, dy) = (m.x - x0, m.y - y0);
    assert!(dx.hypot(dy) > 1e-6, "追击应产生位移");
    let (vx, vy) = sim::DIR8[m.dir as usize];
    // 位移与朝向向量共线 (叉积≈0) 且同向
    assert!(
        (dx * vy - dy * vx).abs() < 1e-9,
        "位移 ({dx},{dy}) 未沿 dir{} 轴",
        m.dir
    );
    assert!(dx * vx + dy * vy > 0.0);
}

#[tokio::test]
async fn dot_ticks_expires_and_kills() {
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 10.0, 10.0));
    let mon_id = g.monsters[0].id.clone();
    let hp0 = g.monsters[0].hp;
    // 上毒: 每跳 3, 持续 10 秒
    g.apply_poison("char1", &mon_id, 3, 10.0).await;
    assert!(g.monsters[0].poison.is_some());
    // 首跳未到点: 不掉血
    g.tick_poisons(Instant::now()).await;
    assert_eq!(g.monsters[0].hp, hp0);
    // 拨快到跳点: 掉一跳
    g.monsters[0].poison.as_mut().unwrap().next_tick = Instant::now();
    g.tick_poisons(Instant::now()).await;
    assert_eq!(g.monsters[0].hp, hp0 - 3, "到点应跳一次毒伤");
    // 重复施毒刷新时长
    let until1 = g.monsters[0].poison.as_ref().unwrap().until;
    g.apply_poison("char1", &mon_id, 3, 10.0).await;
    assert!(g.monsters[0].poison.as_ref().unwrap().until >= until1);
    // 到期清毒
    g.monsters[0].poison.as_mut().unwrap().until = Instant::now();
    g.tick_poisons(Instant::now()).await;
    assert!(g.monsters[0].poison.is_none(), "到期应清毒");
    // 毒可以跳死: 大伤害一跳致死, 击杀归施毒者
    g.apply_poison("char1", &mon_id, 1000, 10.0).await;
    g.monsters[0].poison.as_mut().unwrap().next_tick = Instant::now();
    g.tick_poisons(Instant::now()).await;
    assert!(g.monsters[0].dying_until.is_some(), "毒应能跳死怪");
    assert_eq!(g.players["char1"].exp, 20, "毒杀归施毒者");
    // 死亡后清毒 (下一次步进)
    g.tick_poisons(Instant::now()).await;
    assert!(g.monsters[0].poison.is_none());
}

#[tokio::test]
async fn kill_awards_exp_and_drops() {
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 10.0, 10.0));
    let mon_id = g.monsters[0].id.clone();
    let killed = g.hit_monster("char1", &mon_id, 12).await;
    assert!(killed);
    assert!(g.monsters[0].dying_until.is_some());
    let p = &g.players["char1"];
    assert_eq!(p.exp, 20, "击杀应得 20 经验");
    assert!(p.inventory.is_empty(), "掉落应落地而非直接入包");
    assert_eq!(g.ground.len(), 1, "100% 掉落应落地");
    assert_eq!(g.ground[0].item.template, "iron_sword");
    // 已死怪不能再打
    assert!(!g.hit_monster("char1", &mon_id, 12).await);
}

#[tokio::test]
async fn equip_affects_stats() {
    let mut g = test_game().await;
    let mut p = test_player("c1", "z1", 5.5, 5.5);
    p.inventory.push(make_item("iron_sword").unwrap());
    p.inventory.push(make_item("leather_armor").unwrap());
    let (sword, armor) = (p.inventory[0].id.clone(), p.inventory[1].id.clone());
    g.players.insert("char1".into(), p);
    g.handle_equip("c1", &sword).await;
    g.handle_equip("c1", &armor).await;
    let p = &g.players["char1"];
    assert_eq!(p.equip_attack(), 6);
    assert_eq!(p.equip_defense(), 4);
    assert_eq!(p.max_hp, max_hp_for(1) + 10, "皮甲 +10 上限");
    assert!(p.inventory.is_empty());
    g.handle_unequip("c1", "weapon").await;
    let p = &g.players["char1"];
    assert_eq!(p.equip_attack(), 0);
    assert_eq!(p.inventory.len(), 1);
}

#[tokio::test]
async fn quest_chain_flow() {
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 5.5, 5.5));
    // 前置未完成不可接猎鹿
    g.handle_accept_quest("c1", "hunt_deer").await;
    assert!(!g.players["char1"].quests.contains_key("hunt_deer"));
    // 接猎鸡 → 杀 3 鸡 → 目标未齐不可交付 → 齐了可交付
    g.handle_accept_quest("c1", "hunt_chicken").await;
    assert_eq!(g.players["char1"].quests["hunt_chicken"].state, 1);
    g.progress_quests("char1", "chicken").await;
    g.handle_complete_quest("c1", "hunt_chicken").await;
    assert_eq!(
        g.players["char1"].quests["hunt_chicken"].state, 1,
        "目标未齐不应交付"
    );
    g.progress_quests("char1", "chicken").await;
    g.progress_quests("char1", "chicken").await;
    // 多杀不越界
    g.progress_quests("char1", "chicken").await;
    assert_eq!(g.players["char1"].quests["hunt_chicken"].counts[0], 3);
    g.handle_complete_quest("c1", "hunt_chicken").await;
    let p = &g.players["char1"];
    assert_eq!(p.quests["hunt_chicken"].state, 2);
    assert_eq!(p.exp, 50, "交付应得 50 经验");
    // 前置完成 → 猎鹿可接
    g.handle_accept_quest("c1", "hunt_deer").await;
    assert_eq!(g.players["char1"].quests["hunt_deer"].state, 1);
}

#[tokio::test]
async fn drop_pickup_roundtrip_and_limits() {
    let mut g = test_game().await;
    let mut p = test_player("c1", "z1", 5.5, 5.5);
    p.inventory.push(make_item("wooden_sword").unwrap());
    let item_id = p.inventory[0].id.clone();
    g.players.insert("char1".into(), p);
    // 丢弃 → 落地脚下
    g.handle_drop_item("c1", &item_id).await;
    assert!(g.players["char1"].inventory.is_empty());
    assert_eq!(g.ground.len(), 1);
    let drop_id = g.ground[0].id.clone();
    // 拾取 → 回包
    g.handle_pickup_item("c1", &drop_id).await;
    assert!(g.ground.is_empty());
    assert_eq!(g.players["char1"].inventory.len(), 1);
    // 距离超 2 格拒绝
    let far_id = {
        let item = make_item("copper_ring").unwrap();
        g.spawn_ground("z1", item, 15.5, 15.5);
        g.ground[0].id.clone()
    };
    g.handle_pickup_item("c1", &far_id).await;
    assert_eq!(g.ground.len(), 1, "超距拾取应被拒绝");
    // 背包满拒绝
    g.ground[0].x = 5.5;
    g.ground[0].y = 5.5;
    for _ in 0..MAX_INVENTORY {
        let it = make_item("copper_ring").unwrap();
        g.players.get_mut("char1").unwrap().inventory.push(it);
    }
    g.handle_pickup_item("c1", &far_id).await;
    assert_eq!(g.ground.len(), 1, "背包满应拒绝拾取");
}

#[tokio::test]
async fn chat_targets_by_channel() {
    let mut g = test_game().await;
    // 同区两人 + 异区一人
    g.players
        .insert("char1".into(), test_player("c1", "z1", 5.5, 5.5));
    let mut p2 = test_player("c2", "z1", 6.5, 5.5);
    p2.character.id = "char2".into();
    g.players.insert("char2".into(), p2);
    let mut p3 = test_player("c3", "z2", 5.5, 5.5);
    p3.character.id = "char3".into();
    g.players.insert("char3".into(), p3);

    let mut world = g.chat_targets("c1", protocol::ChatChannel::World);
    world.sort();
    assert_eq!(world, ["c1", "c2", "c3"], "世界频道应全服可见");
    let mut zone = g.chat_targets("c1", protocol::ChatChannel::Zone);
    zone.sort();
    assert_eq!(zone, ["c1", "c2"], "区域频道仅同区可见");
    assert_eq!(
        g.chat_targets("c1", protocol::ChatChannel::Whisper),
        ["c1"],
        "未实现频道仅回显自己"
    );
    // 断线者不可见
    g.players.get_mut("char2").unwrap().connected = false;
    let mut zone = g.chat_targets("c1", protocol::ChatChannel::Zone);
    zone.sort();
    assert_eq!(zone, ["c1"]);
    // 未在场的连接无目标
    assert!(g
        .chat_targets("nobody", protocol::ChatChannel::World)
        .is_empty());
}

#[tokio::test]
async fn charge_dash_hits_knocks_and_stuns() {
    let mut g = test_game().await;
    let mut p = test_player("c1", "z1", 5.0, 10.0);
    // 冲锋态: 朝 +x 冲 8 格, 伤害 5, 僵直 0.8s (怪在 (10,10) 路上)
    p.dash = Some(DashState {
        dir: (1.0, 0.0),
        remaining: 8.0,
        dmg: 5,
        stun_secs: 0.8,
        skill_id: "yeman".into(),
        fx: "4".into(),
        fx_base: 170,
        fx_frames: 8,
        skill_level: 0,
    });
    g.players.insert("char1".into(), p);
    let hp0 = g.monsters[0].hp;
    let (mx0, _my0) = (g.monsters[0].x, g.monsters[0].y);
    let now = Instant::now();
    // 逐拍推进直至撞击 (5→10 约 5 格, 12 格/s × 0.05s = 0.6 格/拍)
    for _ in 0..20 {
        g.step_dashes(now).await;
        if g.players["char1"].dash.is_none() {
            break;
        }
    }
    let p = &g.players["char1"];
    assert!(p.dash.is_none(), "撞击后冲锋应结束");
    assert!(p.x > 5.5 && p.x < 10.5, "应停在怪物面前 (x={})", p.x);
    let m = &g.monsters[0];
    assert_eq!(m.hp, hp0 - 5, "撞击应结算伤害");
    assert!(m.x > mx0 + 0.5, "怪物应沿冲向被击退 (x {mx0}→{})", m.x);
    assert!(m.stunned(now), "怪物应处于僵直");
    // 僵直期间 AI 冻结: 不追不打
    let hits = g.monster_ai(now);
    assert!(hits.is_empty(), "僵直期间不应出手");
    assert!(g.monsters[0].target.is_none(), "僵直期间不应移动");
    // 到期恢复 + 状态清理
    g.monsters[0]
        .statuses
        .insert(StatusKind::Stun, StatusState { until: now });
    assert!(!g.monsters[0].stunned(now), "到期即恢复");
    let active = g.monsters[0].active_statuses(now);
    assert!(active.is_empty(), "过期状态应被清理");
}

#[tokio::test]
async fn charge_dash_stops_at_wall_and_range() {
    let mut g = test_game().await;
    // 朝 -x 冲: 起点 x=2.5, 距墙 2 格, 冲 8 格应撞墙停
    let mut p = test_player("c1", "z1", 2.5, 20.0);
    p.dash = Some(DashState {
        dir: (-1.0, 0.0),
        remaining: 8.0,
        dmg: 5,
        stun_secs: 0.8,
        skill_id: "yeman".into(),
        fx: "4".into(),
        fx_base: 170,
        fx_frames: 8,
        skill_level: 0,
    });
    g.players.insert("char1".into(), p);
    let now = Instant::now();
    for _ in 0..40 {
        g.step_dashes(now).await;
        if g.players["char1"].dash.is_none() {
            break;
        }
    }
    let px = g.players["char1"].x;
    assert!(g.players["char1"].dash.is_none(), "撞墙应结束冲锋");
    assert!(px > 0.0 && px < 2.5, "应停在墙前 (x={px})");
    // 空旷方向: 走满距离自然结束
    let mut p2 = test_player("c2", "z1", 20.5, 30.0);
    p2.dash = Some(DashState {
        dir: (1.0, 0.0),
        remaining: 4.0,
        dmg: 5,
        stun_secs: 0.8,
        skill_id: "yeman".into(),
        fx: "4".into(),
        fx_base: 170,
        fx_frames: 8,
        skill_level: 0,
    });
    g.players.insert("char2".into(), p2);
    for _ in 0..40 {
        g.step_dashes(now).await;
        if g.players["char2"].dash.is_none() {
            break;
        }
    }
    let p2 = &g.players["char2"];
    assert!(p2.dash.is_none(), "走满距离应结束");
    assert!((p2.x - 24.5).abs() < 0.2, "应前进 4 格 (x={})", p2.x);
}
