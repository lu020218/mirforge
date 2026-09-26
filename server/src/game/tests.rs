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

/// set_data 走全局单例, 用它的测试必须串行 (拿住锁到测试结束)
static DATA_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn data_lock() -> std::sync::MutexGuard<'static, ()> {
    DATA_LOCK.lock().unwrap_or_else(|e| e.into_inner())
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
            storage: Vec::new(),
            pk_points: 0,
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
        storage: Vec::new(),
        quests: HashMap::new(),
        skills: HashMap::new(),
        dash: None,
        pk_all: false,
        pk_points: 0,
        grey_until: None,
        dead_until: None,
        poison: None,
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
    let ai = g.monster_ai(now);
    assert!(ai.player_hits.is_empty(), "僵直期间不应出手");
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

#[tokio::test]
async fn summon_spawns_pet_that_fights_for_owner() {
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 12.0, 10.0));
    // 召 1 只 chicken 模板宠物 (内置种子), 出生在主人旁
    let pos = g
        .spawn_pets("char1", "z1", (12.0, 10.0), "chicken", 1, 0.0, 2, 1.6)
        .await;
    assert!(pos.is_some(), "召唤应成功");
    let pet = g.monsters.iter().find(|m| m.owner.is_some()).unwrap();
    assert_eq!(pet.owner.as_deref(), Some("char1"));
    assert_eq!(
        pet.image_base, 0,
        "初始形态固定首档 (形态随宝宝等级, 见 pet_levels_up)"
    );
    let (pet_id, base_hp) = (pet.id.clone(), pet.hp);
    assert!(base_hp > 0);
    // 宠物应索敌攻击附近的稻草人 (10,10): 决策 → 攻击动画 → 到点归属主人
    let now = Instant::now();
    g.monster_ai(now);
    let pet = g.monsters.iter().find(|m| m.id == pet_id).unwrap();
    assert!(
        pet.attack_until.is_some() || pet.target.is_some(),
        "宠物应对敌怪有反应 (攻击或追击)"
    );
    // 拨快攻击动画到点
    if let Some(m) = g.monsters.iter_mut().find(|m| m.id == pet_id) {
        m.attack_until = Some(now);
    }
    let ai = g.monster_ai(now + ATTACK_ANIM);
    if let Some((owner, src_pet, target, _)) = ai.pet_attacks.first() {
        assert_eq!(owner, "char1", "宠物击打归属主人");
        assert_eq!(src_pet, &pet_id, "仇恨来源应是宠物自身");
        assert_ne!(target, &pet_id);
    }
    // 敌怪把宠物纳入猎物: 稻草人的决策目标可为宠物 (宠物更近)
    // 宠物被打掉血: damage_pet 致死进入死亡动画且不重生
    g.damage_pet(&pet_id, "attacker_mon", 99999).await;
    let pet = g.monsters.iter().find(|m| m.id == pet_id).unwrap();
    assert!(pet.dying_until.is_some(), "致死应进入死亡动画");
    assert!(pet.respawn_at.is_none(), "宠物不重生");
}

#[tokio::test]
async fn summon_replace_and_owner_leave_despawns() {
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 12.0, 10.0));
    g.spawn_pets("char1", "z1", (12.0, 10.0), "chicken", 1, 0.0, 0, 1.0)
        .await;
    assert_eq!(g.monsters.iter().filter(|m| m.owner.is_some()).count(), 1);
    // 重复施放 = 先消散旧宠再召新 (业务上由 Summon 分支调 despawn_pets)
    g.despawn_pets("char1").await;
    assert_eq!(
        g.monsters.iter().filter(|m| m.owner.is_some()).count(),
        0,
        "旧宠应消散"
    );
    g.spawn_pets("char1", "z1", (12.0, 10.0), "chicken", 2, 0.0, 0, 1.0)
        .await;
    assert_eq!(
        g.monsters.iter().filter(|m| m.owner.is_some()).count(),
        2,
        "count=2 召两只"
    );
    // 主人换区: tick 清理消散
    g.players.get_mut("char1").unwrap().zone = "z2".into();
    g.tick().await;
    assert_eq!(
        g.monsters.iter().filter(|m| m.owner.is_some()).count(),
        0,
        "主人离区宠物应消散"
    );
}

#[tokio::test]
async fn summon_expires_by_timer() {
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 12.0, 10.0));
    g.spawn_pets("char1", "z1", (12.0, 10.0), "chicken", 1, 5.0, 0, 1.0)
        .await;
    // 拨快到期
    for m in g.monsters.iter_mut().filter(|m| m.owner.is_some()) {
        m.summon_until = Some(Instant::now());
    }
    g.tick().await;
    assert_eq!(
        g.monsters.iter().filter(|m| m.owner.is_some()).count(),
        0,
        "到期应消散"
    );
}

#[tokio::test]
async fn hit_sets_struck_but_poison_tick_does_not() {
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 10.0, 10.0));
    let mon_id = g.monsters[0].id.clone();
    // 直接命中: 进入受击硬直
    g.hit_monster("char1", &mon_id, 3).await;
    let now = Instant::now();
    assert!(
        g.monsters[0].struck_until.is_some_and(|t| t > now),
        "命中应设受击硬直"
    );
    // 硬直期间: 决策与移动暂停 (target 不会被赋新值)
    g.monsters[0].target = None;
    g.monsters[0].next_decide = now;
    g.monster_ai(now);
    assert!(g.monsters[0].target.is_none(), "硬直期间不应移动/索敌");
    // 硬直过后恢复决策
    g.monsters[0].struck_until = Some(now);
    g.monsters[0].next_decide = now;
    g.monster_ai(now);
    assert!(
        g.monsters[0].target.is_some() || g.monsters[0].attack_until.is_some(),
        "硬直结束应恢复行动"
    );
    // 毒跳伤不触发受击
    g.monsters[0].struck_until = None;
    g.apply_poison("char1", &mon_id, 2, 10.0).await;
    g.monsters[0].poison.as_mut().unwrap().next_tick = Instant::now();
    g.tick_poisons(Instant::now()).await;
    assert!(
        g.monsters[0].struck_until.is_none(),
        "毒跳伤不应触发受击顿帧"
    );
}

#[tokio::test]
async fn passive_monster_retaliates_when_hit() {
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 12.0, 10.0));
    // 稻草人改被动: 未被打时不主动索敌
    g.monsters[0].passive = true;
    let now = Instant::now();
    g.monsters[0].next_decide = now;
    g.monster_ai(now);
    assert!(
        g.monsters[0].attack_until.is_none() && !g.monsters[0].chasing,
        "被动怪未被打不应索敌"
    );
    // 被打: 记仇 + 反击 (硬直过后)
    let mon_id = g.monsters[0].id.clone();
    g.hit_monster("char1", &mon_id, 3).await;
    assert_eq!(
        g.monsters[0].aggro_target.as_deref(),
        Some("char1"),
        "被打应记仇"
    );
    g.monsters[0].struck_until = None;
    g.monsters[0].next_decide = now;
    g.monster_ai(now);
    let m = &g.monsters[0];
    assert!(
        m.attack_until.is_some() || m.chasing,
        "被动怪被打后应反击攻击者"
    );
    // 脱战 (拉离老家): 清仇恨恢复温顺
    g.monsters[0].x = g.monsters[0].home.0 + 30.0;
    g.monsters[0].next_decide = now;
    g.monster_ai(now);
    assert!(g.monsters[0].aggro_target.is_none(), "脱战应放下仇恨");
}

#[tokio::test]
async fn monster_retaliates_pet_not_owner() {
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 20.0, 20.0));
    g.spawn_pets("char1", "z1", (11.0, 10.0), "chicken", 1, 0.0, 0, 1.0)
        .await;
    let pet_id = g
        .monsters
        .iter()
        .find(|m| m.owner.is_some())
        .unwrap()
        .id
        .clone();
    let mon_id = g
        .monsters
        .iter()
        .find(|m| m.owner.is_none())
        .unwrap()
        .id
        .clone();
    // 宠物打怪: 归属主人、仇恨记宠物 (模拟 tick 结算路径)
    g.hit_monster_inner("char1", &mon_id, 2, true, Some(&pet_id))
        .await;
    let m = g.monsters.iter().find(|m| m.id == mon_id).unwrap();
    assert_eq!(
        m.aggro_target.as_deref(),
        Some(pet_id.as_str()),
        "怪应记仇宠物而非主人"
    );
    // 怪打宠物: 宠物记仇
    g.damage_pet(&pet_id, &mon_id, 1).await;
    let p = g.monsters.iter().find(|m| m.id == pet_id).unwrap();
    assert_eq!(p.aggro_target.as_deref(), Some(mon_id.as_str()));
}

#[tokio::test]
async fn pet_death_full_lifecycle() {
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 12.0, 10.0));
    g.spawn_pets("char1", "z1", (12.0, 10.0), "chicken", 1, 0.0, 0, 1.0)
        .await;
    let pet_id = g
        .monsters
        .iter()
        .find(|m| m.owner.is_some())
        .unwrap()
        .id
        .clone();
    // 致死: 应进入死亡动画
    g.damage_pet(&pet_id, "mon_x", 99999).await;
    let p = g.monsters.iter().find(|m| m.id == pet_id).unwrap();
    assert!(p.dying_until.is_some(), "致死应设 dying");
    // 死亡动画期间 tick: 宠物应保留 (广播 die 姿态)
    g.tick().await;
    assert!(
        g.monsters.iter().any(|m| m.id == pet_id),
        "dying 中不应被移除"
    );
    // 拨快 dying 到点 → 进尸体
    if let Some(m) = g.monsters.iter_mut().find(|m| m.id == pet_id) {
        m.dying_until = Some(Instant::now() - Duration::from_millis(1));
    }
    g.tick().await;
    let p = g.monsters.iter().find(|m| m.id == pet_id).unwrap();
    assert!(
        p.dying_until.is_none() && p.corpse_until.is_some(),
        "应进尸体期"
    );
    assert!(p.respawn_at.is_none(), "宠物不重生");
    // 拨快尸体到点 → 彻底移除
    if let Some(m) = g.monsters.iter_mut().find(|m| m.id == pet_id) {
        m.corpse_until = Some(Instant::now() - Duration::from_millis(1));
    }
    g.tick().await;
    assert!(
        !g.monsters.iter().any(|m| m.id == pet_id),
        "尸体到期应彻底移除"
    );
}

#[tokio::test]
async fn dead_pet_corpse_does_not_attack() {
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 10.5, 10.0));
    g.spawn_pets("char1", "z1", (10.5, 10.0), "chicken", 1, 0.0, 0, 1.0)
        .await;
    let pet_id = g
        .monsters
        .iter()
        .find(|m| m.owner.is_some())
        .unwrap()
        .id
        .clone();
    // 宠物出手中 (pending_hit 挂着) 被咬死 → 进入尸体期
    let now = Instant::now();
    let mon_id = g
        .monsters
        .iter()
        .find(|m| m.owner.is_none())
        .unwrap()
        .id
        .clone();
    if let Some(m) = g.monsters.iter_mut().find(|m| m.id == pet_id) {
        m.attack_until = Some(now);
        m.pending_hit = Some(mon_id.clone());
    }
    g.damage_pet(&pet_id, &mon_id, 99999).await;
    // 拨快死亡动画 → 尸体期
    if let Some(m) = g.monsters.iter_mut().find(|m| m.id == pet_id) {
        m.dying_until = Some(now - Duration::from_millis(1));
    }
    g.tick().await;
    let p = g.monsters.iter().find(|m| m.id == pet_id).unwrap();
    assert!(p.corpse_until.is_some(), "应在尸体期");
    assert!(!p.alive(), "尸体期不得视为活体 (hp<=0)");
    // 尸体期 AI: 不得产出任何宠物攻击
    let hp0 = g.monsters.iter().find(|m| m.id == mon_id).unwrap().hp;
    let ai = g.monster_ai(Instant::now());
    assert!(ai.pet_attacks.is_empty(), "骨堆不得出手");
    assert_eq!(g.monsters.iter().find(|m| m.id == mon_id).unwrap().hp, hp0);
}

#[tokio::test]
async fn pet_levels_up_by_kills_capped_at_7() {
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 12.0, 10.0));
    g.spawn_pets("char1", "z1", (12.0, 10.0), "chicken", 1, 0.0, 0, 1.0)
        .await;
    let pet_id = g
        .monsters
        .iter()
        .find(|m| m.owner.is_some())
        .unwrap()
        .id
        .clone();
    let (hp1, dmg1) = {
        let p = g.monsters.iter().find(|m| m.id == pet_id).unwrap();
        assert_eq!(p.pet_level, 1);
        assert_eq!(p.image_base, 0, "1 级 = 首形态");
        (p.max_hp, p.damage)
    };
    // 喂 100 经验 → 升 2 级: 数值 ×1.2, 回满, 形态切换
    g.grant_pet_exp(&pet_id, 100).await;
    {
        let p = g.monsters.iter().find(|m| m.id == pet_id).unwrap();
        assert_eq!(p.pet_level, 2, "100 经验应升 2 级");
        assert!(p.max_hp > hp1 && p.damage >= dmg1);
        assert_eq!(p.hp, p.max_hp, "升级应回满");
        assert_eq!(p.image_base, 360, "2 级 = 第二形态");
    }
    // 灌大量经验: 封顶 7 级
    g.grant_pet_exp(&pet_id, 1_000_000).await;
    {
        let p = g.monsters.iter().find(|m| m.id == pet_id).unwrap();
        assert_eq!(p.pet_level, 7, "最高 7 级");
        assert_eq!(p.image_base, 6 * 360, "7 级 = 第七形态");
    }
    // 死亡后重召: 回 1 级
    g.damage_pet(&pet_id, "mon_x", 99999).await;
    g.despawn_pets("char1").await;
    g.spawn_pets("char1", "z1", (12.0, 10.0), "chicken", 1, 0.0, 0, 1.0)
        .await;
    let p = g.monsters.iter().find(|m| m.owner.is_some()).unwrap();
    assert_eq!(p.pet_level, 1, "重召从 1 级开始");
    assert_eq!(p.image_base, 0);
}

#[tokio::test]
async fn heal_targets_players_and_pets() {
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 10.0, 10.0));
    let mut p2 = test_player("c2", "z1", 12.0, 10.0);
    p2.hp = 10;
    g.players.insert("char2".into(), p2);
    g.spawn_pets("char1", "z1", (11.0, 10.0), "chicken", 1, 0.0, 0, 1.0)
        .await;
    let pet_id = g
        .monsters
        .iter()
        .find(|m| m.owner.is_some())
        .unwrap()
        .id
        .clone();
    // 直接走 Heal 结算逻辑: 通过 handle_use_skill 需要技能表 — 改用
    // 与其等价的直接调用路径不可行, 这里用 use_skill 完整链路:
    // 先给角色喂技能等级 (skills_for 取 taoist 表, zhiyu level 门槛 1)
    // 测试世界玩家是 warrior — 直接改类为 Taoist
    g.players.get_mut("char1").unwrap().character.class = CharacterClass::Taoist;
    g.players.get_mut("char1").unwrap().level = 99;
    g.players.get_mut("char1").unwrap().mp = 999;
    // 奶其他玩家
    g.handle_use_skill("c1", "zhiyu", Some("char2".into()))
        .await;
    assert!(
        g.players["char2"].hp > 10,
        "治愈应给其他玩家回血 (hp={})",
        g.players["char2"].hp
    );
    // 奶宝宝 (先扣宠物血)
    if let Some(m) = g.monsters.iter_mut().find(|m| m.id == pet_id) {
        m.hp = 1;
    }
    g.players.get_mut("char1").unwrap().cooldowns.clear();
    g.handle_use_skill("c1", "zhiyu", Some(pet_id.clone()))
        .await;
    let pet_hp = g.monsters.iter().find(|m| m.id == pet_id).unwrap().hp;
    assert!(pet_hp > 1, "治愈应给宝宝回血 (hp={pet_hp})");
    // 无目标: 回落治自己
    g.players.get_mut("char1").unwrap().hp = 5;
    g.players.get_mut("char1").unwrap().cooldowns.clear();
    g.handle_use_skill("c1", "zhiyu", None).await;
    assert!(g.players["char1"].hp > 5, "无目标应治自己");
    // 敌怪不可被治疗: 目标无效回落自己
    let mon_id = g
        .monsters
        .iter()
        .find(|m| m.owner.is_none())
        .unwrap()
        .id
        .clone();
    let mon_hp0 = g.monsters.iter().find(|m| m.id == mon_id).unwrap().hp;
    g.players.get_mut("char1").unwrap().hp = 5;
    g.players.get_mut("char1").unwrap().cooldowns.clear();
    g.handle_use_skill("c1", "zhiyu", Some(mon_id.clone()))
        .await;
    assert_eq!(
        g.monsters.iter().find(|m| m.id == mon_id).unwrap().hp,
        mon_hp0,
        "敌怪不可被治疗"
    );
    assert!(g.players["char1"].hp > 5, "无效目标回落治自己");
}

#[tokio::test]
async fn pet_growth_params_come_from_template() {
    let _g = data_lock();
    let mut g = test_game().await;
    g.players
        .insert("char1".into(), test_player("c1", "z1", 12.0, 10.0));
    // 改内存配置: chicken 模板 低基数/强成长/低上限
    let mut d = (*data()).clone();
    for m in d.monsters.iter_mut().filter(|m| m.id == "chicken") {
        m.pet_exp_base = 10;
        m.pet_grow = 2.0;
        m.pet_max_level = 3;
    }
    set_data(d);
    g.spawn_pets("char1", "z1", (12.0, 10.0), "chicken", 1, 0.0, 0, 1.0)
        .await;
    let pet_id = g
        .monsters
        .iter()
        .find(|m| m.owner.is_some())
        .unwrap()
        .id
        .clone();
    let hp1 = g.monsters.iter().find(|m| m.id == pet_id).unwrap().max_hp;
    // 10 经验即可升 2 级 (基数 10×1), 数值翻倍
    g.grant_pet_exp(&pet_id, 10).await;
    {
        let p = g.monsters.iter().find(|m| m.id == pet_id).unwrap();
        assert_eq!(p.pet_level, 2, "基数 10 应 10 经验升级");
        assert_eq!(p.max_hp, hp1 * 2, "成长 2.0 数值翻倍");
    }
    // 大量经验: 按模板上限 3 封顶
    g.grant_pet_exp(&pet_id, 100_000).await;
    assert_eq!(
        g.monsters
            .iter()
            .find(|m| m.id == pet_id)
            .unwrap()
            .pet_level,
        3,
        "上限取模板 pet_max_level"
    );
    // 复原全局配置 (测试进程内共享)
    set_data(GameData::builtin());
}

#[tokio::test]
async fn tame_converts_and_respects_rules() {
    let _g = data_lock();
    let mut g = test_game().await;
    let mut p = test_player("c1", "z1", 10.5, 10.5);
    p.character.class = CharacterClass::Mage;
    p.level = 99;
    p.mp = 999;
    g.players.insert("char1".into(), p);
    // 内存配置: test_dummy 模板不存在 → 用 chicken 模板造一只可诱惑怪
    let mut d = (*data()).clone();
    for m in d.monsters.iter_mut().filter(|m| m.id == "chicken") {
        m.mon_type = "tameable".into();
    }
    // builtin 技能种子无 youhuo (库迁移插入) — 测试内存表补一条
    d.skills.mage.push(SkillDef {
        id: "youhuo".into(),
        name: "诱惑之光".into(),
        mp: 15,
        cd_ms: 0,
        level: 13,
        range: 7.0,
        self_cast: false,
        kind: SkillKind::Tame {
            chance: 0.9,
            max_pets: 5,
        },
        max_level: 3,
        train_base: 30,
        level_bonus: 0.1,
        icon: 0,
        anim: "cast".into(),
        stages: 2,
        fx: "youhuo".into(),
        fx_base: 170,
        fx_frames: 8,
    });
    set_data(d);
    let now = Instant::now();
    g.monsters.push(Monster {
        id: "wild1".into(),
        template: "chicken".into(),
        name: "野鸡".into(),
        level: 1,
        sound: 3,
        boss: false,
        respawn: Duration::from_secs(3600),
        announce: false,
        image: 1,
        image_base: 0,
        zone: "z1".into(),
        home: (11.5, 10.5),
        roam: 1.0,
        x: 11.5,
        y: 10.5,
        dir: 4,
        target: None,
        chasing: false,
        attack_until: None,
        pending_hit: None,
        next_attack: now,
        next_decide: now,
        passive: true,
        hp: 15,
        max_hp: 15,
        damage: 1,
        exp: 5,
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
        owner: None,
        pet_level: 0,
        pet_exp: 0,
        summon_until: None,
    });
    // 成功率 1.0 (youhuo p1 存库 0.35 — 测试改内存技能表不可行, 直接
    // 多次施放直至成功; 为免概率翻车, 把技能表里的 youhuo 概率视为
    // 0.35+0.1*99 → 封顶 0.9, 施放 20 次成功概率 >1-1e-20)
    let mut tamed = false;
    for _ in 0..50 {
        g.players.get_mut("char1").unwrap().cooldowns.clear();
        g.players.get_mut("char1").unwrap().mp = 999;
        g.handle_use_skill("c1", "youhuo", Some("wild1".into()))
            .await;
        if g.monsters
            .iter()
            .any(|m| m.id == "wild1" && m.owner.is_some())
        {
            tamed = true;
            break;
        }
    }
    assert!(tamed, "多次施放后应诱惑成功");
    let m = g.monsters.iter().find(|m| m.id == "wild1").unwrap();
    assert_eq!(m.owner.as_deref(), Some("char1"));
    assert_eq!(m.pet_level, 1, "归顺从 1 级养起");
    assert!(m.aggro_target.is_none(), "归顺清仇恨");
    // 刷新点补位: 应多出一只同模板的替补, 在老家等重生 (名额不被占走)
    let slot = g
        .monsters
        .iter()
        .find(|x| x.owner.is_none() && x.template == "chicken" && x.id != "wild1")
        .expect("应补一只替补进入重生倒计时");
    assert!(slot.respawn_at.is_some(), "替补应等重生");
    assert_eq!(slot.home, (11.5, 10.5), "替补回原刷新点老家");
    assert!(slot.removed_sent, "替补未曾广播过, 不应再发 removed");
    let slot_id = slot.id.clone();
    // 拨快重生: 替补以满血野怪身份上线
    if let Some(x) = g.monsters.iter_mut().find(|x| x.id == slot_id) {
        x.respawn_at = Some(Instant::now() - Duration::from_millis(1));
    }
    g.tick().await;
    let slot = g.monsters.iter().find(|x| x.id == slot_id).unwrap();
    assert!(slot.respawn_at.is_none() && slot.alive(), "替补应正常重生");
    assert!(slot.owner.is_none(), "替补是野怪不是宠物");
    assert_eq!(slot.hp, slot.max_hp, "重生满血");
    // 不可诱惑目标: 稻草人 (test_dummy 模板 → 不在模板表, 视为不可诱惑)
    let dummy = g
        .monsters
        .iter()
        .find(|m| m.owner.is_none() && m.id != "wild1")
        .unwrap()
        .id
        .clone();
    g.players.get_mut("char1").unwrap().cooldowns.clear();
    g.handle_use_skill("c1", "youhuo", Some(dummy.clone()))
        .await;
    assert!(
        g.monsters
            .iter()
            .find(|m| m.id == dummy)
            .unwrap()
            .owner
            .is_none(),
        "非可诱惑类型不得被魅惑"
    );
    set_data(GameData::builtin());
}

// ─────────── 交易 / 仓库 ───────────

fn trade_pair(g: &mut Game) {
    let mut a = test_player("ca", "z1", 10.0, 10.0);
    a.character.id = "pa".into();
    a.character.name = "甲".into();
    a.inventory.push(make_item("iron_sword").unwrap());
    let mut b = test_player("cb", "z1", 11.0, 10.0);
    b.character.id = "pb".into();
    b.character.name = "乙".into();
    b.gold = 500;
    g.players.insert("pa".into(), a);
    g.players.insert("pb".into(), b);
}

#[tokio::test]
async fn trade_full_flow_swaps_items_and_gold() {
    let mut g = test_game().await;
    trade_pair(&mut g);
    g.handle_trade_request("ca", "pb").await;
    g.handle_trade_accept("cb").await;
    assert_eq!(g.trades.len(), 1, "接受后应建立交易");
    let sword = g.players["pa"].inventory[0].id.clone();
    g.handle_trade_place("ca", &sword).await;
    assert!(g.players["pa"].inventory.is_empty(), "放入托管应移出背包");
    g.handle_trade_set_gold("cb", 300).await;
    g.handle_trade_confirm("ca").await;
    g.handle_trade_confirm("cb").await;
    assert!(g.trades.is_empty(), "双确认后交易应结束");
    assert_eq!(g.players["pb"].inventory.len(), 1, "乙应拿到剑");
    assert_eq!(g.players["pa"].gold, 300, "甲应拿到金币");
    assert_eq!(g.players["pb"].gold, 200);
}

#[tokio::test]
async fn trade_modify_resets_confirm() {
    let mut g = test_game().await;
    trade_pair(&mut g);
    g.handle_trade_request("ca", "pb").await;
    g.handle_trade_accept("cb").await;
    g.handle_trade_confirm("ca").await;
    assert!(g.trades[0].ok[0], "甲已确认");
    // 乙改金币 → 双方确认重置
    g.handle_trade_set_gold("cb", 100).await;
    assert!(!g.trades[0].ok[0] && !g.trades[0].ok[1], "改动后确认应重置");
}

#[tokio::test]
async fn trade_cancel_returns_escrow() {
    let mut g = test_game().await;
    trade_pair(&mut g);
    g.handle_trade_request("ca", "pb").await;
    g.handle_trade_accept("cb").await;
    let sword = g.players["pa"].inventory[0].id.clone();
    g.handle_trade_place("ca", &sword).await;
    g.cancel_trade_of("pb", "取消").await;
    assert!(g.trades.is_empty());
    assert_eq!(g.players["pa"].inventory.len(), 1, "取消后托管物应退回甲");
}

#[tokio::test]
async fn trade_requires_proximity() {
    let mut g = test_game().await;
    trade_pair(&mut g);
    if let Some(b) = g.players.get_mut("pb") {
        b.x = 40.0; // 拉远
    }
    g.handle_trade_request("ca", "pb").await;
    assert!(g.trade_invites.is_empty(), "距离太远不应发出邀请");
}

#[tokio::test]
async fn trade_disconnect_cancels() {
    let mut g = test_game().await;
    trade_pair(&mut g);
    g.handle_trade_request("ca", "pb").await;
    g.handle_trade_accept("cb").await;
    let sword = g.players["pa"].inventory[0].id.clone();
    g.handle_trade_place("ca", &sword).await;
    g.on_disconnect("ca").await;
    assert!(g.trades.is_empty(), "掉线应取消交易");
    assert_eq!(g.players["pa"].inventory.len(), 1, "托管物退回");
}

#[tokio::test]
async fn storage_store_and_take() {
    let _g = data_lock();
    let mut g = test_game().await;
    trade_pair(&mut g);
    // 仓库 NPC 立在甲脚边
    let mut d = (*data()).clone();
    d.npcs.push(gamedata::defs::NpcDef {
        id: "stash".into(),
        name: "仓库管理员".into(),
        map: "z1".into(),
        x: 10.0,
        y: 10.0,
        image: 0,
        kind: "storage".into(),
        enabled: true,
        dialogs: Vec::new(),
        shop: Vec::new(),
    });
    set_data(d);
    let sword = g.players["pa"].inventory[0].id.clone();
    g.handle_store_item("ca", "stash", &sword).await;
    assert!(g.players["pa"].inventory.is_empty(), "存入后背包应空");
    assert_eq!(g.players["pa"].storage.len(), 1, "仓库应有一件");
    g.handle_storage_take("ca", "stash", &sword).await;
    assert_eq!(g.players["pa"].inventory.len(), 1, "取出回背包");
    assert!(g.players["pa"].storage.is_empty());
    // 距离外拒绝
    if let Some(p) = g.players.get_mut("pa") {
        p.x = 40.0;
    }
    let id2 = g.players["pa"].inventory[0].id.clone();
    g.handle_store_item("ca", "stash", &id2).await;
    assert_eq!(g.players["pa"].inventory.len(), 1, "太远不应存入");
    set_data(GameData::builtin());
}

// ─────────── PK / 安全区 ───────────

fn pvp_pair(g: &mut Game) {
    let mut a = test_player("ca", "z1", 10.0, 10.0);
    a.character.id = "pa".into();
    a.character.name = "甲".into();
    a.pk_all = true;
    let mut b = test_player("cb", "z1", 11.0, 10.0);
    b.character.id = "pb".into();
    b.character.name = "乙".into();
    g.players.insert("pa".into(), a);
    g.players.insert("pb".into(), b);
}

#[tokio::test]
async fn peace_mode_rejects_pvp() {
    let mut g = test_game().await;
    pvp_pair(&mut g);
    g.players.get_mut("pa").unwrap().pk_all = false;
    assert!(
        g.pvp_check("pa", "pb", 5.0, false).await.is_none(),
        "和平模式不可攻击玩家"
    );
    g.players.get_mut("pa").unwrap().pk_all = true;
    assert!(g.pvp_check("pa", "pb", 5.0, false).await.is_some());
}

#[tokio::test]
async fn safe_zone_blocks_pvp_both_ways() {
    let mut g = test_game().await;
    pvp_pair(&mut g);
    // 在 z1 配一个罩住乙的安全区
    if let Some(z) = g.zones.get_mut("z1") {
        z.sidecar.safe_zones = vec![(11.0, 10.0, 2.0)];
    }
    assert!(
        g.pvp_check("pa", "pb", 5.0, false).await.is_none(),
        "目标在安全区不可攻击"
    );
    // 反向: 攻击者在安全区同样拒绝
    if let Some(z) = g.zones.get_mut("z1") {
        z.sidecar.safe_zones = vec![(10.0, 10.0, 0.5)];
    }
    assert!(
        g.pvp_check("pa", "pb", 5.0, false).await.is_none(),
        "攻击者在安全区不可出手"
    );
}

#[tokio::test]
async fn hitting_white_marks_grey_and_kill_adds_pk() {
    let mut g = test_game().await;
    pvp_pair(&mut g);
    g.hit_player_pvp("pa", "pb", 5).await;
    let now = Instant::now();
    assert_eq!(
        Game::pk_color(&g.players["pa"], now),
        "grey",
        "打中白名应变灰名"
    );
    // 一刀打死 (乙满血 52, 防 0)
    g.hit_player_pvp("pa", "pb", 9999).await;
    assert!(g.players["pb"].dead_until.is_some(), "乙应进入死亡状态");
    assert_eq!(g.players["pa"].pk_points, 100, "杀白名 +100");
    assert_eq!(Game::pk_color(&g.players["pa"], now), "grey", "灰名期内仍灰");
    // 灰名过期后 → 红名
    g.players.get_mut("pa").unwrap().grey_until = None;
    assert_eq!(Game::pk_color(&g.players["pa"], now), "red", "PK 值 ≥100 红名");
}

#[tokio::test]
async fn killing_non_white_adds_no_pk() {
    let mut g = test_game().await;
    pvp_pair(&mut g);
    // 乙先变灰
    g.players.get_mut("pb").unwrap().grey_until = Some(Instant::now() + Duration::from_secs(60));
    g.hit_player_pvp("pa", "pb", 9999).await;
    assert!(g.players["pb"].dead_until.is_some());
    assert_eq!(g.players["pa"].pk_points, 0, "杀灰名不加 PK 值");
    assert!(
        g.players["pa"].grey_until.is_none(),
        "打灰名自己不变灰"
    );
}

#[tokio::test]
async fn dead_player_revives_at_spawn_full_hp() {
    let mut g = test_game().await;
    pvp_pair(&mut g);
    g.hit_player_pvp("pa", "pb", 9999).await;
    assert_eq!(g.players["pb"].hp, 0);
    // 尸体期不再吃伤害
    g.damage_player(None, "pb", 50).await;
    assert_eq!(g.players["pb"].hp, 0, "尸体不重复扣血");
    // 时间未到不复活
    g.process_revives(Instant::now()).await;
    assert!(g.players["pb"].dead_until.is_some());
    // 到点复活: 回出生点满血
    g.players.get_mut("pb").unwrap().dead_until = Some(Instant::now() - Duration::from_secs(1));
    g.process_revives(Instant::now()).await;
    let p = &g.players["pb"];
    assert!(p.dead_until.is_none());
    assert_eq!(p.hp, p.max_hp, "复活满血");
    let spawn = g.zones["z1"].spawn;
    assert!((p.x - spawn.0).abs() < 0.01 && (p.y - spawn.1).abs() < 0.01, "回出生点");
}

#[tokio::test]
async fn pk_points_decay_over_time() {
    let mut g = test_game().await;
    pvp_pair(&mut g);
    g.players.get_mut("pa").unwrap().pk_points = 5;
    g.last_pk_decay = Instant::now() - Duration::from_secs(121);
    g.decay_pk(Instant::now()).await;
    assert_eq!(g.players["pa"].pk_points, 4, "到点应 -1");
    g.decay_pk(Instant::now()).await;
    assert_eq!(g.players["pa"].pk_points, 4, "间隔未到不再扣");
}

#[tokio::test]
async fn monster_kill_reuses_death_flow() {
    let mut g = test_game().await;
    pvp_pair(&mut g);
    g.apply_monster_hits(vec![("pb".into(), 9999)]).await;
    assert!(g.players["pb"].dead_until.is_some(), "怪物击杀同样进死亡状态");
    assert_eq!(g.players["pa"].pk_points, 0, "怪物击杀无善恶结算");
}

#[tokio::test]
async fn player_poison_ticks_until_expiry() {
    let mut g = test_game().await;
    pvp_pair(&mut g);
    let now = Instant::now();
    g.pending_player_poisons
        .push((now, "pa".into(), "pb".into(), 3, 10.0));
    let hp0 = g.players["pb"].hp;
    g.settle_player_poisons(now).await; // 上毒 + 第一跳
    g.settle_player_poisons(now).await;
    assert!(g.players["pb"].hp < hp0, "第一跳应掉血");
    assert!(g.players["pb"].poison.is_some());
    // 到期清毒
    if let Some(p) = g.players.get_mut("pb") {
        if let Some(t) = p.poison.as_mut() {
            t.0 = now - Duration::from_secs(1);
        }
    }
    g.settle_player_poisons(now).await;
    assert!(g.players["pb"].poison.is_none(), "到期应清毒");
}

// ─────────── 耐久 / 极品 ───────────

#[tokio::test]
async fn rare_roll_respects_chance_and_bounds() {
    let _g = data_lock();
    let mut g = test_game().await;
    // 100% 极品率: 一定滚点, 且点数在 1..=上限
    let mut d = (*data()).clone();
    if let Some(it) = d.items.iter_mut().find(|i| i.template == "iron_sword") {
        it.rare_chance = 1.0;
        it.rare_max = 3;
    }
    set_data(d);
    let mut item = make_item("iron_sword").unwrap();
    let base_atk = item.attack;
    g.roll_rare(&mut item);
    assert!(item.bonus >= 1 && item.bonus <= 3, "点数应在 1..=3: {}", item.bonus);
    assert_eq!(item.attack, base_atk + item.bonus, "铁剑只有攻击非零, 加点全进攻击");
    // 0% 极品率: 永不滚点
    let mut d = (*data()).clone();
    if let Some(it) = d.items.iter_mut().find(|i| i.template == "iron_sword") {
        it.rare_chance = 0.0;
    }
    set_data(d);
    for _ in 0..20 {
        let mut it2 = make_item("iron_sword").unwrap();
        g.roll_rare(&mut it2);
        assert_eq!(it2.bonus, 0, "0 概率不该出极品");
    }
    set_data(GameData::builtin());
}

#[tokio::test]
async fn broken_item_stats_excluded() {
    let mut g = test_game().await;
    g.players.insert("char1".into(), test_player("c1", "z1", 10.0, 10.0));
    let mut sword = make_item("iron_sword").unwrap();
    let p = g.players.get_mut("char1").unwrap();
    p.equipment.insert("weapon".into(), sword.clone());
    assert!(p.equip_attack() > 0, "完好武器计入攻击");
    sword.dur = 0;
    p.equipment.insert("weapon".into(), sword);
    assert_eq!(p.equip_attack(), 0, "损坏武器不计入攻击");
}

#[tokio::test]
async fn weapon_wears_and_breaks() {
    let mut g = test_game().await;
    g.players.insert("char1".into(), test_player("c1", "z1", 10.0, 10.0));
    let mut sword = make_item("iron_sword").unwrap();
    sword.dur = 1;
    g.players
        .get_mut("char1")
        .unwrap()
        .equipment
        .insert("weapon".into(), sword);
    // 1/8 概率: 多磨几次必掉 (随机数确定性推进)
    for _ in 0..200 {
        g.wear_weapon("char1").await;
        if g.players["char1"].equipment["weapon"].dur == 0 {
            break;
        }
    }
    assert_eq!(g.players["char1"].equipment["weapon"].dur, 0, "200 次内必损坏");
    assert_eq!(g.players["char1"].equip_attack(), 0, "损坏后攻击失效");
}

#[tokio::test]
async fn armor_wears_when_hit() {
    let mut g = test_game().await;
    g.players.insert("char1".into(), test_player("c1", "z1", 10.0, 10.0));
    let armor = make_item("leather_armor").unwrap();
    g.players
        .get_mut("char1")
        .unwrap()
        .equipment
        .insert("armor".into(), armor);
    let d0 = g.players["char1"].equipment["armor"].dur;
    for _ in 0..300 {
        g.damage_player(None, "char1", 1).await;
        if let Some(p) = g.players.get_mut("char1") {
            p.hp = p.max_hp; // 别打死
        }
        if g.players["char1"].equipment["armor"].dur < d0 {
            break;
        }
    }
    assert!(g.players["char1"].equipment["armor"].dur < d0, "被击应磨防具");
}

#[tokio::test]
async fn repair_charges_and_restores() {
    let _g = data_lock();
    let mut g = test_game().await;
    g.players.insert("char1".into(), test_player("c1", "z1", 10.0, 10.0));
    // 修理 NPC 在脚边
    let mut d = (*data()).clone();
    d.npcs.push(gamedata::defs::NpcDef {
        id: "smith".into(),
        name: "修理商".into(),
        map: "z1".into(),
        x: 10.0,
        y: 10.0,
        image: 0,
        kind: "repair".into(),
        enabled: true,
        dialogs: Vec::new(),
        shop: Vec::new(),
    });
    set_data(d);
    let mut sword = make_item("iron_sword").unwrap();
    sword.dur = 5; // 损耗 15 点
    let per = (item_def("iron_sword").unwrap().price as u64 / sword.max_dur as u64).max(1);
    let expect = 15 * per;
    {
        let p = g.players.get_mut("char1").unwrap();
        p.equipment.insert("weapon".into(), sword);
        p.gold = expect - 1; // 差一块钱
    }
    g.handle_repair("c1", "smith").await;
    assert_eq!(g.players["char1"].equipment["weapon"].dur, 5, "钱不够不修");
    g.players.get_mut("char1").unwrap().gold = expect + 10;
    g.handle_repair("c1", "smith").await;
    let p = &g.players["char1"];
    assert_eq!(p.equipment["weapon"].dur, p.equipment["weapon"].max_dur, "修满");
    assert_eq!(p.gold, 10, "按损耗计费");
    set_data(GameData::builtin());
}
