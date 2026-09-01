//! Web 管理控制台 (axum)。
//!
//! 默认监听 `127.0.0.1:4001` (仅本机); `MIRFORGE_ADMIN` 覆盖地址,
//! 设为 `off` 关闭。绑定非回环地址时必须设置 `MIRFORGE_ADMIN_TOKEN`,
//! 请求经 `x-admin-token` 头校验。
//!
//! 管理命令经 mpsc 注入游戏主循环 (与网关事件同一 select), 状态查询
//! 走 oneshot 请求-响应 —— 无锁, 不与 20Hz tick 抢状态。

use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

/// 管理命令 (游戏循环内处理)
pub enum AdminCmd {
    Status(oneshot::Sender<StatusSnapshot>),
    /// 配置已热替换 (通知在线玩家刷新技能表等)
    ConfigReloaded {
        kind: String,
    },
    Broadcast(String),
    Kick {
        name: String,
        done: oneshot::Sender<bool>,
    },
    SaveAll(oneshot::Sender<()>),
    /// 区域列表 + 可接入地图
    ZonesInfo(oneshot::Sender<ZonesInfo>),
    /// NPC/BOSS 落位校验 (地图已接入 / 坐标可走 / 传送目标合法)
    /// —— 走格与区域表只在游戏循环里, 所以单独问一次
    CheckPlacement {
        npcs: Vec<crate::game::NpcDef>,
        bosses: Vec<crate::game::BossDef>,
        quests: Vec<crate::game::QuestDef>,
        done: oneshot::Sender<Vec<String>>,
    },
    /// 边车更新 (校验/写回/热重载该区怪物)
    PutZone {
        map: String,
        sidecar: serde_json::Value,
        done: oneshot::Sender<Result<(), String>>,
    },
    /// 接入新地图 (默认边车)
    AddZone {
        map: String,
        done: oneshot::Sender<Result<(), String>>,
    },
    /// 移除区域 (仅在无人在场且无传送门指向时允许)
    DeleteZone {
        map: String,
        done: oneshot::Sender<Result<(), String>>,
    },
    /// 地图缩略图所需信息 (文件路径 + 标记点)
    MapThumbInfo {
        map: String,
        done: oneshot::Sender<Option<MapThumb>>,
    },
}

/// 缩略图渲染输入
pub struct MapThumb {
    pub path: std::path::PathBuf,
    pub spawn: (f64, f64),
    pub portals: Vec<(f64, f64)>,
    pub spawns: Vec<(f64, f64)>,
}

#[derive(Serialize)]
pub struct ZonesInfo {
    pub zones: Vec<ZoneRow>,
    pub available: Vec<String>,
}

#[derive(Serialize)]
pub struct ZoneRow {
    pub map: String,
    pub name: String,
    pub sidecar: serde_json::Value,
}

#[derive(Serialize)]
pub struct StatusSnapshot {
    pub uptime_secs: u64,
    pub players: Vec<PlayerRow>,
    pub monsters_alive: usize,
    pub monsters_total: usize,
    pub ground_items: usize,
    pub zones: Vec<String>,
}

#[derive(Serialize)]
pub struct PlayerRow {
    pub name: String,
    pub level: u32,
    pub zone: String,
    pub x: f64,
    pub y: f64,
    pub hp: i32,
    pub max_hp: i32,
    pub connected: bool,
}

#[derive(Clone)]
struct AppState {
    tx: mpsc::UnboundedSender<AdminCmd>,
    token: Option<String>,
    db: crate::db::Db,
}

fn authed(state: &AppState, headers: &HeaderMap) -> bool {
    match &state.token {
        None => true,
        Some(t) => headers
            .get("x-admin-token")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == t),
    }
}

#[derive(Deserialize)]
struct BroadcastReq {
    message: String,
}

#[derive(Deserialize)]
struct KickReq {
    name: String,
}

async fn api_status(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<StatusSnapshot>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (tx, rx) = oneshot::channel();
    st.tx
        .send(AdminCmd::Status(tx))
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    rx.await
        .map(Json)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

async fn api_broadcast(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<BroadcastReq>,
) -> Result<StatusCode, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    audit("broadcast", &req.message);
    st.tx
        .send(AdminCmd::Broadcast(req.message))
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn api_kick(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<KickReq>,
) -> Result<Json<bool>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (tx, rx) = oneshot::channel();
    st.tx
        .send(AdminCmd::Kick {
            name: req.name,
            done: tx,
        })
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    rx.await
        .map(Json)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

async fn api_save(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (tx, rx) = oneshot::channel();
    st.tx
        .send(AdminCmd::SaveAll(tx))
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    rx.await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct ConfigPayload {
    items: serde_json::Value,
    skills: serde_json::Value,
    quests: serde_json::Value,
    npcs: serde_json::Value,
    bosses: serde_json::Value,
    monsters: serde_json::Value,
}

async fn api_config_get(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ConfigPayload>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let d = crate::game::data();
    Ok(Json(ConfigPayload {
        items: serde_json::to_value(&d.items).unwrap_or_default(),
        skills: serde_json::to_value(&d.skills).unwrap_or_default(),
        quests: serde_json::to_value(&d.quests).unwrap_or_default(),
        npcs: serde_json::to_value(&d.npcs).unwrap_or_default(),
        bosses: serde_json::to_value(&d.bosses).unwrap_or_default(),
        monsters: serde_json::to_value(&d.monsters).unwrap_or_default(),
    }))
}

#[derive(Deserialize)]
struct PutConfigReq {
    kind: String,
    /// 对应类别的完整 JSON (与 server/data/*.json 同构)
    value: serde_json::Value,
}

#[derive(Serialize)]
struct PutConfigResp {
    ok: bool,
    errors: Vec<String>,
}

async fn api_config_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PutConfigReq>,
) -> Result<Json<PutConfigResp>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let fail = |e: String| {
        Json(PutConfigResp {
            ok: false,
            errors: vec![e],
        })
    };
    // 以当前生效配置为基, 替换目标类别 → 整体校验
    let cur = crate::game::data();
    let mut next = (*cur).clone();
    let parsed: Result<(), String> = (|| {
        match req.kind.as_str() {
            "items" => {
                next.items =
                    serde_json::from_value(req.value.clone()).map_err(|e| format!("items: {e}"))?
            }
            "skills" => {
                next.skills =
                    serde_json::from_value(req.value.clone()).map_err(|e| format!("skills: {e}"))?
            }
            "quests" => {
                next.quests =
                    serde_json::from_value(req.value.clone()).map_err(|e| format!("quests: {e}"))?
            }
            "npcs" => {
                next.npcs =
                    serde_json::from_value(req.value.clone()).map_err(|e| format!("npcs: {e}"))?
            }
            "bosses" => {
                next.bosses =
                    serde_json::from_value(req.value.clone()).map_err(|e| format!("bosses: {e}"))?
            }
            "monsters" => {
                next.monsters = serde_json::from_value(req.value.clone())
                    .map_err(|e| format!("monsters: {e}"))?
            }
            k => return Err(format!("未知配置类别: {k}")),
        }
        Ok(())
    })();
    if let Err(e) = parsed {
        return Ok(fail(e));
    }
    let mut errors = next.validate();
    if matches!(req.kind.as_str(), "npcs" | "bosses" | "quests") && errors.is_empty() {
        // 地图/走格只有游戏循环有, 单独问一次
        let (tx, rx) = oneshot::channel();
        if st
            .tx
            .send(AdminCmd::CheckPlacement {
                npcs: next.npcs.clone(),
                bosses: next.bosses.clone(),
                quests: next.quests.clone(),
                done: tx,
            })
            .is_ok()
        {
            errors.extend(rx.await.unwrap_or_default());
        }
    }
    if !errors.is_empty() {
        return Ok(Json(PutConfigResp { ok: false, errors }));
    }
    // 持久化到数据库 (整表事务替换)
    let pool = st.db.pool();
    let saved = match req.kind.as_str() {
        "items" => crate::config_store::save_items(pool, &next.items).await,
        "skills" => crate::config_store::save_skills(pool, &next.skills).await,
        "npcs" => crate::config_store::save_npcs(pool, &next.npcs).await,
        "bosses" => crate::config_store::save_bosses(pool, &next.bosses).await,
        "monsters" => crate::config_store::save_monsters(pool, &next.monsters).await,
        _ => crate::config_store::save_quests(pool, &next.quests).await,
    };
    if let Err(e) = saved {
        return Ok(fail(format!("保存失败: {e}")));
    }
    crate::game::set_data(next);
    let _ = st.tx.send(AdminCmd::ConfigReloaded {
        kind: req.kind.clone(),
    });
    audit("put_config", &req.kind);
    Ok(Json(PutConfigResp {
        ok: true,
        errors: Vec::new(),
    }))
}

async fn api_zones_get(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ZonesInfo>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (tx, rx) = oneshot::channel();
    st.tx
        .send(AdminCmd::ZonesInfo(tx))
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    rx.await
        .map(Json)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

#[derive(Deserialize)]
struct PutZoneReq {
    map: String,
    sidecar: serde_json::Value,
}

#[derive(Deserialize)]
struct AddZoneReq {
    map: String,
}

#[derive(Serialize)]
struct ZoneOpResp {
    ok: bool,
    error: Option<String>,
}

fn zone_resp(r: Result<(), String>) -> Json<ZoneOpResp> {
    match r {
        Ok(()) => Json(ZoneOpResp {
            ok: true,
            error: None,
        }),
        Err(e) => Json(ZoneOpResp {
            ok: false,
            error: Some(e),
        }),
    }
}

async fn api_zones_put(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<PutZoneReq>,
) -> Result<Json<ZoneOpResp>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (tx, rx) = oneshot::channel();
    st.tx
        .send(AdminCmd::PutZone {
            map: req.map,
            sidecar: req.sidecar,
            done: tx,
        })
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    rx.await
        .map(zone_resp)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

async fn api_zones_delete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AddZoneReq>,
) -> Result<Json<ZoneOpResp>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (tx, rx) = oneshot::channel();
    st.tx
        .send(AdminCmd::DeleteZone {
            map: req.map.clone(),
            done: tx,
        })
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let r = rx.await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if r.is_ok() {
        audit("delete_zone", &req.map);
    }
    Ok(zone_resp(r))
}

async fn api_zones_add(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<AddZoneReq>,
) -> Result<Json<ZoneOpResp>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (tx, rx) = oneshot::channel();
    st.tx
        .send(AdminCmd::AddZone {
            map: req.map,
            done: tx,
        })
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    rx.await
        .map(zone_resp)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

// ── 资源帧预览 (图标/外观选择器) ──

static PREVIEW_LIBS: std::sync::Mutex<
    Option<std::collections::HashMap<String, Option<mir_formats::mfl::AnyLib>>>,
> = std::sync::Mutex::new(None);

/// 自有资源包根 (与客户端同规则: MIRFORGE_PACKS 可覆盖, 默认工作目录 packs/)
fn packs_root() -> std::path::PathBuf {
    std::env::var("MIRFORGE_PACKS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("packs"))
}

/// 预览库白名单 (packs/ 内固定路径, 杜绝任意文件读取)。
/// Crystal 资源路线已废除, 预览只走 packs — mir-pack convert 负责转码。
fn preview_pack_path(kind: &str, n: u16) -> Option<std::path::PathBuf> {
    let rel = match kind {
        "items" => "items.mfl".to_string(),
        "weapon" => format!("weapon/{n:03}.mfl"),
        "armour" => format!("armor/{n:03}.mfl"),
        "monster" => format!("monster/{n:03}.mfl"),
        "minimap" => "mmap.mfl".to_string(),
        "npc" => format!("npc/{n:03}.mfl"),
        _ => return None,
    };
    Some(packs_root().join(rel))
}

fn with_preview_lib<R>(
    kind: &str,
    n: u16,
    f: impl FnOnce(&mir_formats::mfl::AnyLib) -> Option<R>,
) -> Option<R> {
    let key = format!("{kind}/{n}");
    let mut guard = PREVIEW_LIBS.lock().ok()?;
    let cache = guard.get_or_insert_with(Default::default);
    if !cache.contains_key(&key) {
        let lib = preview_pack_path(kind, n).and_then(|p| mir_formats::mfl::AnyLib::open(&p).ok());
        cache.insert(key.clone(), lib);
    }
    cache.get(&key).and_then(|l| l.as_ref()).and_then(f)
}

#[derive(Deserialize)]
struct IconsQuery {
    #[serde(default)]
    start: usize,
    #[serde(default = "default_icon_count")]
    count: usize,
}

fn default_icon_count() -> usize {
    100
}

/// 图标网格 PNG: 10 列 × 48px 单元, 棋盘底; 前端按坐标换算帧号
async fn api_icons_grid(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IconsQuery>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let count = q.count.min(400);
    let cols = 10usize;
    let cell = 48u32;
    let rows = count.div_ceil(cols).max(1);
    let png = tokio::task::spawn_blocking(move || {
        let mut canvas = image::RgbaImage::new(cols as u32 * cell, rows as u32 * cell);
        for (x, y, p) in canvas.enumerate_pixels_mut() {
            let dark = ((x / 8) + (y / 8)) % 2 == 0;
            *p = image::Rgba(if dark {
                [26, 28, 40, 255]
            } else {
                [34, 36, 50, 255]
            });
        }
        with_preview_lib("items", 0, |lib| {
            for i in 0..count {
                let Ok(Some(img)) = lib.image(q.start + i) else {
                    continue;
                };
                let (ox, oy) = (((i % cols) as u32) * cell, ((i / cols) as u32) * cell);
                // 居中放置, 超出裁剪
                let (w, h) = (img.width as u32, img.height as u32);
                let dx = ox + cell.saturating_sub(w) / 2;
                let dy = oy + cell.saturating_sub(h) / 2;
                for y in 0..h.min(cell) {
                    for x in 0..w.min(cell) {
                        let si = ((y * w + x) * 4) as usize;
                        let px = &img.rgba[si..si + 4];
                        if px[3] > 0 && dx + x < canvas.width() && dy + y < canvas.height() {
                            canvas.put_pixel(
                                dx + x,
                                dy + y,
                                image::Rgba([px[0], px[1], px[2], px[3]]),
                            );
                        }
                    }
                }
            }
            Some(())
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        let _ = canvas.write_to(&mut buf, image::ImageFormat::Png);
        buf.into_inner()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=3600")
        .body(axum::body::Body::from(png))
        .unwrap())
}

/// NPC 形象图库: 每格一个 NPC 库 (Data/NPC/{n:02}.Lib 的站立首帧)
///
/// 与 `/api/icons` 不同 — 那里是同一个库里的连续帧, 这里是逐个库取首帧。
async fn api_npc_grid(
    st: State<AppState>,
    headers: HeaderMap,
    q: Query<IconsQuery>,
) -> Result<axum::response::Response, StatusCode> {
    api_sprite_grid(st, headers, AxPath("npc".to_string()), q).await
}

/// 精灵形象网格: 每格一个库的站立首帧 (kind = npc / monster)
async fn api_sprite_grid(
    State(st): State<AppState>,
    headers: HeaderMap,
    AxPath(kind): AxPath<String>,
    Query(q): Query<IconsQuery>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if !matches!(kind.as_str(), "npc" | "monster" | "weapon" | "armour") {
        return Err(StatusCode::NOT_FOUND);
    }
    let count = q.count.min(64);
    let (cols, cw, ch) = (8usize, 96u32, 120u32);
    let rows = count.div_ceil(cols).max(1);
    let start = q.start;
    // 武器/衣甲按 packs 布局取朝南站立帧 32 (站 0+dir*8, dir4=南);
    // npc/monster 库首帧常是占位小图, 取不到像样的就向后扫第一个实帧
    let frame_idx = if matches!(kind.as_str(), "weapon" | "armour") {
        32
    } else {
        0
    };
    let png = tokio::task::spawn_blocking(move || {
        let mut canvas = image::RgbaImage::new(cols as u32 * cw, rows as u32 * ch);
        for (x, y, p) in canvas.enumerate_pixels_mut() {
            let dark = ((x / 8) + (y / 8)) % 2 == 0;
            *p = image::Rgba(if dark {
                [26, 28, 40, 255]
            } else {
                [34, 36, 50, 255]
            });
        }
        for i in 0..count {
            let n = (start + i) as u16;
            let (ox, oy) = (((i % cols) as u32) * cw, ((i / cols) as u32) * ch);
            with_preview_lib(&kind, n, |lib| {
                let img = lib
                    .image(frame_idx)
                    .ok()
                    .flatten()
                    .filter(|f| f.width >= 12 && f.height >= 12)
                    .or_else(|| {
                        (0..lib.len().min(900)).find_map(|i| {
                            lib.image(i)
                                .ok()
                                .flatten()
                                .filter(|f| f.width >= 12 && f.height >= 12)
                        })
                    })?;
                let (w, h) = (img.width as u32, img.height as u32);
                // 水平居中, 垂直贴底 (NPC 立绘基准在脚下)
                let dx = ox + cw.saturating_sub(w) / 2;
                let dy = oy + ch.saturating_sub(h.min(ch));
                for y in 0..h.min(ch) {
                    for x in 0..w.min(cw) {
                        let si = ((y * w + x) * 4) as usize;
                        let px = &img.rgba[si..si + 4];
                        if px[3] > 0 && dx + x < canvas.width() && dy + y < canvas.height() {
                            canvas.put_pixel(
                                dx + x,
                                dy + y,
                                image::Rgba([px[0], px[1], px[2], px[3]]),
                            );
                        }
                    }
                }
                Some(())
            });
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        let _ = canvas.write_to(&mut buf, image::ImageFormat::Png);
        buf.into_inner()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=3600")
        .body(axum::body::Body::from(png))
        .unwrap())
}

/// 单帧 PNG: items 图标 / weapon-armour 站立帧 (帧 16 = 朝南)
#[derive(Deserialize)]
struct FrameQuery {
    /// 怪物库内基址 (一库多怪时从该帧起找代表帧)
    #[serde(default)]
    base: u32,
}

async fn api_frame_png(
    State(st): State<AppState>,
    headers: HeaderMap,
    AxPath((kind, n)): AxPath<(String, u16)>,
    Query(fq): Query<FrameQuery>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let png = tokio::task::spawn_blocking(move || {
        let (lib_n, frame) = match kind.as_str() {
            "items" => (0u16, n as usize),
            // 外观预览: 该库朝南站立首帧 (packs 布局 0+dir*8, dir4=南)
            "weapon" | "armour" => (n, 32usize),
            // 怪物: 取不到就靠下面的扫描回退
            "monster" => (n, 32usize),
            // 小地图: 库内第 n 帧
            "minimap" => (0u16, n as usize),
            // NPC: 站立首帧
            "npc" => (n, 0usize),
            _ => return None,
        };
        with_preview_lib(&kind, lib_n, |lib| {
            // 首选帧取不到像样的 (市售包大量 1×1 占位) 就向后扫第一个实帧;
            // items/minimap 帧号即语义, 不做扫描回退
            let scan_ok = matches!(kind.as_str(), "weapon" | "armour" | "monster" | "npc");
            let start = fq.base as usize;
            let img = lib
                .image(start + frame)
                .ok()
                .flatten()
                .filter(|f| !scan_ok || (f.width >= 12 && f.height >= 12))
                .or_else(|| {
                    scan_ok
                        .then(|| {
                            (start..(start + 900).min(lib.len())).find_map(|i| {
                                lib.image(i)
                                    .ok()
                                    .flatten()
                                    .filter(|f| f.width >= 12 && f.height >= 12)
                            })
                        })
                        .flatten()
                })?;
            let buf =
                image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.rgba.clone())?;
            let mut out = std::io::Cursor::new(Vec::new());
            buf.write_to(&mut out, image::ImageFormat::Png).ok()?;
            Some(out.into_inner())
        })
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=3600")
        .body(axum::body::Body::from(png))
        .unwrap())
}

/// 怪物库内实体候选段: 首实帧 + 每个"长空洞"(≥2 个方向块) 之后的块起始。
/// 一库多怪的素材靠它做二级外观选择
async fn api_mon_bases(
    State(st): State<AppState>,
    headers: HeaderMap,
    AxPath(n): AxPath<u16>,
) -> Result<Json<Vec<u32>>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let out = tokio::task::spawn_blocking(move || {
        with_preview_lib("monster", n, |lib| {
            let real = |i: usize| lib.dims(i).is_some_and(|(w, h)| w >= 8 && h >= 8);
            let cap = lib.len().min(6000);
            let base0 = (0..cap).find(|&i| real(i))?;
            // 跨度: 首块连续实帧后的下一个实帧间隔
            let run = (1..64).find(|&d| !real(base0 + d)).unwrap_or(64);
            let stride = (run..64)
                .find(|&d| real(base0 + d))
                .unwrap_or(10)
                .clamp(run, 32);
            let mut out = vec![base0 as u32];
            let mut i = base0;
            let mut gap = 0usize;
            while i < cap {
                if real(i) {
                    if gap >= stride {
                        out.push(i as u32);
                    }
                    gap = 0;
                } else {
                    gap += 1;
                }
                i += 1;
            }
            out.truncate(64);
            Some(out)
        })
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(out))
}

/// 任意 packs 库的段候选 (帧段空洞切分) — 技能特效选段等通用
/// 返回 [起始帧, 段内实帧估数] 列表
async fn api_packs_bases(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ViewerQuery>,
) -> Result<Json<Vec<(u32, u32)>>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let out = tokio::task::spawn_blocking(move || {
        let lib = viewer_lib(&q.file)?;
        let real = |i: usize| lib.dims(i).is_some_and(|(w, h)| w >= 8 && h >= 8);
        let cap = lib.len().min(6000);
        let base0 = (0..cap).find(|&i| real(i))?;
        let mut segs: Vec<(u32, u32)> = Vec::new();
        let (mut start, mut count, mut gap) = (base0, 0u32, 0usize);
        let mut i = base0;
        while i < cap {
            if real(i) {
                if gap >= 8 {
                    segs.push((start as u32, count));
                    start = i;
                    count = 0;
                }
                gap = 0;
                count += 1;
            } else {
                gap += 1;
            }
            i += 1;
        }
        if count > 0 {
            segs.push((start as u32, count));
        }
        segs.truncate(64);
        Some(segs)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(out))
}

// ── 资源查看器: 浏览 packs/ 下任意 .mfl 的帧 ──

/// 查看器库缓存 (最多同时持有几个 — 地图大库上百 MB, 不能无限攒)
static VIEW_LIBS: std::sync::Mutex<
    Option<std::collections::HashMap<String, std::sync::Arc<mir_formats::mfl::AnyLib>>>,
> = std::sync::Mutex::new(None);

/// 相对路径白名单校验: 只允许 packs 根下的 .mfl, 杜绝任意文件读取
fn viewer_path(rel: &str) -> Option<std::path::PathBuf> {
    if rel.contains("..") || rel.starts_with('/') || rel.contains('\\') || !rel.ends_with(".mfl") {
        return None;
    }
    Some(packs_root().join(rel))
}

fn viewer_lib(rel: &str) -> Option<std::sync::Arc<mir_formats::mfl::AnyLib>> {
    let mut guard = VIEW_LIBS.lock().ok()?;
    let cache = guard.get_or_insert_with(Default::default);
    if let Some(l) = cache.get(rel) {
        return Some(l.clone());
    }
    let lib = mir_formats::mfl::AnyLib::open(&viewer_path(rel)?).ok()?;
    if cache.len() >= 4 {
        cache.clear(); // 简单上限: 查看器串行使用, 清空即可
    }
    let arc = std::sync::Arc::new(lib);
    cache.insert(rel.to_string(), arc.clone());
    Some(arc)
}

#[derive(Serialize)]
struct PackEntry {
    path: String,
    size: u64,
}

/// packs/ 下全部 .mfl 清单 (相对路径 + 字节数)
async fn api_packs_list(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<PackEntry>>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<PackEntry>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("mfl") {
                if let (Ok(rel), Ok(meta)) = (p.strip_prefix(base), e.metadata()) {
                    out.push(PackEntry {
                        path: rel.to_string_lossy().replace('\\', "/"),
                        size: meta.len(),
                    });
                }
            }
        }
    }
    let root = packs_root();
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Json(out))
}

#[derive(Deserialize)]
struct ViewerQuery {
    file: String,
    #[serde(default)]
    idx: usize,
}

#[derive(Serialize)]
struct PackInfo {
    frames: usize,
    real: usize,
}

/// 单库信息: 帧位数与实帧数
async fn api_packs_info(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ViewerQuery>,
) -> Result<Json<PackInfo>, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let info = tokio::task::spawn_blocking(move || {
        let lib = viewer_lib(&q.file)?;
        let mut real = 0;
        for i in 0..lib.len() {
            if lib.image(i).ok().flatten().is_some() {
                real += 1;
            }
        }
        Some(PackInfo {
            frames: lib.len(),
            real,
        })
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(info))
}

/// 单帧 PNG (空帧/越界 404); 响应头带尺寸与锚点供前端展示
async fn api_packs_frame(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ViewerQuery>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let out = tokio::task::spawn_blocking(move || {
        let lib = viewer_lib(&q.file)?;
        let img = lib.image(q.idx).ok().flatten()?;
        let buf =
            image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.rgba.clone())?;
        let mut png = std::io::Cursor::new(Vec::new());
        buf.write_to(&mut png, image::ImageFormat::Png).ok()?;
        Some((
            png.into_inner(),
            img.width,
            img.height,
            img.offset_x,
            img.offset_y,
        ))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    let (png, w, h, ox, oy) = out;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=3600")
        .header("x-frame-meta", format!("{w}x{h} 锚({ox},{oy})"))
        .body(axum::body::Body::from(png))
        .unwrap())
}

#[derive(Deserialize)]
struct StripQuery {
    file: String,
    #[serde(default)]
    base: usize,
    #[serde(default)]
    frames: usize,
}

/// 成品拼条缓存 (段级; 特效库解帧+缩放不便宜, 弹层反复开)
type StripCache = std::collections::HashMap<(String, usize, usize), std::sync::Arc<Vec<u8>>>;
static STRIP_CACHE: std::sync::Mutex<Option<StripCache>> = std::sync::Mutex::new(None);

/// 特效段动画拼条: 自 base 起收集至多 frames 个实帧, 按各帧锚点对齐到
/// 公共包围盒后缩放进 96px 格子, 横拼一条 PNG。前端用 CSS steps() 循环
/// 播放, 一段一个请求就能看完整动画。
async fn api_packs_strip(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<StripQuery>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    const CELL: u32 = 96;
    let n = q.frames.clamp(1, 30);
    let key = (q.file.clone(), q.base, n);
    if let Ok(mut g) = STRIP_CACHE.lock() {
        if let Some(hit) = g.get_or_insert_with(Default::default).get(&key) {
            return Ok(strip_response(hit.as_ref().clone()));
        }
    }
    let png = tokio::task::spawn_blocking(move || {
        let lib = viewer_lib(&q.file)?;
        // 收集实帧 (容忍段内空洞, 扫描窗口有界)
        let mut imgs = Vec::new();
        let end = (q.base + n * 4 + 32).min(lib.len());
        for i in q.base..end {
            if imgs.len() >= n {
                break;
            }
            if let Ok(Some(img)) = lib.image(i) {
                if img.width >= 2 && img.height >= 2 {
                    imgs.push(img);
                }
            }
        }
        if imgs.is_empty() {
            return None;
        }
        // 锚点公共包围盒: 每帧真实相对位置对齐, 动画不抖
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for im in &imgs {
            x0 = x0.min(im.offset_x as i32);
            y0 = y0.min(im.offset_y as i32);
            x1 = x1.max(im.offset_x as i32 + im.width as i32);
            y1 = y1.max(im.offset_y as i32 + im.height as i32);
        }
        let (bw, bh) = ((x1 - x0) as f32, (y1 - y0) as f32);
        let scale = (CELL as f32 / bw).min(CELL as f32 / bh).min(1.0);
        let (pad_x, pad_y) = (
            (CELL as f32 - bw * scale) / 2.0,
            (CELL as f32 - bh * scale) / 2.0,
        );
        let mut canvas = image::RgbaImage::new(n as u32 * CELL, CELL);
        for (i, im) in imgs.iter().enumerate() {
            let cell_x = i as u32 * CELL;
            let dw = (im.width as f32 * scale).ceil() as u32;
            let dh = (im.height as f32 * scale).ceil() as u32;
            let ox = pad_x + (im.offset_x as i32 - x0) as f32 * scale;
            let oy = pad_y + (im.offset_y as i32 - y0) as f32 * scale;
            for dy in 0..dh {
                let sy = (dy as f32 / scale) as usize;
                let ty = oy as u32 + dy;
                if sy >= im.height as usize || ty >= CELL {
                    continue;
                }
                for dx in 0..dw {
                    let sx = (dx as f32 / scale) as usize;
                    let tx = cell_x + ox as u32 + dx;
                    if sx >= im.width as usize || tx >= (i as u32 + 1) * CELL {
                        continue;
                    }
                    let si = (sy * im.width as usize + sx) * 4;
                    let px = &im.rgba[si..si + 4];
                    if px[3] > 0 {
                        canvas.put_pixel(tx, ty, image::Rgba([px[0], px[1], px[2], px[3]]));
                    }
                }
            }
        }
        let mut png = std::io::Cursor::new(Vec::new());
        canvas.write_to(&mut png, image::ImageFormat::Png).ok()?;
        Some(png.into_inner())
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    let arc = std::sync::Arc::new(png.clone());
    if let Ok(mut g) = STRIP_CACHE.lock() {
        let c = g.get_or_insert_with(Default::default);
        if c.len() > 128 {
            c.clear();
        }
        c.insert(key, arc);
    }
    Ok(strip_response(png))
}

fn strip_response(png: Vec<u8>) -> axum::response::Response {
    axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=3600")
        .body(axum::body::Body::from(png))
        .unwrap()
}

/// 地图示意缩略图: 由 .map 阻挡位生成 (可走浅色/阻挡深色),
/// 叠加出生点(金)/传送门(青)/刷新点(红) 标记 —— 用于配置时定位坐标
async fn api_map_thumb(
    State(st): State<AppState>,
    headers: HeaderMap,
    AxPath(map): AxPath<String>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (tx, rx) = oneshot::channel();
    st.tx
        .send(AdminCmd::MapThumbInfo { map, done: tx })
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let info = rx
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let out = tokio::task::spawn_blocking(move || {
        let mapdata = mir_formats::map::parse(&std::fs::read(&info.path).ok()?).ok()?;
        let (mw, mh) = (mapdata.width, mapdata.height);
        // 缩放到长边 ≤ 512
        let step = ((mw.max(mh) as f32 / 512.0).ceil() as u32).max(1);
        let (tw, th) = (mw / step, mh / step);
        let mut img = image::RgbaImage::new(tw.max(1), th.max(1));
        for ty in 0..th {
            for tx2 in 0..tw {
                // 取样本格: 该缩略像素覆盖区域内是否多数可走
                let (mut walk, mut total) = (0u32, 0u32);
                for dy in 0..step {
                    for dx in 0..step {
                        if let Some(c) = mapdata.cell(tx2 * step + dx, ty * step + dy) {
                            total += 1;
                            if !c.blocked {
                                walk += 1;
                            }
                        }
                    }
                }
                let v = (walk * 255).checked_div(total).unwrap_or(0);
                let g = 40 + (v * 150 / 255) as u8;
                img.put_pixel(tx2, ty, image::Rgba([g, g, (g as u16 + 14) as u8, 255]));
            }
        }
        let mut mark = |x: f64, y: f64, c: [u8; 4], r: i32| {
            let (px, py) = ((x as u32 / step) as i32, (y as u32 / step) as i32);
            for dy in -r..=r {
                for dx in -r..=r {
                    let (mx, my) = (px + dx, py + dy);
                    if mx >= 0 && my >= 0 && (mx as u32) < tw && (my as u32) < th {
                        img.put_pixel(mx as u32, my as u32, image::Rgba(c));
                    }
                }
            }
        };
        for (x, y) in &info.spawns {
            mark(*x, *y, [224, 64, 64, 255], 1);
        }
        for (x, y) in &info.portals {
            mark(*x, *y, [90, 220, 220, 255], 2);
        }
        mark(info.spawn.0, info.spawn.1, [255, 216, 118, 255], 3);
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).ok()?;
        Some((buf.into_inner(), mw, mh))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (png, mw, mh) = out.ok_or(StatusCode::NOT_FOUND)?;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        // 前端按图像宽高与格数的比例换算点击坐标
        .header("x-map-width", mw.to_string())
        .header("x-map-height", mh.to_string())
        .body(axum::body::Body::from(png))
        .unwrap())
}

/// 地图原图瓦片: 每块 16x16 格 (768x512px)
async fn api_map_tile(
    State(st): State<AppState>,
    headers: HeaderMap,
    AxPath((map, tx, ty)): AxPath<(String, u32, u32)>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (done, rx) = oneshot::channel();
    let map_name = map.clone();
    st.tx
        .send(AdminCmd::MapThumbInfo { map, done })
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let info = rx
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let png = tokio::task::spawn_blocking(move || {
        let map = mir_formats::map::parse(&std::fs::read(&info.path).ok()?).ok()?;
        Some(crate::map_render::tile_cached(&map_name, &map, tx, ty))
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=86400")
        .body(axum::body::Body::from(png.to_vec()))
        .unwrap())
}

/// 小地图选择网格: mmap.Lib 帧缩放到单元格 (5 列)
async fn api_minimap_grid(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<IconsQuery>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let count = q.count.clamp(1, 60);
    let png = tokio::task::spawn_blocking(move || {
        let (cols, cw, ch) = (5u32, 128u32, 88u32);
        let rows = (count as u32).div_ceil(cols).max(1);
        let mut canvas = image::RgbaImage::new(cols * cw, rows * ch);
        for (x, y, p) in canvas.enumerate_pixels_mut() {
            let dark = ((x / 8) + (y / 8)) % 2 == 0;
            *p = image::Rgba(if dark {
                [26, 28, 40, 255]
            } else {
                [34, 36, 50, 255]
            });
        }
        with_preview_lib("minimap", 0, |lib| {
            for i in 0..count {
                let Ok(Some(src)) = lib.image(q.start + i) else {
                    continue;
                };
                let (sw, sh) = (src.width as u32, src.height as u32);
                if sw == 0 || sh == 0 {
                    continue;
                }
                let (ox, oy) = ((i as u32 % cols) * cw, (i as u32 / cols) * ch);
                // 等比缩放进单元格 (留 4px 边)
                let sc = ((cw - 6) as f32 / sw as f32).min((ch - 6) as f32 / sh as f32);
                let (dw, dh) = (
                    ((sw as f32 * sc) as u32).max(1),
                    ((sh as f32 * sc) as u32).max(1),
                );
                for dy in 0..dh {
                    for dx in 0..dw {
                        let (sx2, sy2) = ((dx as f32 / sc) as u32, (dy as f32 / sc) as u32);
                        if sx2 >= sw || sy2 >= sh {
                            continue;
                        }
                        let si = ((sy2 * sw + sx2) * 4) as usize;
                        let px = &src.rgba[si..si + 4];
                        if px[3] > 0 {
                            canvas.put_pixel(
                                ox + 3 + dx,
                                oy + 3 + dy,
                                image::Rgba([px[0], px[1], px[2], 255]),
                            );
                        }
                    }
                }
            }
            Some(())
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        let _ = canvas.write_to(&mut buf, image::ImageFormat::Png);
        buf.into_inner()
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(axum::response::Response::builder()
        .header("content-type", "image/png")
        .header("cache-control", "max-age=3600")
        .body(axum::body::Body::from(png))
        .unwrap())
}

/// 审计日志: 管理台写操作追加一行 JSON 到 admin-audit.log
pub fn audit(action: &str, detail: &str) {
    let line = serde_json::json!({
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "action": action,
        "detail": detail,
    });
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("admin-audit.log")
    {
        let _ = writeln!(f, "{line}");
    }
    tracing::info!("管理操作: {action} {detail}");
}

async fn index() -> Html<&'static str> {
    Html(include_str!("admin.html"))
}

/// 启动管理台 HTTP 服务; 返回命令接收端 (游戏循环消费)
pub fn spawn(db: crate::db::Db) -> Option<mpsc::UnboundedReceiver<AdminCmd>> {
    let addr = std::env::var("MIRFORGE_ADMIN").unwrap_or_else(|_| "127.0.0.1:4001".into());
    if addr == "off" {
        return None;
    }
    let token = std::env::var("MIRFORGE_ADMIN_TOKEN").ok();
    if token.is_none() && !addr.starts_with("127.") && !addr.starts_with("localhost") {
        tracing::error!("管理台绑定非本机地址必须设置 MIRFORGE_ADMIN_TOKEN, 已禁用");
        return None;
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let state = AppState { tx, token, db };
    let app = Router::new()
        .route("/", get(index))
        .route("/api/status", get(api_status))
        .route("/api/broadcast", post(api_broadcast))
        .route("/api/kick", post(api_kick))
        .route("/api/save", post(api_save))
        .route("/api/config", get(api_config_get).put(api_config_put))
        .route(
            "/api/zones",
            get(api_zones_get)
                .put(api_zones_put)
                .post(api_zones_add)
                .delete(api_zones_delete),
        )
        .route("/api/icons", get(api_icons_grid))
        .route("/api/npcs/grid", get(api_npc_grid))
        .route("/api/spritegrid/:kind", get(api_sprite_grid))
        .route("/api/frame/:kind/:n", get(api_frame_png))
        .route("/api/minimaps", get(api_minimap_grid))
        .route("/api/monster_bases/:n", get(api_mon_bases))
        .route("/api/packs/bases", get(api_packs_bases))
        .route("/api/packs/strip", get(api_packs_strip))
        .route("/api/packs/list", get(api_packs_list))
        .route("/api/packs/info", get(api_packs_info))
        .route("/api/packs/frame", get(api_packs_frame))
        .route("/api/mapthumb/:map", get(api_map_thumb))
        .route("/api/maptile/:map/:tx/:ty", get(api_map_tile))
        .with_state(state);
    tokio::spawn(async move {
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => {
                tracing::info!("管理台: http://{addr}");
                let _ = axum::serve(l, app).await;
            }
            Err(e) => tracing::error!("管理台监听失败 {addr}: {e}"),
        }
    });
    Some(rx)
}
