//! Web 管理控制台 (axum)。
//!
//! 默认监听 `127.0.0.1:4001` (仅本机); `MIRFORGE_ADMIN` 覆盖地址,
//! 设为 `off` 关闭。绑定非回环地址时必须设置 `MIRFORGE_ADMIN_TOKEN`,
//! 请求经 `x-admin-token` 头校验。
//!
//! 管理命令经 mpsc 注入游戏主循环 (与网关事件同一 select), 状态查询
//! 走 oneshot 请求-响应 —— 无锁, 不与 20Hz tick 抢状态。

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

/// 管理命令 (游戏循环内处理)
pub enum AdminCmd {
    Status(oneshot::Sender<StatusSnapshot>),
    Broadcast(String),
    Kick {
        name: String,
        done: oneshot::Sender<bool>,
    },
    SaveAll(oneshot::Sender<()>),
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

async fn index() -> Html<&'static str> {
    Html(include_str!("admin.html"))
}

/// 启动管理台 HTTP 服务; 返回命令接收端 (游戏循环消费)
pub fn spawn() -> Option<mpsc::UnboundedReceiver<AdminCmd>> {
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
    let state = AppState { tx, token };
    let app = Router::new()
        .route("/", get(index))
        .route("/api/status", get(api_status))
        .route("/api/broadcast", post(api_broadcast))
        .route("/api/kick", post(api_kick))
        .route("/api/save", post(api_save))
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
