//! hub 客户端 (区服侧): 配置快照拉取 + 变更订阅 + 心跳。
//!
//! 设 `MIRFORGE_HUB` 时启用 — 配置权威在 hub, 本模块负责:
//! - 启动: 拉全量快照 (`/internal/config/snapshot`), 成功后写本地缓存
//!   文件; hub 不可达时用缓存兜底 (无缓存则拒绝启动, 避免用错误的
//!   内容开服)。
//! - 运行中: 常驻 WS 订阅 `/internal/config/ws`, hub 每次保存推新 rev,
//!   领先于本地即重拉快照, 经 `AdminCmd::ApplySnapshot` 交给游戏循环
//!   热替换; 断线 5→60 秒退避重连。
//! - 心跳: 每 30 秒 POST `/internal/heartbeat` 上报已生效 rev 与在线数,
//!   供 hub 总览页判断各区服配置是否一致。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};

use futures_util::{SinkExt, StreamExt};
use gamedata::defs::Snapshot;
use tracing::{info, warn};

/// 游戏循环应用完成的快照 rev (心跳上报用)
pub static APPLIED_REV: AtomicI64 = AtomicI64::new(0);

#[derive(Clone)]
pub struct HubCfg {
    /// hub http 地址, 如 http://127.0.0.1:4001
    pub url: String,
    pub token: String,
    pub server_id: String,
}

pub fn from_env() -> Option<HubCfg> {
    let url = std::env::var("MIRFORGE_HUB").ok()?;
    if url.is_empty() || url == "off" {
        return None;
    }
    Some(HubCfg {
        url: url.trim_end_matches('/').to_string(),
        token: std::env::var("MIRFORGE_HUB_TOKEN").unwrap_or_default(),
        server_id: std::env::var("MIRFORGE_SERVER_ID").unwrap_or_else(|_| "dev".into()),
    })
}

/// 快照缓存文件: 跟着玩家库走 (hub 宕机时区服凭它照常启动)
pub fn cache_path(db_path: &str) -> PathBuf {
    PathBuf::from(format!("{db_path}.snapshot.json"))
}

fn http_get_snapshot(cfg: &HubCfg) -> Result<Snapshot, String> {
    let resp = ureq::get(&format!("{}/internal/config/snapshot", cfg.url))
        .set("x-hub-token", &cfg.token)
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .map_err(|e| format!("hub 快照请求失败: {e}"))?;
    resp.into_json::<Snapshot>()
        .map_err(|e| format!("hub 快照解析失败: {e}"))
}

pub async fn fetch_snapshot(cfg: &HubCfg) -> Result<Snapshot, String> {
    let cfg = cfg.clone();
    tokio::task::spawn_blocking(move || http_get_snapshot(&cfg))
        .await
        .map_err(|e| format!("快照任务异常: {e}"))?
}

/// 启动期快照: hub 优先, 失败读缓存
pub async fn initial(cfg: &HubCfg, cache: &Path) -> Result<Snapshot, String> {
    match fetch_snapshot(cfg).await {
        Ok(snap) => {
            write_cache(cache, &snap);
            info!("hub 配置快照 rev={} (在线拉取)", snap.rev);
            Ok(snap)
        }
        Err(e) => {
            warn!("{e}; 尝试本地缓存 {cache:?}");
            let text = std::fs::read_to_string(cache)
                .map_err(|_| format!("hub 不可达且无本地快照缓存: {e}"))?;
            let snap: Snapshot =
                serde_json::from_str(&text).map_err(|e| format!("快照缓存损坏: {e}"))?;
            info!("hub 配置快照 rev={} (本地缓存兜底)", snap.rev);
            Ok(snap)
        }
    }
}

fn write_cache(cache: &Path, snap: &Snapshot) {
    match serde_json::to_string(snap) {
        Ok(text) => {
            if let Err(e) = std::fs::write(cache, text) {
                warn!("快照缓存写入失败 {cache:?}: {e}");
            }
        }
        Err(e) => warn!("快照序列化失败: {e}"),
    }
}

fn ws_url(cfg: &HubCfg) -> String {
    let base = cfg
        .url
        .replacen("http://", "ws://", 1)
        .replacen("https://", "wss://", 1);
    format!("{base}/internal/config/ws?token={}", cfg.token)
}

/// 常驻: 变更订阅 + 心跳 (随进程存活)
pub fn spawn_runtime(
    cfg: HubCfg,
    cache: PathBuf,
    admin_tx: tokio::sync::mpsc::UnboundedSender<crate::admin::AdminCmd>,
) {
    // 心跳
    {
        let cfg = cfg.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tick.tick().await;
                let cfg2 = cfg.clone();
                let rev = APPLIED_REV.load(Ordering::Relaxed);
                let online = crate::game::online_count();
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = ureq::post(&format!("{}/internal/heartbeat", cfg2.url))
                        .set("x-hub-token", &cfg2.token)
                        .timeout(std::time::Duration::from_secs(5))
                        .send_json(serde_json::json!({
                            "server": cfg2.server_id,
                            "rev": rev,
                            "online": online,
                        }));
                })
                .await;
            }
        });
    }
    // 变更订阅
    tokio::spawn(async move {
        let mut backoff = 5u64;
        loop {
            match tokio_tungstenite::connect_async(ws_url(&cfg)).await {
                Ok((mut sock, _)) => {
                    info!("hub 配置订阅已连接");
                    backoff = 5;
                    while let Some(msg) = sock.next().await {
                        let text = match msg {
                            Ok(tokio_tungstenite::tungstenite::Message::Text(t)) => t,
                            Ok(tokio_tungstenite::tungstenite::Message::Ping(p)) => {
                                let _ = sock
                                    .send(tokio_tungstenite::tungstenite::Message::Pong(p))
                                    .await;
                                continue;
                            }
                            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) | Err(_) => break,
                            Ok(_) => continue,
                        };
                        let rev = serde_json::from_str::<serde_json::Value>(&text)
                            .ok()
                            .and_then(|v| v.get("rev").and_then(|r| r.as_i64()))
                            .unwrap_or(0);
                        if rev <= APPLIED_REV.load(Ordering::Relaxed) {
                            continue;
                        }
                        match fetch_snapshot(&cfg).await {
                            Ok(snap) => {
                                write_cache(&cache, &snap);
                                info!("hub 配置更新 rev={}, 热应用中", snap.rev);
                                let _ = admin_tx.send(crate::admin::AdminCmd::ApplySnapshot {
                                    snap: Box::new(snap),
                                });
                            }
                            Err(e) => warn!("拉取新快照失败: {e}"),
                        }
                    }
                    warn!("hub 配置订阅断开, {backoff} 秒后重连");
                }
                Err(e) => {
                    warn!("hub 配置订阅连接失败: {e}, {backoff} 秒后重连");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(60);
        }
    });
}
