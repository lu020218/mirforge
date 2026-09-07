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
            magic INTEGER NOT NULL DEFAULT 0,
            spirit INTEGER NOT NULL DEFAULT 0,
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
            max_level INTEGER NOT NULL DEFAULT 3,
            train_base INTEGER NOT NULL DEFAULT 30,
            level_bonus REAL NOT NULL DEFAULT 0.3,
            icon INTEGER NOT NULL DEFAULT 0,
            fx_lib INTEGER NOT NULL DEFAULT 0,
            fx_base INTEGER NOT NULL DEFAULT 0,
            fx_frames INTEGER NOT NULL DEFAULT 0,
            anim TEXT NOT NULL DEFAULT '',
            stages INTEGER NOT NULL DEFAULT 0,
            fx TEXT NOT NULL DEFAULT '',
            ord INTEGER NOT NULL DEFAULT 0
        )",
        "CREATE TABLE IF NOT EXISTS cfg_news (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            body TEXT NOT NULL DEFAULT '',
            pinned INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now', 'localtime'))
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
        "CREATE TABLE IF NOT EXISTS cfg_monsters (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            image INTEGER NOT NULL DEFAULT 0,
            base INTEGER NOT NULL DEFAULT 0,
            hp INTEGER NOT NULL DEFAULT 30,
            damage INTEGER NOT NULL DEFAULT 0,
            exp INTEGER NOT NULL DEFAULT 10,
            passive INTEGER NOT NULL DEFAULT 0,
            drops TEXT NOT NULL DEFAULT '[]',
            ord INTEGER NOT NULL DEFAULT 0
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
    let _ = sqlx::query("ALTER TABLE cfg_items ADD COLUMN magic INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE cfg_items ADD COLUMN spirit INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE cfg_skills ADD COLUMN max_level INTEGER NOT NULL DEFAULT 3")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE cfg_skills ADD COLUMN train_base INTEGER NOT NULL DEFAULT 30")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE cfg_skills ADD COLUMN level_bonus REAL NOT NULL DEFAULT 0.3")
        .execute(pool)
        .await;
    let _ = sqlx::query("ALTER TABLE cfg_monsters ADD COLUMN base INTEGER NOT NULL DEFAULT 0")
        .execute(pool)
        .await;
    for col in [
        "icon INTEGER NOT NULL DEFAULT 0",
        "fx_lib INTEGER NOT NULL DEFAULT 0",
        "fx_base INTEGER NOT NULL DEFAULT 0",
        "fx_frames INTEGER NOT NULL DEFAULT 0",
        "anim TEXT NOT NULL DEFAULT ''",
        "stages INTEGER NOT NULL DEFAULT 0",
        "fx TEXT NOT NULL DEFAULT ''",
    ] {
        let _ = sqlx::query(&format!("ALTER TABLE cfg_skills ADD COLUMN {col}"))
            .execute(pool)
            .await;
    }
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
        "SELECT template, name, slot, attack, magic, spirit, defense, hp, image, shape, price
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
        magic: r.get::<i64, _>("magic") as i32,
        spirit: r.get::<i64, _>("spirit") as i32,
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
        "SELECT id, class, name, mp, cd_ms, level, range, self_cast, kind_type, p1, p2,
                max_level, train_base, level_bonus, icon, fx, fx_base, fx_frames, anim, stages
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
                "dot" => SkillKind::Dot {
                    tick_mult: p1,
                    secs: p2,
                },
                _ => SkillKind::Damage(p1),
            },
            max_level: r.get::<i64, _>("max_level").max(0) as u32,
            train_base: r.get::<i64, _>("train_base").max(1) as u32,
            level_bonus: r.get("level_bonus"),
            icon: r.get::<i64, _>("icon").max(0) as u32,
            fx: r.get("fx"),
            fx_base: r.get::<i64, _>("fx_base").max(0) as u32,
            fx_frames: r.get::<i64, _>("fx_frames").clamp(0, 255) as u8,
            anim: r.get("anim"),
            stages: r.get::<i64, _>("stages").clamp(0, 3) as u8,
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

    let monsters = sqlx::query(
        "SELECT id, name, image, base, hp, damage, exp, passive, drops FROM cfg_monsters ORDER BY ord, id",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| crate::game::MonsterDef {
        id: r.get("id"),
        name: r.get("name"),
        image: r.get::<i64, _>("image") as u16,
        base: r.get::<i64, _>("base").max(0) as u32,
        hp: r.get::<i64, _>("hp") as i32,
        damage: r.get::<i64, _>("damage") as i32,
        exp: r.get::<i64, _>("exp").max(0) as u64,
        passive: r.get::<i64, _>("passive") != 0,
        drops: serde_json::from_str(&r.get::<String, _>("drops")).unwrap_or_default(),
    })
    .collect();

    Ok(GameData {
        items,
        skills,
        quests,
        npcs,
        bosses,
        monsters,
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
            "INSERT INTO cfg_items (template, name, slot, attack, magic, spirit, defense, hp, image, shape, price, ord)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&d.template)
        .bind(&d.name)
        .bind(&d.slot)
        .bind(d.attack as i64)
        .bind(d.magic as i64)
        .bind(d.spirit as i64)
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
                SkillKind::Dot { tick_mult, secs } => ("dot", *tick_mult, *secs),
            };
            sqlx::query(
                "INSERT INTO cfg_skills
                 (id, class, name, mp, cd_ms, level, range, self_cast, kind_type, p1, p2,
                  max_level, train_base, level_bonus, icon, fx, fx_base, fx_frames, anim, stages, ord)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
            .bind(s.max_level as i64)
            .bind(s.train_base as i64)
            .bind(s.level_bonus)
            .bind(s.icon as i64)
            .bind(&s.fx)
            .bind(s.fx_base as i64)
            .bind(s.fx_frames as i64)
            .bind(&s.anim)
            .bind(s.stages as i64)
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

pub async fn save_monsters(
    pool: &SqlitePool,
    monsters: &[crate::game::MonsterDef],
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM cfg_monsters")
        .execute(&mut *tx)
        .await?;
    for (i, m) in monsters.iter().enumerate() {
        sqlx::query(
            "INSERT INTO cfg_monsters (id, name, image, base, hp, damage, exp, passive, drops, ord)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&m.id)
        .bind(&m.name)
        .bind(m.image as i64)
        .bind(m.base as i64)
        .bind(m.hp as i64)
        .bind(m.damage as i64)
        .bind(m.exp as i64)
        .bind(m.passive as i64)
        .bind(serde_json::to_string(&m.drops).unwrap_or_else(|_| "[]".into()))
        .bind(i as i64)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// 旧库迁移: cfg_monsters 为空时, 从既有刷新点行蒸馏出怪物模板
/// (同名模板取首见的数值与掉落), 幂等 — 蒸馏过一次后不再动
pub async fn migrate_monsters(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM cfg_monsters")
        .fetch_one(pool)
        .await?;
    if n > 0 {
        return Ok(());
    }
    let rows = sqlx::query(
        "SELECT id, template, image, hp, damage, exp, passive FROM cfg_spawns ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<crate::game::MonsterDef> = Vec::new();
    for r in rows {
        let template: String = r.get("template");
        if template.is_empty() || !seen.insert(template.clone()) {
            continue;
        }
        let spawn_id: i64 = r.get("id");
        let drops = sqlx::query("SELECT item, chance FROM cfg_drops WHERE spawn_id = ?")
            .bind(spawn_id)
            .fetch_all(pool)
            .await?
            .into_iter()
            .map(|d| crate::game::DropSidecar {
                item: d.get("item"),
                chance: d.get("chance"),
            })
            .collect();
        out.push(crate::game::MonsterDef {
            name: template.clone(),
            id: template,
            image: r.get::<i64, _>("image") as u16,
            base: 0,
            hp: r.get::<i64, _>("hp") as i32,
            damage: r.get::<i64, _>("damage") as i32,
            exp: r.get::<i64, _>("exp").max(0) as u64,
            passive: r.get::<i64, _>("passive") != 0,
            drops,
        });
    }
    if !out.is_empty() {
        tracing::info!("怪物模板迁移: 从刷新点蒸馏 {} 个模板", out.len());
        save_monsters(pool, &out).await?;
    }
    Ok(())
}

/// 旧库迁移: 技能图标/特效此前硬编码在客户端, 配置化后按原映射补入库
/// (仅当全部技能 icon=0 时执行一次, 幂等)
pub async fn migrate_skill_fx(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM cfg_skills WHERE icon != 0 OR fx_frames != 0")
            .fetch_one(pool)
            .await?;
    if n > 0 {
        return Ok(());
    }
    // (id, 图标帧, 特效库, 起始帧, 帧数) — 客户端原硬编码表原样搬迁
    let map: [(&str, i64, i64, i64, i64); 9] = [
        ("huoqiu", 8, 0, 170, 10),
        ("zhiyu", 281, 0, 250, 20),
        ("shidu", 161, 0, 600, 20),
        ("huofu", 125, 0, 1320, 16),
        ("leidian", 92, 0, 880, 10),
        ("bingpaoxiao", 534, 1, 580, 8),
        ("liehuo", 433, 0, 3500, 8),
        ("shizihou", 536, 1, 650, 10),
        ("yeman", 399, 1, 0, 18),
    ];
    let mut hits = 0;
    for (id, icon, lib, base, frames) in map {
        let r = sqlx::query(
            "UPDATE cfg_skills SET icon = ?, fx_lib = ?, fx_base = ?, fx_frames = ? WHERE id = ?",
        )
        .bind(icon)
        .bind(lib)
        .bind(base)
        .bind(frames)
        .bind(id)
        .execute(pool)
        .await?;
        hits += r.rows_affected();
    }
    if hits > 0 {
        tracing::info!("技能图标/特效迁移: 补齐 {hits} 个技能的原映射");
    }
    Ok(())
}

/// 旧代用图标升级: 早期图标库是从特效帧生成的 9 帧代用品 (帧 0-8),
/// 换成购买图标库后按语义映射升级。仅命中"仍配着旧代用帧号"的技能, 幂等
/// 特效切换单技能标准文件 (magic/010+, 起手@0/飞行@10/命中@170):
/// 仅命中"仍指向素材源合集库旧段"的技能, 幂等
pub async fn migrate_skill_fx_split(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    // (id, 旧库, 旧命中基址, 新库) — 命中基址统一迁到 170。
    // 单技能文件从 100 起编号, 与素材源合集 (000-009) 拉开, 拆完好删旧库;
    // 第二组条目兜底曾短暂用过 010-018 编号的库
    let map: [(&str, i64, i64, i64); 18] = [
        ("huoqiu", 0, 170, 100),
        ("zhiyu", 0, 250, 101),
        ("shidu", 0, 600, 102),
        ("huofu", 0, 1320, 103),
        ("leidian", 0, 880, 104),
        ("bingpaoxiao", 1, 580, 105),
        ("liehuo", 0, 3500, 106),
        ("shizihou", 1, 650, 107),
        ("yeman", 1, 0, 108),
        ("huoqiu", 10, 170, 100),
        ("zhiyu", 11, 170, 101),
        ("shidu", 12, 170, 102),
        ("huofu", 13, 170, 103),
        ("leidian", 14, 170, 104),
        ("bingpaoxiao", 15, 170, 105),
        ("liehuo", 16, 170, 106),
        ("shizihou", 17, 170, 107),
        ("yeman", 18, 170, 108),
    ];
    let mut hits = 0;
    for (id, old_lib, old_base, new_lib) in map {
        let r = sqlx::query(
            "UPDATE cfg_skills SET fx_lib = ?, fx_base = 170
             WHERE id = ? AND fx_lib = ? AND fx_base = ?",
        )
        .bind(new_lib)
        .bind(id)
        .bind(old_lib)
        .bind(old_base)
        .execute(pool)
        .await?;
        hits += r.rows_affected();
    }
    if hits > 0 {
        tracing::info!("技能特效迁移: {hits} 个技能切换到单技能标准文件");
    }
    Ok(())
}

/// 公告条目 (登录器新闻栏与管理台共用)
#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct NewsItem {
    #[serde(default)]
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub created_at: String,
}

/// 公告全量 (置顶优先, 新的在前)
pub async fn load_news(pool: &SqlitePool) -> Result<Vec<NewsItem>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, title, body, pinned, created_at FROM cfg_news
         ORDER BY pinned DESC, id DESC",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| NewsItem {
            id: r.get("id"),
            title: r.get("title"),
            body: r.get("body"),
            pinned: r.get::<i64, _>("pinned") != 0,
            created_at: r.get("created_at"),
        })
        .collect())
}

/// 公告全量替换 (管理台保存; 保留原 id 无意义, 重建即可)
pub async fn save_news(pool: &SqlitePool, items: &[NewsItem]) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM cfg_news")
        .execute(&mut *tx)
        .await?;
    for n in items {
        sqlx::query(
            "INSERT INTO cfg_news (title, body, pinned, created_at)
             VALUES (?, ?, ?, COALESCE(NULLIF(?, ''), datetime('now', 'localtime')))",
        )
        .bind(&n.title)
        .bind(&n.body)
        .bind(n.pinned as i64)
        .bind(&n.created_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// 施毒术改持续毒伤 (仅当仍是单体伤害时切换, 幂等):
/// 每跳 = 攻击 × 0.6 × 修炼加成, 2 秒/跳, 持续 10 秒
pub async fn migrate_shidu_dot(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let r = sqlx::query(
        "UPDATE cfg_skills SET kind_type = 'dot', p1 = 0.6, p2 = 10
         WHERE id = 'shidu' AND kind_type = 'damage'",
    )
    .execute(pool)
    .await?;
    if r.rows_affected() > 0 {
        tracing::info!("施毒术已切换为持续毒伤");
    }
    Ok(())
}

/// 特效名字化: fx 为空时按旧数字 fx_lib 回填 —— 1xx 单技能文件映射为
/// 技能英文名, 其余数字原样转字符串 (旧编号库继续可用)。幂等。
/// 随后把道士三技能切到新购素材 (fx 不同名才切, 切过不再动)
pub async fn migrate_skill_fx_named(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let named: [(i64, &str); 9] = [
        (100, "huoqiu"),
        (101, "zhiyu"),
        (102, "shidu"),
        (103, "huofu"),
        (104, "leidian"),
        (105, "bingpaoxiao"),
        (106, "liehuo"),
        (107, "shizihou"),
        (108, "yeman"),
    ];
    let mut hits = 0;
    // 新购道士素材先行切换 (必须在名字回填之前 —— 回填一旦占名,
    // "fx 不同名才切"的幂等条件就再也不会命中, 新素材参数落不下去):
    // 治愈 1 段 12 帧 / 施毒 2 段 10 帧 / 火符 3 段 10 帧
    let fresh: [(&str, i64, i64); 3] = [("zhiyu", 12, 1), ("shidu", 10, 2), ("huofu", 10, 3)];
    for (id, frames, stages) in fresh {
        let r = sqlx::query(
            "UPDATE cfg_skills SET fx = ?, fx_base = 170, fx_frames = ?, stages = ?
             WHERE id = ? AND fx != ?",
        )
        .bind(id)
        .bind(frames)
        .bind(stages)
        .bind(id)
        .bind(id)
        .execute(pool)
        .await?;
        hits += r.rows_affected();
    }
    for (lib, name) in named {
        let r = sqlx::query("UPDATE cfg_skills SET fx = ? WHERE fx = '' AND fx_lib = ?")
            .bind(name)
            .bind(lib)
            .execute(pool)
            .await?;
        hits += r.rows_affected();
    }
    let r = sqlx::query(
        "UPDATE cfg_skills SET fx = CAST(fx_lib AS TEXT) WHERE fx = '' AND fx_frames > 0",
    )
    .execute(pool)
    .await?;
    hits += r.rows_affected();
    if hits > 0 {
        tracing::info!("特效名字化迁移: {hits} 处配置切换");
    }
    Ok(())
}

/// 技能类型回填: 火球/火符=三段(飞行), 雷电=二段(起手), 其余=一段 (仅填 0 值, 幂等)
pub async fn migrate_skill_stages(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let mut hits = 0;
    for (id, st) in [("huoqiu", 3i64), ("huofu", 3), ("leidian", 2)] {
        let r = sqlx::query("UPDATE cfg_skills SET stages = ? WHERE id = ? AND stages = 0")
            .bind(st)
            .bind(id)
            .execute(pool)
            .await?;
        hits += r.rows_affected();
    }
    let r = sqlx::query("UPDATE cfg_skills SET stages = 1 WHERE stages = 0")
        .execute(pool)
        .await?;
    hits += r.rows_affected();
    if hits > 0 {
        tracing::info!("技能类型回填: {hits} 个技能设定段数");
    }
    Ok(())
}

/// 施放动作回填: 战士技能挥砍, 法师/道士技能施法 (仅填空值, 幂等)
pub async fn migrate_skill_anim(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let r = sqlx::query(
        "UPDATE cfg_skills SET anim = CASE WHEN class = 'warrior' THEN 'attack' ELSE 'cast' END
         WHERE anim = ''",
    )
    .execute(pool)
    .await?;
    if r.rows_affected() > 0 {
        tracing::info!(
            "技能动作回填: {} 个技能按职业设定挥砍/施法",
            r.rows_affected()
        );
    }
    Ok(())
}

pub async fn migrate_skill_icons_v2(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let map: [(&str, i64, i64); 9] = [
        ("huoqiu", 0, 8),
        ("zhiyu", 1, 281),
        ("shidu", 2, 161),
        ("huofu", 3, 125),
        ("leidian", 4, 92),
        ("bingpaoxiao", 5, 534),
        ("liehuo", 6, 433),
        ("shizihou", 7, 536),
        ("yeman", 8, 399),
    ];
    let mut hits = 0;
    for (id, old_icon, new_icon) in map {
        let r = sqlx::query("UPDATE cfg_skills SET icon = ? WHERE id = ? AND icon = ?")
            .bind(new_icon)
            .bind(id)
            .bind(old_icon)
            .execute(pool)
            .await?;
        hits += r.rows_affected();
    }
    if hits > 0 {
        tracing::info!("技能图标升级: {hits} 个技能从代用图标换到购买图标库");
    }
    Ok(())
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
    save_monsters(pool, &data.monsters).await?;
    for (map, sc) in zones {
        save_zone(pool, map, sc).await?;
    }
    Ok(())
}
