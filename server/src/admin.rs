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
    ConfigReloaded,
    Broadcast(String),
    Kick {
        name: String,
        done: oneshot::Sender<bool>,
    },
    SaveAll(oneshot::Sender<()>),
    /// 区域列表 + 可接入地图
    ZonesInfo(oneshot::Sender<ZonesInfo>),
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
            k => return Err(format!("未知配置类别: {k}")),
        }
        Ok(())
    })();
    if let Err(e) = parsed {
        return Ok(fail(e));
    }
    let errors = next.validate();
    if !errors.is_empty() {
        return Ok(Json(PutConfigResp { ok: false, errors }));
    }
    // 持久化到数据库 (整表事务替换)
    let pool = st.db.pool();
    let saved = match req.kind.as_str() {
        "items" => crate::config_store::save_items(pool, &next.items).await,
        "skills" => crate::config_store::save_skills(pool, &next.skills).await,
        _ => crate::config_store::save_quests(pool, &next.quests).await,
    };
    if let Err(e) = saved {
        return Ok(fail(format!("保存失败: {e}")));
    }
    crate::game::set_data(next);
    let _ = st.tx.send(AdminCmd::ConfigReloaded);
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

static RES_ROOT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
static PREVIEW_LIBS: std::sync::Mutex<
    Option<std::collections::HashMap<String, Option<mir_formats::crystal_lib::CrystalLib>>>,
> = std::sync::Mutex::new(None);

/// 预览库白名单: 路径固定, 杜绝任意文件读取
fn preview_lib_path(kind: &str, n: u16) -> Option<std::path::PathBuf> {
    let root = RES_ROOT.get()?;
    let rel = match kind {
        "items" => "Data/Items.Lib".to_string(),
        "weapon" => format!("Data/CWeapon/{n:02}.Lib"),
        "armour" => format!("Data/CArmour/{n:02}.Lib"),
        _ => return None,
    };
    Some(root.join(rel))
}

fn with_preview_lib<R>(
    kind: &str,
    n: u16,
    f: impl FnOnce(&mir_formats::crystal_lib::CrystalLib) -> Option<R>,
) -> Option<R> {
    let key = format!("{kind}/{n}");
    let mut guard = PREVIEW_LIBS.lock().ok()?;
    let cache = guard.get_or_insert_with(Default::default);
    if !cache.contains_key(&key) {
        let lib = preview_lib_path(kind, n)
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|d| mir_formats::crystal_lib::CrystalLib::parse(d).ok());
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

/// 单帧 PNG: items 图标 / weapon-armour 站立帧 (帧 16 = 朝南)
async fn api_frame_png(
    State(st): State<AppState>,
    headers: HeaderMap,
    AxPath((kind, n)): AxPath<(String, u16)>,
) -> Result<axum::response::Response, StatusCode> {
    if !authed(&st, &headers) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let png = tokio::task::spawn_blocking(move || {
        let (lib_n, frame) = match kind.as_str() {
            "items" => (0u16, n as usize),
            // 外观预览: 该库朝南站立首帧
            "weapon" | "armour" => (n, 16usize),
            _ => return None,
        };
        with_preview_lib(&kind, lib_n, |lib| {
            let img = lib.image(frame).ok().flatten()?;
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
pub fn spawn(
    res_root: std::path::PathBuf,
    db: crate::db::Db,
) -> Option<mpsc::UnboundedReceiver<AdminCmd>> {
    let _ = RES_ROOT.set(res_root);
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
            get(api_zones_get).put(api_zones_put).post(api_zones_add),
        )
        .route("/api/icons", get(api_icons_grid))
        .route("/api/frame/:kind/:n", get(api_frame_png))
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
