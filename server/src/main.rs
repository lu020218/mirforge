//! MirForge 权威服务器（M2 基座）。
//!
//! 启动：直读原版 .map 构建行走网格（与客户端同一 sim 判定），JSON 边车提供
//! 出生点等运营配置；WebSocket 网关 + 20Hz 游戏循环。
//!
//! 环境变量：
//! - `MIRFORGE_RES`   资源根目录（必需，含 Map/）
//! - `MIRFORGE_MAP`   地图文件名（默认 `0.map`）
//! - `MIRFORGE_ADDR`  监听地址（默认 `127.0.0.1:4000`）
//! - `MIRFORGE_DB`    SQLite 路径（默认 `mirforge.db`）
//!
//! 边车：`<地图文件>.json`（与 .map 同目录）或仓库 `server/zones/<地图文件>.json`，
//! 字段 `{ "name": "...", "spawn": [x, y] }`；缺省出生点 (330.5, 150.5)。

mod db;
mod game;
mod gateway;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use sim::WalkGrid;
use tracing::info;

#[derive(Deserialize, Default)]
struct ZoneSidecar {
    name: Option<String>,
    spawn: Option<(f64, f64)>,
}

fn load_sidecar(map_path: &Path) -> ZoneSidecar {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut with_json = map_path.as_os_str().to_owned();
    with_json.push(".json");
    candidates.push(PathBuf::from(with_json));
    if let Some(fname) = map_path.file_name() {
        for base in ["zones", "server/zones"] {
            let mut zone = PathBuf::from(base).join(fname);
            zone.as_mut_os_string().push(".json");
            candidates.push(zone);
        }
    }
    for c in candidates {
        if let Ok(text) = std::fs::read_to_string(&c) {
            match serde_json::from_str(&text) {
                Ok(s) => {
                    info!("边车已加载: {c:?}");
                    return s;
                }
                Err(e) => tracing::warn!("边车解析失败 {c:?}: {e}"),
            }
        }
    }
    ZoneSidecar::default()
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let root = std::env::var("MIRFORGE_RES").unwrap_or_else(|_| {
        eprintln!("请设置 MIRFORGE_RES 指向传奇资源目录");
        std::process::exit(2);
    });
    let map_name = std::env::var("MIRFORGE_MAP").unwrap_or_else(|_| "0.map".into());
    let addr = std::env::var("MIRFORGE_ADDR").unwrap_or_else(|_| "127.0.0.1:4000".into());
    let db_path = std::env::var("MIRFORGE_DB").unwrap_or_else(|_| "mirforge.db".into());

    let idx = mir_formats::scan::ResourceIndex::scan(Path::new(&root));
    let Some(entry) = idx.maps.iter().find(|m| {
        m.path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(&map_name))
    }) else {
        eprintln!("资源目录中找不到地图 {map_name}");
        std::process::exit(2);
    };
    let map = mir_formats::map::parse(&std::fs::read(&entry.path).expect("读地图失败"))
        .expect("解析地图失败");
    let walk = WalkGrid::from_cells(map.width, map.height, |x, y| {
        map.cell(x, y).is_some_and(|c| c.blocked)
    });
    let sidecar = load_sidecar(&entry.path);
    let spawn = sidecar.spawn.unwrap_or((330.5, 150.5));
    let zone = game::Zone {
        id: map_name.clone(),
        name: sidecar.name.unwrap_or_else(|| map_name.clone()),
        walk,
        spawn,
    };
    info!(
        "区域 {} ({:?} {}x{}), 出生点 ({:.1},{:.1})",
        zone.name, map.kind, map.width, map.height, spawn.0, spawn.1
    );

    let db = db::Db::open(&db_path).await.expect("打开数据库失败");
    let (gw, events) = gateway::Gateway::new();
    let sessions = gw.sessions();
    let game = game::Game::new(zone, db, sessions);
    tokio::spawn(game.run(events));
    Arc::new(gw).listen(&addr).await.expect("网关监听失败");
}
