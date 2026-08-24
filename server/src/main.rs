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

mod db;
mod game;
mod gateway;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use sim::WalkGrid;
use tracing::{info, warn};

#[derive(Deserialize, Default)]
struct ZoneSidecar {
    name: Option<String>,
    spawn: Option<(f64, f64)>,
    #[serde(default)]
    portals: Vec<PortalSidecar>,
    #[serde(default)]
    monsters: Vec<MonsterSidecar>,
}

#[derive(Deserialize)]
struct PortalSidecar {
    x: f64,
    y: f64,
    to: String,
    to_x: Option<f64>,
    to_y: Option<f64>,
}

#[derive(Deserialize)]
struct MonsterSidecar {
    template: String,
    /// 客户端怪物图库号 (Data/Monster/{image:03}.Lib)
    image: u16,
    x: f64,
    y: f64,
    #[serde(default = "one")]
    count: u32,
    /// 被动怪 (不主动仇恨, 如鸡/鹿)
    #[serde(default)]
    passive: bool,
    #[serde(default = "default_hp")]
    hp: i32,
    #[serde(default)]
    damage: i32,
    #[serde(default = "default_exp")]
    exp: u64,
    #[serde(default = "default_radius")]
    radius: f64,
}

fn one() -> u32 {
    1
}
fn default_radius() -> f64 {
    5.0
}
fn default_hp() -> i32 {
    30
}
fn default_exp() -> u64 {
    10
}

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

fn load_zone(
    idx: &mir_formats::scan::ResourceIndex,
    map_name: &str,
    sidecar: ZoneSidecar,
) -> Option<game::Zone> {
    let entry = idx.maps.iter().find(|m| {
        m.path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(map_name))
    })?;
    let map = mir_formats::map::parse(&std::fs::read(&entry.path).ok()?).ok()?;
    let walk = WalkGrid::from_cells(map.width, map.height, |x, y| {
        map.cell(x, y).is_some_and(|c| c.blocked)
    });
    let want = sidecar
        .spawn
        .unwrap_or((map.width as f64 / 2.0, map.height as f64 / 2.0));
    let spawn = game::nearest_walkable(&walk, want.0, want.1);
    let name = sidecar.name.unwrap_or_else(|| map_name.to_string());
    info!(
        "区域 {name} [{map_name}] ({:?} {}x{}), 出生点 ({:.1},{:.1}), 传送门 {}",
        map.kind,
        map.width,
        map.height,
        spawn.0,
        spawn.1,
        sidecar.portals.len()
    );
    Some(game::Zone {
        id: map_name.to_string(),
        name,
        walk,
        spawn,
        portals: sidecar
            .portals
            .into_iter()
            .map(|p| game::Portal {
                x: p.x,
                y: p.y,
                to_zone: p.to,
                to_x: p.to_x,
                to_y: p.to_y,
            })
            .collect(),
        monster_spawns: sidecar
            .monsters
            .into_iter()
            .map(|m| game::MonsterSpawn {
                template: m.template,
                image: m.image,
                x: m.x,
                y: m.y,
                count: m.count,
                radius: m.radius,
                passive: m.passive,
                hp: m.hp,
                damage: m.damage,
                exp: m.exp,
            })
            .collect(),
    })
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
            let sidecar = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok());
            let Some(sidecar) = sidecar else {
                warn!("边车解析失败, 跳过: {path:?}");
                continue;
            };
            match load_zone(&idx, &map_name, sidecar) {
                Some(z) => {
                    zones.insert(map_name, z);
                }
                None => warn!("区域加载失败 (找不到或解析失败): {map_name}"),
            }
        }
    }
    // 缺省区域必须存在 (无边车也拉起, 保底可玩)
    if !zones.contains_key(&default_zone) {
        match load_zone(&idx, &default_zone, ZoneSidecar::default()) {
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
    let game = game::Game::new(zones, default_zone, db, sessions);
    tokio::spawn(game.run(events));
    Arc::new(gw).listen(&addr).await.expect("网关监听失败");
}
