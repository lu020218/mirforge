//! 游戏配置的数据库存储（物品/技能/任务/区域边车）。
//!
//! 设计：SQLite 规范化表是配置的**唯一持久化真源**；`server/data/*.json` 与
//! `zones/*.json` 降级为**首次启动的种子数据**（库为空时导入一次，之后不再读）。
//! 运行时仍由 `game::GameData` 内存快照服务游戏逻辑（同步读，不碰数据库），
//! 管理台写入 = 事务替换表内容 → 重建内存快照。

use std::collections::HashMap;

use sqlx::{Row, SqlitePool};

use crate::game::{
    BossDef, DropSidecar, GameData, ItemDef, MonsterSidecar, NpcDef, NpcDialogPage, NpcOptionDef,
    PortalSidecar, QuestDef, QuestReward, ShopEntry, SkillDef, SkillKind, SkillsCfg, ZoneSidecar,
};

/// 建表（幂等）
pub async fn ensure_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    for ddl in [
        "CREATE TABLE IF NOT EXISTS cfg_items (
            template TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            slot TEXT NOT NULL,
            attack INTEGER NOT NULL DEFAULT 0,
            defense INTEGER NOT NULL DEFAULT 0,
            hp INTEGER NOT NULL DEFAULT 0,
            image INTEGER NOT NULL DEFAULT 0,
            shape INTEGER NOT NULL DEFAULT 0,
            ord INTEGER NOT NULL DEFAULT 0
        )",
        "CREATE TABLE IF NOT EXISTS cfg_skills (
            id TEXT PRIMARY KEY,
            class TEXT NOT NULL,
            name TEXT NOT NULL,
            mp INTEGER NOT NULL DEFAULT 0,
            cd_ms INTEGER NOT NULL DEFAULT 0,
            level INTEGER NOT NULL DEFAULT 1,
            range REAL NOT NULL DEFAULT 0,
            self_cast INTEGER NOT NULL DEFAULT 0,
            kind_type TEXT NOT NULL DEFAULT 'damage',
            p1 REAL NOT NULL DEFAULT 0,
            p2 REAL NOT NULL DEFAULT 0,
            ord INTEGER NOT NULL DEFAULT 0
        )",
        "CREATE TABLE IF NOT EXISTS cfg_quests (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            exp_reward INTEGER NOT NULL DEFAULT 0,
            prereq TEXT,
            ord INTEGER NOT NULL DEFAULT 0
        )",
        "CREATE TABLE IF NOT EXISTS cfg_quest_rewards (
            quest_id TEXT NOT NULL,
            item TEXT NOT NULL,
            count INTEGER NOT NULL DEFAULT 1,
            ord INTEGER NOT NULL DEFAULT 0
        )",
        "CREATE TABLE IF NOT EXISTS cfg_quest_objectives (
            quest_id TEXT NOT NULL,
            idx INTEGER NOT NULL,
            target TEXT NOT NULL,
            count INTEGER NOT NULL,
            PRIMARY KEY (quest_id, idx)
        )",
        "CREATE TABLE IF NOT EXISTS cfg_zones (
            map TEXT PRIMARY KEY,
            name TEXT,
            spawn_x REAL,
            spawn_y REAL,
            minimap INTEGER
        )",
        "CREATE TABLE IF NOT EXISTS cfg_portals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            map TEXT NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            to_map TEXT NOT NULL,
            to_x REAL,
            to_y REAL
        )",
        "CREATE TABLE IF NOT EXISTS cfg_spawns (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            map TEXT NOT NULL,
            template TEXT NOT NULL,
            image INTEGER NOT NULL DEFAULT 0,
            x REAL NOT NULL,
            y REAL NOT NULL,
            count INTEGER NOT NULL DEFAULT 1,
            radius REAL NOT NULL DEFAULT 5,
            passive INTEGER NOT NULL DEFAULT 0,
            hp INTEGER NOT NULL DEFAULT 30,
            damage INTEGER NOT NULL DEFAULT 0,
            exp INTEGER NOT NULL DEFAULT 10
        )",
        "CREATE TABLE IF NOT EXISTS cfg_npcs (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            map TEXT NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            image INTEGER NOT NULL DEFAULT 0,
            kind TEXT NOT NULL DEFAULT 'talk',
            enabled INTEGER NOT NULL DEFAULT 1,
            ord INTEGER NOT NULL DEFAULT 0
        )",
        "CREATE TABLE IF NOT EXISTS cfg_bosses (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            map TEXT NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            image INTEGER NOT NULL DEFAULT 0,
            hp INTEGER NOT NULL DEFAULT 1000,
            damage INTEGER NOT NULL DEFAULT 20,
            exp INTEGER NOT NULL DEFAULT 500,
            respawn_secs INTEGER NOT NULL DEFAULT 1800,
            roam REAL NOT NULL DEFAULT 0,
            announce INTEGER NOT NULL DEFAULT 1,
            enabled INTEGER NOT NULL DEFAULT 1,
            ord INTEGER NOT NULL DEFAULT 0
        )",
        "CREATE TABLE IF NOT EXISTS cfg_boss_drops (
            boss_id TEXT NOT NULL,
            item TEXT NOT NULL,
            chance REAL NOT NULL,
            ord INTEGER NOT NULL DEFAULT 0
        )",
        "CREATE TABLE IF NOT EXISTS cfg_npc_shop (
            npc_id TEXT NOT NULL,
            item TEXT NOT NULL,
            price INTEGER NOT NULL DEFAULT 0,
            stock INTEGER NOT NULL DEFAULT -1,
            ord INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (npc_id, item)
        )",
        "CREATE TABLE IF NOT EXISTS cfg_npc_dialogs (
            npc_id TEXT NOT NULL,
            page INTEGER NOT NULL,
            text TEXT NOT NULL,
            PRIMARY KEY (npc_id, page)
        )",
        "CREATE TABLE IF NOT EXISTS cfg_npc_options (
            npc_id TEXT NOT NULL,
            page INTEGER NOT NULL,
            idx INTEGER NOT NULL,
            label TEXT NOT NULL,
            action TEXT NOT NULL DEFAULT 'close',
            arg TEXT NOT NULL DEFAULT '',
            PRIMARY KEY (npc_id, page, idx)
        )",
        "CREATE TABLE IF NOT EXISTS cfg_drops (
            spawn_id INTEGER NOT NULL,
            item TEXT NOT NULL,
            chance REAL NOT NULL
        )",
    ] {
        sqlx::query(ddl).execute(pool).await?;
    }
    // 旧库升级 (列已存在则忽略)
    let _ = sqlx::query("ALTER TABLE cfg_quests ADD COLUMN gold_reward INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE cfg_items ADD COLUMN price INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE cfg_zones ADD COLUMN minimap INTEGER")
        .execute(pool)
        .await;
    Ok(())
}

/// 配置表是否为空（决定是否需要种子导入）
pub async fn is_empty(pool: &SqlitePool) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query("SELECT COUNT(*) AS n FROM cfg_items")
        .fetch_one(pool)
        .await?
        .get("n");
    Ok(n == 0)
}

// ─────────── 读取（启动 / 热重载后重建内存快照） ───────────

pub async fn load_game_data(pool: &SqlitePool) -> Result<GameData, sqlx::Error> {
    let items = sqlx::query(
        "SELECT template, name, slot, attack, defense, hp, image, shape, price
         FROM cfg_items ORDER BY ord, template",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| ItemDef {
        template: r.get("template"),
        name: r.get("name"),
        slot: r.get("slot"),
        attack: r.get::<i64, _>("attack") as i32,
        defense: r.get::<i64, _>("defense") as i32,
        hp: r.get::<i64, _>("hp") as i32,
        image: r.get::<i64, _>("image") as u16,
        shape: r.get::<i64, _>("shape") as u16,
        price: r.get::<i64, _>("price").max(0) as u32,
    })
    .collect();

    let mut skills = SkillsCfg {
        warrior: Vec::new(),
        mage: Vec::new(),
        taoist: Vec::new(),
    };
    for r in sqlx::query(
        "SELECT id, class, name, mp, cd_ms, level, range, self_cast, kind_type, p1, p2
         FROM cfg_skills ORDER BY class, ord, level",
    )
    .fetch_all(pool)
    .await?
    {
        let (p1, p2): (f64, f64) = (r.get("p1"), r.get("p2"));
        let def = SkillDef {
            id: r.get("id"),
            name: r.get("name"),
            mp: r.get::<i64, _>("mp") as i32,
            cd_ms: r.get::<i64, _>("cd_ms") as u64,
            level: r.get::<i64, _>("level") as u32,
            range: r.get("range"),
            self_cast: r.get::<i64, _>("self_cast") != 0,
            kind: match r.get::<String, _>("kind_type").as_str() {
                "heal" => SkillKind::Heal,
                "aoe" => SkillKind::Aoe {
                    radius: p1,
                    mult: p2,
                },
                _ => SkillKind::Damage(p1),
            },
        };
        match r.get::<String, _>("class").as_str() {
            "mage" => skills.mage.push(def),
            "taoist" => skills.taoist.push(def),
            _ => skills.warrior.push(def),
        }
    }

    let mut quests: Vec<QuestDef> = sqlx::query(
        "SELECT id, name, exp_reward, gold_reward, prereq FROM cfg_quests ORDER BY ord, id",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| QuestDef {
        id: r.get("id"),
        name: r.get("name"),
        objectives: Vec::new(),
        exp_reward: r.get::<i64, _>("exp_reward") as u64,
        gold_reward: r.get::<i64, _>("gold_reward").max(0) as u64,
        rewards: Vec::new(),
        prereq: r.get("prereq"),
    })
    .collect();
    let mut qrewards: HashMap<String, Vec<QuestReward>> = HashMap::new();
    for r in
        sqlx::query("SELECT quest_id, item, count FROM cfg_quest_rewards ORDER BY quest_id, ord")
            .fetch_all(pool)
            .await?
    {
        qrewards
            .entry(r.get("quest_id"))
            .or_default()
            .push(QuestReward {
                item: r.get("item"),
                count: r.get::<i64, _>("count").max(0) as u32,
            });
    }
    for r in sqlx::query(
        "SELECT quest_id, target, count FROM cfg_quest_objectives ORDER BY quest_id, idx",
    )
    .fetch_all(pool)
    .await?
    {
        let qid: String = r.get("quest_id");
        if let Some(q) = quests.iter_mut().find(|q| q.id == qid) {
            q.objectives
                .push((r.get("target"), r.get::<i64, _>("count") as u32));
        }
    }
    for q in quests.iter_mut() {
        if let Some(rw) = qrewards.remove(&q.id) {
            q.rewards = rw;
        }
    }

    let mut npcs: Vec<NpcDef> = sqlx::query(
        "SELECT id, name, map, x, y, image, kind, enabled FROM cfg_npcs ORDER BY ord, id",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| NpcDef {
        id: r.get("id"),
        name: r.get("name"),
        map: r.get("map"),
        x: r.get("x"),
        y: r.get("y"),
        image: r.get::<i64, _>("image") as u16,
        kind: r.get("kind"),
        enabled: r.get::<i64, _>("enabled") != 0,
        dialogs: Vec::new(),
        shop: Vec::new(),
    })
    .collect();
    let mut shops: HashMap<String, Vec<ShopEntry>> = HashMap::new();
    for r in sqlx::query("SELECT npc_id, item, price, stock FROM cfg_npc_shop ORDER BY npc_id, ord")
        .fetch_all(pool)
        .await?
    {
        shops.entry(r.get("npc_id")).or_default().push(ShopEntry {
            item: r.get("item"),
            price: r.get::<i64, _>("price").max(0) as u32,
            stock: r.get::<i64, _>("stock") as i32,
        });
    }
    // 对话页与选项分表存, 读出来再按 npc_id/page 挂回去
    let mut pages: HashMap<String, Vec<NpcDialogPage>> = HashMap::new();
    for r in sqlx::query("SELECT npc_id, page, text FROM cfg_npc_dialogs ORDER BY npc_id, page")
        .fetch_all(pool)
        .await?
    {
        pages
            .entry(r.get("npc_id"))
            .or_default()
            .push(NpcDialogPage {
                page: r.get::<i64, _>("page") as u32,
                text: r.get("text"),
                options: Vec::new(),
            });
    }
    for r in sqlx::query(
        "SELECT npc_id, page, label, action, arg FROM cfg_npc_options ORDER BY npc_id, page, idx",
    )
    .fetch_all(pool)
    .await?
    {
        let npc_id: String = r.get("npc_id");
        let page = r.get::<i64, _>("page") as u32;
        if let Some(d) = pages
            .get_mut(&npc_id)
            .and_then(|v| v.iter_mut().find(|d| d.page == page))
        {
            d.options.push(NpcOptionDef {
                label: r.get("label"),
                action: r.get("action"),
                arg: r.get("arg"),
            });
        }
    }
    for n in npcs.iter_mut() {
        if let Some(d) = pages.remove(&n.id) {
            n.dialogs = d;
        }
        if let Some(sh) = shops.remove(&n.id) {
            n.shop = sh;
        }
    }

    let mut bosses: Vec<BossDef> = sqlx::query(
        "SELECT id, name, map, x, y, image, hp, damage, exp, respawn_secs, roam, announce, enabled
         FROM cfg_bosses ORDER BY ord, id",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| BossDef {
        id: r.get("id"),
        name: r.get("name"),
        map: r.get("map"),
        x: r.get("x"),
        y: r.get("y"),
        image: r.get::<i64, _>("image") as u16,
        hp: r.get::<i64, _>("hp") as i32,
        damage: r.get::<i64, _>("damage") as i32,
        exp: r.get::<i64, _>("exp") as u64,
        respawn_secs: r.get::<i64, _>("respawn_secs").max(1) as u64,
        roam: r.get("roam"),
        announce: r.get::<i64, _>("announce") != 0,
        enabled: r.get::<i64, _>("enabled") != 0,
        drops: Vec::new(),
    })
    .collect();
    let mut bdrops: HashMap<String, Vec<DropSidecar>> = HashMap::new();
    for r in sqlx::query("SELECT boss_id, item, chance FROM cfg_boss_drops ORDER BY boss_id, ord")
        .fetch_all(pool)
        .await?
    {
        bdrops
            .entry(r.get("boss_id"))
            .or_default()
            .push(DropSidecar {
                item: r.get("item"),
                chance: r.get("chance"),
            });
    }
    for b in bosses.iter_mut() {
        if let Some(ds) = bdrops.remove(&b.id) {
            b.drops = ds
                .into_iter()
                .map(|d| crate::game::DropEntry {
                    item: d.item,
                    chance: d.chance,
                })
                .collect();
        }
    }

    Ok(GameData {
        items,
        skills,
        quests,
        npcs,
        bosses,
    })
}

pub async fn save_bosses(pool: &SqlitePool, bosses: &[BossDef]) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for t in ["cfg_bosses", "cfg_boss_drops"] {
        sqlx::query(&format!("DELETE FROM {t}"))
            .execute(&mut *tx)
            .await?;
    }
    for (i, b) in bosses.iter().enumerate() {
        sqlx::query(
            "INSERT INTO cfg_bosses
             (id, name, map, x, y, image, hp, damage, exp, respawn_secs, roam, announce, enabled, ord)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&b.id)
        .bind(&b.name)
        .bind(&b.map)
        .bind(b.x)
        .bind(b.y)
        .bind(b.image as i64)
        .bind(b.hp as i64)
        .bind(b.damage as i64)
        .bind(b.exp as i64)
        .bind(b.respawn_secs as i64)
        .bind(b.roam)
        .bind(b.announce as i64)
        .bind(b.enabled as i64)
        .bind(i as i64)
        .execute(&mut *tx)
        .await?;
        for (di, d) in b.drops.iter().enumerate() {
            sqlx::query(
                "INSERT INTO cfg_boss_drops (boss_id, item, chance, ord) VALUES (?, ?, ?, ?)",
            )
            .bind(&b.id)
            .bind(&d.item)
            .bind(d.chance)
            .bind(di as i64)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await
}

pub async fn save_npcs(pool: &SqlitePool, npcs: &[NpcDef]) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for t in [
        "cfg_npcs",
        "cfg_npc_dialogs",
        "cfg_npc_options",
        "cfg_npc_shop",
    ] {
        sqlx::query(&format!("DELETE FROM {t}"))
            .execute(&mut *tx)
            .await?;
    }
    for (i, n) in npcs.iter().enumerate() {
        sqlx::query(
            "INSERT INTO cfg_npcs (id, name, map, x, y, image, kind, enabled, ord)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&n.id)
        .bind(&n.name)
        .bind(&n.map)
        .bind(n.x)
        .bind(n.y)
        .bind(n.image as i64)
        .bind(&n.kind)
        .bind(n.enabled as i64)
        .bind(i as i64)
        .execute(&mut *tx)
        .await?;
        for (si, e) in n.shop.iter().enumerate() {
            sqlx::query(
                "INSERT INTO cfg_npc_shop (npc_id, item, price, stock, ord) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&n.id)
            .bind(&e.item)
            .bind(e.price as i64)
            .bind(e.stock as i64)
            .bind(si as i64)
            .execute(&mut *tx)
            .await?;
        }
        for d in &n.dialogs {
            sqlx::query("INSERT INTO cfg_npc_dialogs (npc_id, page, text) VALUES (?, ?, ?)")
                .bind(&n.id)
                .bind(d.page as i64)
                .bind(&d.text)
                .execute(&mut *tx)
                .await?;
            for (oi, o) in d.options.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO cfg_npc_options (npc_id, page, idx, label, action, arg)
                     VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(&n.id)
                .bind(d.page as i64)
                .bind(oi as i64)
                .bind(&o.label)
                .bind(&o.action)
                .bind(&o.arg)
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    tx.commit().await
}

pub async fn load_zone_sidecars(
    pool: &SqlitePool,
) -> Result<HashMap<String, ZoneSidecar>, sqlx::Error> {
    let mut out: HashMap<String, ZoneSidecar> = HashMap::new();
    for r in sqlx::query("SELECT map, name, spawn_x, spawn_y, minimap FROM cfg_zones ORDER BY map")
        .fetch_all(pool)
        .await?
    {
        let map: String = r.get("map");
        let sx: Option<f64> = r.get("spawn_x");
        let sy: Option<f64> = r.get("spawn_y");
        out.insert(
            map,
            ZoneSidecar {
                name: r.get("name"),
                spawn: sx.zip(sy),
                minimap: r.get::<Option<i64>, _>("minimap").map(|v| v as u16),
                portals: Vec::new(),
                monsters: Vec::new(),
            },
        );
    }
    for r in sqlx::query("SELECT map, x, y, to_map, to_x, to_y FROM cfg_portals ORDER BY id")
        .fetch_all(pool)
        .await?
    {
        let map: String = r.get("map");
        if let Some(z) = out.get_mut(&map) {
            z.portals.push(PortalSidecar {
                x: r.get("x"),
                y: r.get("y"),
                to: r.get("to_map"),
                to_x: r.get("to_x"),
                to_y: r.get("to_y"),
            });
        }
    }
    let drops = sqlx::query("SELECT spawn_id, item, chance FROM cfg_drops")
        .fetch_all(pool)
        .await?;
    for r in sqlx::query(
        "SELECT id, map, template, image, x, y, count, radius, passive, hp, damage, exp
         FROM cfg_spawns ORDER BY id",
    )
    .fetch_all(pool)
    .await?
    {
        let map: String = r.get("map");
        let sid: i64 = r.get("id");
        let Some(z) = out.get_mut(&map) else { continue };
        z.monsters.push(MonsterSidecar {
            template: r.get("template"),
            image: r.get::<i64, _>("image") as u16,
            x: r.get("x"),
            y: r.get("y"),
            count: r.get::<i64, _>("count") as u32,
            radius: r.get("radius"),
            passive: r.get::<i64, _>("passive") != 0,
            hp: r.get::<i64, _>("hp") as i32,
            damage: r.get::<i64, _>("damage") as i32,
            exp: r.get::<i64, _>("exp") as u64,
            drops: drops
                .iter()
                .filter(|d| d.get::<i64, _>("spawn_id") == sid)
                .map(|d| DropSidecar {
                    item: d.get("item"),
                    chance: d.get("chance"),
                })
                .collect(),
        });
    }
    Ok(out)
}

// ─────────── 写入（管理台保存；整表事务替换） ───────────

pub async fn save_items(pool: &SqlitePool, items: &[ItemDef]) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM cfg_items")
        .execute(&mut *tx)
        .await?;
    for (i, d) in items.iter().enumerate() {
        sqlx::query(
            "INSERT INTO cfg_items (template, name, slot, attack, defense, hp, image, shape, price, ord)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&d.template)
        .bind(&d.name)
        .bind(&d.slot)
        .bind(d.attack as i64)
        .bind(d.defense as i64)
        .bind(d.hp as i64)
        .bind(d.image as i64)
        .bind(d.shape as i64)
        .bind(d.price as i64)
        .bind(i as i64)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

pub async fn save_skills(pool: &SqlitePool, cfg: &SkillsCfg) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM cfg_skills")
        .execute(&mut *tx)
        .await?;
    for (class, list) in [
        ("warrior", &cfg.warrior),
        ("mage", &cfg.mage),
        ("taoist", &cfg.taoist),
    ] {
        for (i, s) in list.iter().enumerate() {
            let (kind_type, p1, p2) = match &s.kind {
                SkillKind::Heal => ("heal", 0.0, 0.0),
                SkillKind::Damage(m) => ("damage", *m, 0.0),
                SkillKind::Aoe { radius, mult } => ("aoe", *radius, *mult),
            };
            sqlx::query(
                "INSERT INTO cfg_skills
                 (id, class, name, mp, cd_ms, level, range, self_cast, kind_type, p1, p2, ord)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&s.id)
            .bind(class)
            .bind(&s.name)
            .bind(s.mp as i64)
            .bind(s.cd_ms as i64)
            .bind(s.level as i64)
            .bind(s.range)
            .bind(s.self_cast as i64)
            .bind(kind_type)
            .bind(p1)
            .bind(p2)
            .bind(i as i64)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await
}

pub async fn save_quests(pool: &SqlitePool, quests: &[QuestDef]) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    for t in ["cfg_quest_objectives", "cfg_quest_rewards", "cfg_quests"] {
        sqlx::query(&format!("DELETE FROM {t}"))
            .execute(&mut *tx)
            .await?;
    }
    for (i, q) in quests.iter().enumerate() {
        sqlx::query(
            "INSERT INTO cfg_quests (id, name, exp_reward, gold_reward, prereq, ord)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&q.id)
        .bind(&q.name)
        .bind(q.exp_reward as i64)
        .bind(q.gold_reward as i64)
        .bind(q.prereq.as_deref())
        .bind(i as i64)
        .execute(&mut *tx)
        .await?;
        for (j, (target, count)) in q.objectives.iter().enumerate() {
            sqlx::query(
                "INSERT INTO cfg_quest_objectives (quest_id, idx, target, count)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&q.id)
            .bind(j as i64)
            .bind(target)
            .bind(*count as i64)
            .execute(&mut *tx)
            .await?;
        }
        for (j, rw) in q.rewards.iter().enumerate() {
            sqlx::query(
                "INSERT INTO cfg_quest_rewards (quest_id, item, count, ord) VALUES (?, ?, ?, ?)",
            )
            .bind(&q.id)
            .bind(&rw.item)
            .bind(rw.count as i64)
            .bind(j as i64)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await
}

/// 保存单个区域边车（含传送门/刷新点/掉落，整区替换）
pub async fn save_zone(pool: &SqlitePool, map: &str, sc: &ZoneSidecar) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "DELETE FROM cfg_drops WHERE spawn_id IN (SELECT id FROM cfg_spawns WHERE map = ?)",
    )
    .bind(map)
    .execute(&mut *tx)
    .await?;
    for t in ["cfg_spawns", "cfg_portals", "cfg_zones"] {
        sqlx::query(&format!("DELETE FROM {t} WHERE map = ?"))
            .bind(map)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "INSERT INTO cfg_zones (map, name, spawn_x, spawn_y, minimap) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(map)
    .bind(sc.name.as_deref())
    .bind(sc.spawn.map(|s| s.0))
    .bind(sc.spawn.map(|s| s.1))
    .bind(sc.minimap.map(|v| v as i64))
    .execute(&mut *tx)
    .await?;
    for p in &sc.portals {
        sqlx::query(
            "INSERT INTO cfg_portals (map, x, y, to_map, to_x, to_y) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(map)
        .bind(p.x)
        .bind(p.y)
        .bind(&p.to)
        .bind(p.to_x)
        .bind(p.to_y)
        .execute(&mut *tx)
        .await?;
    }
    for m in &sc.monsters {
        let r = sqlx::query(
            "INSERT INTO cfg_spawns
             (map, template, image, x, y, count, radius, passive, hp, damage, exp)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(map)
        .bind(&m.template)
        .bind(m.image as i64)
        .bind(m.x)
        .bind(m.y)
        .bind(m.count as i64)
        .bind(m.radius)
        .bind(m.passive as i64)
        .bind(m.hp as i64)
        .bind(m.damage as i64)
        .bind(m.exp as i64)
        .execute(&mut *tx)
        .await?;
        let sid = r.last_insert_rowid();
        for d in &m.drops {
            sqlx::query("INSERT INTO cfg_drops (spawn_id, item, chance) VALUES (?, ?, ?)")
                .bind(sid)
                .bind(&d.item)
                .bind(d.chance)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await
}

/// 删除区域配置 (含传送门/刷新点/掉落)
pub async fn delete_zone(pool: &SqlitePool, map: &str) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "DELETE FROM cfg_drops WHERE spawn_id IN (SELECT id FROM cfg_spawns WHERE map = ?)",
    )
    .bind(map)
    .execute(&mut *tx)
    .await?;
    for t in ["cfg_spawns", "cfg_portals", "cfg_zones"] {
        sqlx::query(&format!("DELETE FROM {t} WHERE map = ?"))
            .bind(map)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

// ─────────── 首次种子导入（库为空时，从 JSON 文件/内置默认灌入一次） ───────────

pub async fn seed_from_files(
    pool: &SqlitePool,
    data: &GameData,
    zones: &HashMap<String, ZoneSidecar>,
) -> Result<(), sqlx::Error> {
    save_items(pool, &data.items).await?;
    save_skills(pool, &data.skills).await?;
    save_quests(pool, &data.quests).await?;
    save_npcs(pool, &data.npcs).await?;
    save_bosses(pool, &data.bosses).await?;
    for (map, sc) in zones {
        save_zone(pool, map, sc).await?;
    }
    Ok(())
}
