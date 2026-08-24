//! WebSocket 网关：连接管理 + 会话表 + 心跳/重连窗口。
//! 架构承袭旧服务器（连接任务 ↔ mpsc ↔ 游戏循环），消息类型来自 protocol crate。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use protocol::{ClientMessage, ServerMessage};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

/// 客户端 5s 一跳；宽松超时容忍后台标签节流
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);
/// 断线后状态保留窗口（Resume 原位恢复）
pub const RECONNECT_WINDOW: Duration = Duration::from_secs(120);

/// 单连接会话
pub struct PlayerSession {
    pub sender: mpsc::UnboundedSender<ServerMessage>,
    pub last_heartbeat: Instant,
    pub connected: bool,
    pub disconnected_at: Option<Instant>,
}

/// 入站动作（游戏循环消费）
pub struct PlayerAction {
    pub conn_id: String,
    pub message: ClientMessage,
}

/// 连接生命周期事件（游戏循环据此清会话态）
pub enum ConnEvent {
    Action(PlayerAction),
    Disconnected { conn_id: String },
}

pub type Sessions = Arc<RwLock<HashMap<String, PlayerSession>>>;

pub struct Gateway {
    sessions: Sessions,
    event_tx: mpsc::UnboundedSender<ConnEvent>,
}

impl Gateway {
    pub fn new() -> (Self, mpsc::UnboundedReceiver<ConnEvent>) {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        (
            Gateway {
                sessions: Arc::new(RwLock::new(HashMap::new())),
                event_tx,
            },
            event_rx,
        )
    }

    pub fn sessions(&self) -> Sessions {
        self.sessions.clone()
    }

    pub async fn listen(self: Arc<Self>, addr: &str) -> std::io::Result<()> {
        let listener = TcpListener::bind(addr).await?;
        info!("网关监听 {addr}");
        let sessions = self.sessions.clone();
        let event_tx = self.event_tx.clone();
        tokio::spawn(heartbeat_checker(sessions, event_tx));
        loop {
            let (stream, peer) = listener.accept().await?;
            let gw = self.clone();
            tokio::spawn(async move {
                if let Err(e) = gw.handle_connection(stream).await {
                    error!("连接 {peer} 出错: {e}");
                }
            });
        }
    }

    async fn handle_connection(
        &self,
        stream: TcpStream,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let ws = tokio_tungstenite::accept_async(stream).await?;
        let (mut ws_tx, mut ws_rx) = ws.split();
        let conn_id = uuid::Uuid::new_v4().to_string();
        let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<ServerMessage>();
        self.sessions.write().await.insert(
            conn_id.clone(),
            PlayerSession {
                sender: msg_tx,
                last_heartbeat: Instant::now(),
                connected: true,
                disconnected_at: None,
            },
        );

        // 出站: ServerMessage → WS 文本帧
        let send_task = tokio::spawn(async move {
            while let Some(msg) = msg_rx.recv().await {
                let Ok(json) = protocol::encode(&msg) else {
                    continue;
                };
                if ws_tx.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
        });

        // 入站: WS 文本帧 → 游戏循环
        let cid = conn_id.clone();
        let sessions = self.sessions.clone();
        let event_tx = self.event_tx.clone();
        let recv_task = tokio::spawn(async move {
            while let Some(Ok(msg)) = ws_rx.next().await {
                match msg {
                    Message::Text(text) => match protocol::decode_client(&text) {
                        Ok(m) => {
                            if matches!(m, ClientMessage::Heartbeat) {
                                if let Some(s) = sessions.write().await.get_mut(&cid) {
                                    s.last_heartbeat = Instant::now();
                                }
                            }
                            let _ = event_tx.send(ConnEvent::Action(PlayerAction {
                                conn_id: cid.clone(),
                                message: m,
                            }));
                        }
                        Err(e) => warn!("非法消息 ({cid}): {e}"),
                    },
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        });

        tokio::select! {
            _ = send_task => {},
            _ = recv_task => {},
        }

        // 断线: 标记并保留重连窗口
        if let Some(s) = self.sessions.write().await.get_mut(&conn_id) {
            s.connected = false;
            s.disconnected_at = Some(Instant::now());
        }
        let _ = self.event_tx.send(ConnEvent::Disconnected {
            conn_id: conn_id.clone(),
        });
        info!(
            "连接 {conn_id} 断开, 状态保留 {}s",
            RECONNECT_WINDOW.as_secs()
        );
        Ok(())
    }
}

/// 会话表工具: 单发
pub async fn send_to(sessions: &Sessions, conn_id: &str, msg: ServerMessage) {
    if let Some(s) = sessions.read().await.get(conn_id) {
        if s.connected {
            let _ = s.sender.send(msg);
        }
    }
}

/// 会话表工具: 群发
pub async fn broadcast_to(sessions: &Sessions, conn_ids: &[String], msg: ServerMessage) {
    let map = sessions.read().await;
    for cid in conn_ids {
        if let Some(s) = map.get(cid) {
            if s.connected {
                let _ = s.sender.send(msg.clone());
            }
        }
    }
}

async fn heartbeat_checker(sessions: Sessions, event_tx: mpsc::UnboundedSender<ConnEvent>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        let now = Instant::now();
        let mut expired = Vec::new();
        {
            let mut map = sessions.write().await;
            for (cid, s) in map.iter_mut() {
                if s.connected {
                    if now.duration_since(s.last_heartbeat) > HEARTBEAT_TIMEOUT {
                        warn!("心跳超时: {cid}");
                        s.connected = false;
                        s.disconnected_at = Some(now);
                    }
                } else if let Some(t) = s.disconnected_at {
                    if now.duration_since(t) > RECONNECT_WINDOW {
                        expired.push(cid.clone());
                    }
                }
            }
            for cid in &expired {
                map.remove(cid);
            }
        }
        for cid in expired {
            info!("会话过期移除: {cid}");
            let _ = event_tx.send(ConnEvent::Disconnected { conn_id: cid });
        }
    }
}
