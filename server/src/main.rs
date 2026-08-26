//! MirForge 权威服务器（M2 基座）。
//!
//! 启动：边车目录（zones/*.json）决定加载哪些区域；每个区域直读原版 .map
//! 构建行走网格（与客户端同一 sim 判定）；WebSocket 网关 + 20Hz 游戏循环。
//!
//! 环境变量：
//! - `MIRFORGE_RES`   资源根目录（必需，含 Map/）
//! - `MIRFORGE_ZONES` 边车目录（默认找 `zones/` 或 `server/zones/`）
//! - `MIRFORGE_MAP`   缺省区域（新角色出生地，默认 `0.map`）
//! - `MIRFORGE_ADDR`  监听地址（默认 `127.0.0.1:4000`）
//! - `MIRFORGE_DB`    SQLite 路径（默认 `mirforge.db`）
//!
//! 边车 `zones/<地图文件>.json`：
//! ```json
//! { "name": "比奇省", "spawn": [330.5, 150.5],
//!   "portals": [{ "x": 334.5, "y": 154.5, "to": "2.map" }] }
//! ```
//! `spawn` 省略取地图中心；出生点与传送落点都会自动吸附到最近可走格。

mod admin;
mod db;
mod game;
mod gateway;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::{info, warn};

fn zones_dir() -> PathBuf {
    if let Ok(d) = std::env::var("MIRFORGE_ZONES") {
        return PathBuf::from(d);
    }
    for c in ["zones", "server/zones"] {
        if Path::new(c).is_dir() {
            return PathBuf::from(c);
        }
    }
    PathBuf::from("zones")
}

fn map_path_of(idx: &mir_formats::scan::ResourceIndex, map_name: &str) -> Option<PathBuf> {
    idx.maps
        .iter()
        .find(|m| {
            m.path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(map_name))
        })
        .map(|m| m.path.clone())
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let root = std::env::var("MIRFORGE_RES").unwrap_or_else(|_| {
        eprintln!("请设置 MIRFORGE_RES 指向传奇资源目录");
        std::process::exit(2);
    });
    let default_zone = std::env::var("MIRFORGE_MAP").unwrap_or_else(|_| "0.map".into());
    let addr = std::env::var("MIRFORGE_ADDR").unwrap_or_else(|_| "127.0.0.1:4000".into());
    let db_path = std::env::var("MIRFORGE_DB").unwrap_or_else(|_| "mirforge.db".into());

    // 数据配置 (物品/技能/任务): MIRFORGE_DATA 或 data/、server/data/; 缺省内置
    let data_dir = std::env::var("MIRFORGE_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            ["data", "server/data"]
                .iter()
                .map(PathBuf::from)
                .find(|p| p.is_dir())
                .unwrap_or_else(|| PathBuf::from("data"))
        });
    game::init_data(&data_dir);

    let idx = mir_formats::scan::ResourceIndex::scan(Path::new(&root));
    // 边车目录里的每个 <map>.json 就是一个区域
    let dir = zones_dir();
    let mut zones: HashMap<String, game::Zone> = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for f in rd.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(map_name) = path.file_stem().and_then(|s| s.to_str()).map(String::from) else {
                continue;
            };
            let sidecar: Option<game::ZoneSidecar> = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok());
            let Some(sidecar) = sidecar else {
                warn!("边车解析失败, 跳过: {path:?}");
                continue;
            };
            let loaded =
                map_path_of(&idx, &map_name).and_then(|p| game::load_zone(&p, &map_name, sidecar));
            match loaded {
                Some(z) => {
                    zones.insert(map_name, z);
                }
                None => warn!("区域加载失败 (找不到或解析失败): {map_name}"),
            }
        }
    }
    // 缺省区域必须存在 (无边车也拉起, 保底可玩)
    if !zones.contains_key(&default_zone) {
        let loaded = map_path_of(&idx, &default_zone)
            .and_then(|p| game::load_zone(&p, &default_zone, game::ZoneSidecar::default()));
        match loaded {
            Some(z) => {
                zones.insert(default_zone.clone(), z);
            }
            None => {
                eprintln!("资源目录中找不到缺省地图 {default_zone}");
                std::process::exit(2);
            }
        }
    }
    // 传送门指向的区域必须已加载
    for z in zones.values() {
        for p in &z.portals {
            if !zones.contains_key(&p.to_zone) {
                warn!("{} 的传送门指向未加载区域 {}", z.id, p.to_zone);
            }
        }
    }
    info!("共加载 {} 个区域, 缺省 {default_zone}", zones.len());

    let db = db::Db::open(&db_path).await.expect("打开数据库失败");
    let (gw, events) = gateway::Gateway::new();
    let sessions = gw.sessions();
    // 全部可用 .map (管理台"新增地图"用)
    let map_files: HashMap<String, PathBuf> = idx
        .maps
        .iter()
        .filter_map(|m| {
            m.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| (n.to_lowercase(), m.path.clone()))
        })
        .collect();
    let game = game::Game::new(zones, default_zone, db, sessions, map_files, dir.clone());
    let admin_rx = admin::spawn();
    tokio::spawn(game.run(events, admin_rx));
    Arc::new(gw).listen(&addr).await.expect("网关监听失败");
}
