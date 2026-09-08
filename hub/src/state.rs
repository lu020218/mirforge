//! hub 共享状态与鉴权。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use axum::http::HeaderMap;
use sqlx::SqlitePool;

/// 区服心跳记录 (内存态; hub 重启即清, 由下一轮心跳恢复)
#[derive(Clone)]
pub struct Heartbeat {
    pub rev: i64,
    pub online: u32,
    pub at: Instant,
}

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    /// 运营鉴权 (x-admin-token); None = 仅本机免鉴权
    pub token: Option<String>,
    /// 区服内部通道密钥 (x-hub-token / ws ?token=)
    pub hub_token: Option<String>,
    /// 公告变更信号 (登录器 WS 订阅)
    pub news_tx: tokio::sync::broadcast::Sender<()>,
    /// 配置 rev 变更信号 (区服 WS 订阅)
    pub cfg_tx: tokio::sync::broadcast::Sender<i64>,
    /// 区服心跳表: server_id → 最近一跳
    pub beats: std::sync::Arc<Mutex<HashMap<String, Heartbeat>>>,
}

pub fn authed(state: &AppState, headers: &HeaderMap) -> bool {
    match &state.token {
        None => true,
        Some(t) => headers
            .get("x-admin-token")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == t),
    }
}

/// 区服内部端点鉴权: 未配置密钥时仅信任本机部署 (与管理台同语义)
pub fn hub_authed(state: &AppState, headers: &HeaderMap) -> bool {
    match &state.hub_token {
        None => true,
        Some(t) => headers
            .get("x-hub-token")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v == t),
    }
}
