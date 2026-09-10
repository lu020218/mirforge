//! MirForge 中心站 (hub)。
//!
//! 全区唯一的配置权威与统一管理后台: 游戏内容配置 (物品/技能/怪物/
//! 任务/区域) + 公告 + 更新包 + 区服注册表都在这里; 区服只存玩家数据,
//! 启动/热更时从本站拉配置快照 (见 `gamedata::defs::Snapshot`)。
//!
//! 环境变量:
//! - `MIRFORGE_HUB_ADDR`   监听地址 (默认 `127.0.0.1:4001`)
//! - `MIRFORGE_HUB_DB`     配置库路径 (默认 `hub.db`)
//! - `MIRFORGE_ADMIN_TOKEN` 运营鉴权; 绑非回环地址时必须设置
//! - `MIRFORGE_HUB_TOKEN`  区服内部通道密钥 (快照/心跳/变更订阅)
//! - `MIRFORGE_PACKS`      资源包根 (预览/地图校验; 默认 `packs/`)
//! - `MIRFORGE_UPDATES`    更新包目录 (默认 `updates/`)
//! - `MIRFORGE_DATA`       首次建库种子 JSON 目录 (默认 `server/data`)
//!
//! 子命令: `mirforge-hub import --from <旧区服.db>` — 把单机模式旧库的
//! 全部配置 (含手工调整) 整体搬进 hub 库并置 rev=1。

mod api;
mod assets;
mod map_render;
mod state;
mod world;

use axum::routing::{get, post};
use axum::Router;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tracing::{error, info};

async fn open_db(path: &str) -> Result<SqlitePool, sqlx::Error> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5));
    SqlitePoolOptions::new().connect_with(opts).await
}

/// hub 专属表 (区服注册表; cfg_* 与 rev 走 gamedata)
async fn ensure_hub_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS servers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            game_addr TEXT NOT NULL DEFAULT '',
            internal_addr TEXT NOT NULL DEFAULT '',
            sort INTEGER NOT NULL DEFAULT 0,
            hidden INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// 建库 + 迁移链 + (库为空时) 种子导入 — 与区服单机模式同一套流程
async fn init_config(pool: &SqlitePool) {
    gamedata::store::ensure_schema(pool)
        .await
        .expect("建表失败");
    gamedata::store::ensure_rev(pool).await.expect("rev 表失败");
    ensure_hub_schema(pool).await.expect("注册表失败");
    gamedata::store::migrate_skill_fx(pool)
        .await
        .expect("迁移失败");
    gamedata::store::migrate_skill_icons_v2(pool)
        .await
        .expect("迁移失败");
    gamedata::store::migrate_skill_anim(pool)
        .await
        .expect("迁移失败");
    gamedata::store::migrate_skill_stages(pool)
        .await
        .expect("迁移失败");
    gamedata::store::migrate_skill_fx_split(pool)
        .await
        .expect("迁移失败");
    gamedata::store::migrate_skill_fx_named(pool)
        .await
        .expect("迁移失败");
    gamedata::store::migrate_shidu_dot(pool)
        .await
        .expect("迁移失败");
    gamedata::store::migrate_kind_s1(pool)
        .await
        .expect("迁移失败");
    gamedata::store::migrate_yeman_charge(pool)
        .await
        .expect("迁移失败");
    gamedata::store::migrate_yeman_fx(pool)
        .await
        .expect("迁移失败");
    gamedata::store::migrate_zhaohuan(pool)
        .await
        .expect("召唤骷髅迁移失败");
    gamedata::store::migrate_zhiyu_range(pool)
        .await
        .expect("治愈射程迁移失败");
    if gamedata::store::is_empty(pool).await.unwrap_or(false) {
        let data_dir = std::env::var("MIRFORGE_DATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("server/data"));
        let seed = gamedata::defs::GameData::seed_source(&data_dir);
        let mut zones = std::collections::HashMap::new();
        zones.insert(
            std::env::var("MIRFORGE_MAP")
                .unwrap_or_else(|_| "0.map".into())
                .to_lowercase(),
            gamedata::defs::ZoneSidecar::default(),
        );
        gamedata::store::seed_from_files(pool, &seed, &zones)
            .await
            .expect("种子导入失败");
        info!(
            "配置库初始化: 物品 {} / 技能 {} / 任务 {}",
            seed.items.len(),
            seed.skills.warrior.len() + seed.skills.mage.len() + seed.skills.taoist.len(),
            seed.quests.len()
        );
    }
}

/// `import --from <旧库>`: 旧区服库的配置整体搬入 hub (含手工调整)
async fn import_from(pool: &SqlitePool, src_path: &str) {
    let src = open_db(src_path).await.expect("打开旧库失败");
    let data = gamedata::store::load_game_data(&src)
        .await
        .expect("读取旧库配置失败");
    let zones = gamedata::store::load_zone_sidecars(&src)
        .await
        .expect("读取旧库区域失败");
    let news = gamedata::store::load_news(&src).await.unwrap_or_default();
    gamedata::store::save_items(pool, &data.items)
        .await
        .expect("items");
    gamedata::store::save_skills(pool, &data.skills)
        .await
        .expect("skills");
    gamedata::store::save_quests(pool, &data.quests)
        .await
        .expect("quests");
    gamedata::store::save_npcs(pool, &data.npcs)
        .await
        .expect("npcs");
    gamedata::store::save_bosses(pool, &data.bosses)
        .await
        .expect("bosses");
    gamedata::store::save_monsters(pool, &data.monsters)
        .await
        .expect("monsters");
    for (map, sc) in &zones {
        gamedata::store::save_zone(pool, map, sc)
            .await
            .expect("zone");
    }
    gamedata::store::save_news(pool, &news).await.expect("news");
    let rev = gamedata::store::bump_rev(pool).await.expect("rev");
    info!(
        "导入完成: 物品 {} / 任务 {} / 区域 {} / 公告 {} 条, rev={rev}",
        data.items.len(),
        data.quests.len(),
        zones.len(),
        news.len()
    );
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let db_path = std::env::var("MIRFORGE_HUB_DB").unwrap_or_else(|_| "hub.db".into());
    let pool = open_db(&db_path).await.expect("打开配置库失败");
    init_config(&pool).await;

    // 子命令: import --from <旧库>
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("import") {
        let from = args
            .iter()
            .position(|a| a == "--from")
            .and_then(|i| args.get(i + 1))
            .expect("用法: mirforge-hub import --from <旧区服.db>");
        import_from(&pool, from).await;
        return;
    }

    let addr = std::env::var("MIRFORGE_HUB_ADDR").unwrap_or_else(|_| "127.0.0.1:4001".into());
    let token = std::env::var("MIRFORGE_ADMIN_TOKEN").ok();
    let hub_token = std::env::var("MIRFORGE_HUB_TOKEN").ok();
    let loopback = addr.starts_with("127.") || addr.starts_with("localhost");
    if token.is_none() && !loopback {
        error!("hub 绑定非本机地址必须设置 MIRFORGE_ADMIN_TOKEN, 已退出");
        return;
    }

    let (news_tx, _) = tokio::sync::broadcast::channel(16);
    let (cfg_tx, _) = tokio::sync::broadcast::channel(16);
    let state = state::AppState {
        pool,
        token,
        hub_token,
        news_tx,
        cfg_tx,
        beats: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    };

    let app = Router::new()
        .route("/", get(api::index))
        // 配置 (权威)
        .route(
            "/api/config",
            get(api::api_config_get).put(api::api_config_put),
        )
        .route(
            "/api/zones",
            get(api::api_zones_get)
                .put(api::api_zones_put)
                .post(api::api_zones_add)
                .delete(api::api_zones_delete),
        )
        // 公告
        .route("/api/news", get(api::api_news_get).put(api::api_news_put))
        .route("/api/public/news", get(api::api_public_news))
        .route("/api/public/news/ws", get(api::api_public_news_ws))
        // 区服注册表 + 总览
        .route(
            "/api/hub/servers",
            get(api::api_servers_get).put(api::api_servers_put),
        )
        .route("/api/hub/overview", get(api::api_overview))
        .route("/api/public/servers", get(api::api_public_servers))
        // 内部通道 (区服)
        .route("/internal/config/snapshot", get(api::api_internal_snapshot))
        .route("/internal/config/ws", get(api::api_internal_config_ws))
        .route("/internal/heartbeat", post(api::api_internal_heartbeat))
        // 运行时操作按服代理 (状态/踢人/广播/存盘)
        .route(
            "/srv/:id/api/*path",
            get(api::api_srv_proxy).post(api::api_srv_proxy),
        )
        // 更新包
        .route("/updates/*path", get(assets::api_update_file))
        // packs 预览 (管理台选择器)
        .route("/api/icons", get(assets::api_icons_grid))
        .route("/api/npcs/grid", get(assets::api_npc_grid))
        .route("/api/spritegrid/:kind", get(assets::api_sprite_grid))
        .route("/api/frame/:kind/:n", get(assets::api_frame_png))
        .route("/api/monster_bases/:n", get(assets::api_mon_bases))
        .route("/api/packs/bases", get(assets::api_packs_bases))
        .route("/api/packs/strip", get(assets::api_packs_strip))
        .route("/api/packs/list", get(assets::api_packs_list))
        .route("/api/packs/info", get(assets::api_packs_info))
        .route("/api/packs/frame", get(assets::api_packs_frame))
        .route("/api/minimaps", get(assets::api_minimap_grid))
        .route("/api/mapthumb/:map", get(api::api_map_thumb))
        .route("/api/maptile/:map/:tx/:ty", get(api::api_map_tile))
        .with_state(state);

    info!("hub 管理台: http://{addr}/  (配置库 {db_path})");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("绑定失败");
    axum::serve(listener, app).await.expect("服务异常退出");
}
