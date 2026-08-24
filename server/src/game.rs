//! 游戏循环：动作消费 + 20Hz 区域广播 + 会话/断线恢复。
//! M2 范围：登录/建角/进图/移动（sim 校验）/心跳/Resume；玩法系统随 M3 迁入。

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use protocol::{ClientMessage, EntityUpdate, Position, ServerMessage, PROTOCOL_VERSION};
use sim::{WalkGrid, BODY_RADIUS};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::db::{CharacterRow, Db};
use crate::gateway::{broadcast_to, send_to, ConnEvent, Sessions, RECONNECT_WINDOW};

/// 与客户端一致的移动上限（跑步 格/秒）；校验余量 20%
const MAX_SPEED: f64 = 3.4 * 1.2;
/// 单包步长最长按此时间窗折算（防补发大步瞬移）
const MAX_STEP_WINDOW: Duration = Duration::from_millis(500);
/// 广播节拍
const TICK: Duration = Duration::from_millis(50);
/// 位置定期存档间隔
const SAVE_EVERY: Duration = Duration::from_secs(5);

/// 传送门（边车配置）：踏入 x,y 半径 [`PORTAL_RADIUS`] 内 → 传到 to_zone
/// （to_x/to_y 省略时落在目标区域出生点）
pub struct Portal {
    pub x: f64,
    pub y: f64,
    pub to_zone: String,
    pub to_x: Option<f64>,
    pub to_y: Option<f64>,
}

const PORTAL_RADIUS: f64 = 1.0;

pub struct Zone {
    pub id: String,
    pub name: String,
    pub walk: WalkGrid,
    pub spawn: (f64, f64),
    pub portals: Vec<Portal>,
}

/// 从 (x,y) 就近找可站立点（螺旋外扩, 最远 60 格）
pub fn nearest_walkable(walk: &WalkGrid, x: f64, y: f64) -> (f64, f64) {
    if walk.is_walkable_circle(x, y, BODY_RADIUS) {
        return (x, y);
    }
    let (cx, cy) = (x.floor() as i64, y.floor() as i64);
    for r in 1..=60i64 {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs() != r && dy.abs() != r {
                    continue; // 只扫当前圈
                }
                let (tx, ty) = ((cx + dx) as f64 + 0.5, (cy + dy) as f64 + 0.5);
                if walk.is_walkable_circle(tx, ty, BODY_RADIUS) {
                    return (tx, ty);
                }
            }
        }
    }
    (x, y)
}

/// 连接级状态（角色选定前）
#[derive(Default)]
struct ConnState {
    hello_ok: bool,
    account_id: Option<String>,
}

/// 在场角色状态（断线后保留至重连窗口结束）
struct PlayerState {
    conn_id: String,
    account_id: String,
    character: CharacterRow,
    zone: String,
    x: f64,
    y: f64,
    moving: bool,
    running: bool,
    last_move: Instant,
    connected: bool,
    disconnected_at: Option<Instant>,
}

pub struct Game {
    zones: HashMap<String, Zone>,
    default_zone: String,
    db: Db,
    sessions: Sessions,
    conns: HashMap<String, ConnState>,
    /// character_id → 状态
    players: HashMap<String, PlayerState>,
    /// resume token → account_id
    tokens: HashMap<String, String>,
    last_save: Instant,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Game {
    pub fn new(
        zones: HashMap<String, Zone>,
        default_zone: String,
        db: Db,
        sessions: Sessions,
    ) -> Self {
        Game {
            zones,
            default_zone,
            db,
            sessions,
            conns: HashMap::new(),
            players: HashMap::new(),
            tokens: HashMap::new(),
            last_save: Instant::now(),
        }
    }

    pub async fn run(mut self, mut events: mpsc::UnboundedReceiver<ConnEvent>) {
        let mut tick = tokio::time::interval(TICK);
        loop {
            tokio::select! {
                ev = events.recv() => {
                    match ev {
                        Some(ConnEvent::Action(a)) => self.handle(a.conn_id, a.message).await,
                        Some(ConnEvent::Disconnected { conn_id }) => self.on_disconnect(&conn_id).await,
                        None => break,
                    }
                }
                _ = tick.tick() => self.tick().await,
            }
        }
    }

    async fn handle(&mut self, conn_id: String, msg: ClientMessage) {
        // Hello 协商必须先行
        let hello_ok = self.conns.get(&conn_id).is_some_and(|c| c.hello_ok);
        if let ClientMessage::Hello { version } = msg {
            if version == PROTOCOL_VERSION {
                self.conns.entry(conn_id.clone()).or_default().hello_ok = true;
                send_to(
                    &self.sessions,
                    &conn_id,
                    ServerMessage::HelloAck {
                        version: PROTOCOL_VERSION,
                    },
                )
                .await;
            } else {
                send_to(
                    &self.sessions,
                    &conn_id,
                    ServerMessage::Error {
                        message: format!(
                            "协议版本不符: 服务器 {PROTOCOL_VERSION}, 客户端 {version}"
                        ),
                    },
                )
                .await;
            }
            return;
        }
        if !hello_ok {
            send_to(
                &self.sessions,
                &conn_id,
                ServerMessage::Error {
                    message: "请先发送 hello 协商协议版本".into(),
                },
            )
            .await;
            return;
        }

        match msg {
            ClientMessage::Heartbeat => {
                send_to(&self.sessions, &conn_id, ServerMessage::HeartbeatAck).await;
            }
            ClientMessage::Register { username, password } => {
                let reply = match self.db.register(&username, &password).await {
                    Ok(Some(account_id)) => {
                        self.conns.entry(conn_id.clone()).or_default().account_id =
                            Some(account_id.clone());
                        self.issue_token(&conn_id, &account_id).await;
                        ServerMessage::LoginResult {
                            success: true,
                            account_id: Some(account_id),
                            message: "注册成功".into(),
                        }
                    }
                    Ok(None) => ServerMessage::LoginResult {
                        success: false,
                        account_id: None,
                        message: "用户名已存在".into(),
                    },
                    Err(e) => {
                        warn!("注册失败: {e}");
                        ServerMessage::LoginResult {
                            success: false,
                            account_id: None,
                            message: "服务器内部错误".into(),
                        }
                    }
                };
                send_to(&self.sessions, &conn_id, reply).await;
            }
            ClientMessage::Login { username, password } => {
                match self.db.login(&username, &password).await {
                    Ok(Some(account_id)) => {
                        self.conns.entry(conn_id.clone()).or_default().account_id =
                            Some(account_id.clone());
                        self.issue_token(&conn_id, &account_id).await;
                        send_to(
                            &self.sessions,
                            &conn_id,
                            ServerMessage::LoginResult {
                                success: true,
                                account_id: Some(account_id.clone()),
                                message: "登录成功".into(),
                            },
                        )
                        .await;
                        self.send_character_list(&conn_id, &account_id).await;
                    }
                    Ok(None) => {
                        send_to(
                            &self.sessions,
                            &conn_id,
                            ServerMessage::LoginResult {
                                success: false,
                                account_id: None,
                                message: "用户名或密码错误".into(),
                            },
                        )
                        .await;
                    }
                    Err(e) => warn!("登录失败: {e}"),
                }
            }
            ClientMessage::CreateCharacter {
                name,
                class,
                gender,
            } => {
                let Some(account_id) = self.account_of(&conn_id) else {
                    return;
                };
                let spawn = self
                    .zones
                    .get(&self.default_zone)
                    .map(|z| z.spawn)
                    .unwrap_or((330.5, 150.5));
                match self
                    .db
                    .create_character(
                        &account_id,
                        &name,
                        class,
                        &gender,
                        &self.default_zone,
                        spawn,
                    )
                    .await
                {
                    Ok(Some(character_id)) => {
                        send_to(
                            &self.sessions,
                            &conn_id,
                            ServerMessage::CharacterCreated { character_id, name },
                        )
                        .await;
                        self.send_character_list(&conn_id, &account_id).await;
                    }
                    Ok(None) => {
                        send_to(
                            &self.sessions,
                            &conn_id,
                            ServerMessage::Error {
                                message: "角色名已存在".into(),
                            },
                        )
                        .await;
                    }
                    Err(e) => warn!("建角失败: {e}"),
                }
            }
            ClientMessage::SelectCharacter { character_id } => {
                let Some(account_id) = self.account_of(&conn_id) else {
                    return;
                };
                match self.db.character(&account_id, &character_id).await {
                    Ok(Some(c)) => self.enter_game(&conn_id, &account_id, c).await,
                    Ok(None) => {
                        send_to(
                            &self.sessions,
                            &conn_id,
                            ServerMessage::Error {
                                message: "角色不存在".into(),
                            },
                        )
                        .await;
                    }
                    Err(e) => warn!("选角失败: {e}"),
                }
            }
            ClientMessage::Resume {
                token,
                character_id,
            } => {
                let Some(account_id) = self.tokens.get(&token).cloned() else {
                    send_to(
                        &self.sessions,
                        &conn_id,
                        ServerMessage::ResumeFailed {
                            message: "令牌无效或已过期".into(),
                        },
                    )
                    .await;
                    return;
                };
                self.conns.entry(conn_id.clone()).or_default().account_id =
                    Some(account_id.clone());
                // 活状态还在 → 原位恢复（位置/朝向全保留）
                if let Some(p) = self.players.get_mut(&character_id) {
                    if p.account_id == account_id {
                        p.conn_id = conn_id.clone();
                        p.connected = true;
                        p.disconnected_at = None;
                        let (x, y) = (p.x, p.y);
                        info!("Resume 原位恢复: {character_id} @({x:.1},{y:.1})");
                        self.send_enter_payload(&conn_id, &character_id, x, y).await;
                        return;
                    }
                }
                // 活状态已过期 → 从库重进（回存档点）
                match self.db.character(&account_id, &character_id).await {
                    Ok(Some(c)) => self.enter_game(&conn_id, &account_id, c).await,
                    _ => {
                        send_to(
                            &self.sessions,
                            &conn_id,
                            ServerMessage::ResumeFailed {
                                message: "角色状态已失效, 请重新登录".into(),
                            },
                        )
                        .await;
                    }
                }
            }
            ClientMessage::Move { direction } => {
                if let Some((cid, character_id, x, y)) = self.handle_move(&conn_id, direction) {
                    // 触发传送门 → 通知切区
                    self.send_enter_zone_only(&cid, &character_id, x, y).await;
                }
            }
            other => {
                // M3 玩法消息占位
                warn!("暂未实现的消息: {other:?}");
            }
        }
    }

    fn account_of(&self, conn_id: &str) -> Option<String> {
        self.conns.get(conn_id).and_then(|c| c.account_id.clone())
    }

    async fn issue_token(&mut self, conn_id: &str, account_id: &str) {
        let token = uuid::Uuid::new_v4().to_string();
        self.tokens.insert(token.clone(), account_id.to_string());
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::SessionToken { token },
        )
        .await;
    }

    async fn send_character_list(&self, conn_id: &str, account_id: &str) {
        if let Ok(characters) = self.db.characters_of(account_id).await {
            send_to(
                &self.sessions,
                conn_id,
                ServerMessage::CharacterList { characters },
            )
            .await;
        }
    }

    async fn enter_game(&mut self, conn_id: &str, account_id: &str, c: CharacterRow) {
        // 存档区域已不存在时回缺省区域出生点
        let (zone, x, y) = if self.zones.contains_key(&c.zone) {
            (c.zone.clone(), c.x, c.y)
        } else {
            let z = &self.zones[&self.default_zone];
            (self.default_zone.clone(), z.spawn.0, z.spawn.1)
        };
        // 同角色旧连接被顶替
        let character_id = c.id.clone();
        self.players.insert(
            character_id.clone(),
            PlayerState {
                conn_id: conn_id.to_string(),
                account_id: account_id.to_string(),
                character: c,
                zone: zone.clone(),
                x,
                y,
                moving: false,
                running: false,
                last_move: Instant::now(),
                connected: true,
                disconnected_at: None,
            },
        );
        info!("进入游戏: {character_id} {zone} @({x:.1},{y:.1})");
        self.send_enter_payload(conn_id, &character_id, x, y).await;
    }

    async fn send_enter_payload(&self, conn_id: &str, character_id: &str, x: f64, y: f64) {
        let p = &self.players[character_id];
        let character = serde_json::json!({
            "id": p.character.id,
            "name": p.character.name,
            "class": p.character.class,
            "gender": p.character.gender,
            "level": p.character.level,
        });
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::EnterGame {
                character,
                inventory: serde_json::json!([]),
            },
        )
        .await;
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::LoginSuccess {
                player_id: character_id.to_string(),
                timestamp: now_ms(),
            },
        )
        .await;
        let (zone_id, zone_name) = self
            .zones
            .get(&p.zone)
            .map(|z| (z.id.clone(), z.name.clone()))
            .unwrap_or((p.zone.clone(), p.zone.clone()));
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::ZoneChanged {
                zone_id,
                zone_name,
                position: Position { x, y },
            },
        )
        .await;
    }

    /// 移动校验：步长按时间窗限幅 → sim 同源判定（客户端预测调同一函数）。
    /// 踏中传送门时切区并返回 (conn, character, x, y) 供调用方发 ZoneChanged。
    fn handle_move(
        &mut self,
        conn_id: &str,
        direction: Position,
    ) -> Option<(String, String, f64, f64)> {
        let (id, p) = self
            .players
            .iter_mut()
            .find(|(_, p)| p.conn_id == conn_id)?;
        let zone = self.zones.get(&p.zone)?;
        let now = Instant::now();
        let dt = now
            .duration_since(p.last_move)
            .min(MAX_STEP_WINDOW)
            .as_secs_f64();
        p.last_move = now;
        let (mut dx, mut dy) = (direction.x, direction.y);
        let len = (dx * dx + dy * dy).sqrt();
        let max = MAX_SPEED * dt;
        if len > max && len > 0.0 {
            let k = max / len;
            dx *= k;
            dy *= k;
        }
        let (nx, ny) = zone.walk.try_move(p.x, p.y, dx, dy, BODY_RADIUS);
        p.moving = (nx - p.x).abs() > 1e-9 || (ny - p.y).abs() > 1e-9;
        // 跑步阈值: 单包速度超走路上限即视为跑 (广播动画用)
        p.running = p.moving && len / dt.max(1e-6) > 2.0;
        p.x = nx;
        p.y = ny;
        // 传送门判定
        let portal = zone
            .portals
            .iter()
            .find(|pt| ((pt.x - nx).powi(2) + (pt.y - ny).powi(2)).sqrt() < PORTAL_RADIUS)?;
        let target = self.zones.get(&portal.to_zone)?;
        let (tx, ty) = nearest_walkable(
            &target.walk,
            portal.to_x.unwrap_or(target.spawn.0),
            portal.to_y.unwrap_or(target.spawn.1),
        );
        let to_zone = portal.to_zone.clone();
        info!(
            "传送: {} {} ({nx:.1},{ny:.1}) → {to_zone} ({tx:.1},{ty:.1})",
            id, p.zone
        );
        p.zone = to_zone;
        p.x = tx;
        p.y = ty;
        p.moving = false;
        Some((conn_id.to_string(), id.clone(), tx, ty))
    }

    /// 只发 ZoneChanged（切区通知）
    async fn send_enter_zone_only(&self, conn_id: &str, character_id: &str, x: f64, y: f64) {
        let Some(p) = self.players.get(character_id) else {
            return;
        };
        let (zone_id, zone_name) = self
            .zones
            .get(&p.zone)
            .map(|z| (z.id.clone(), z.name.clone()))
            .unwrap_or((p.zone.clone(), p.zone.clone()));
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::ZoneChanged {
                zone_id,
                zone_name,
                position: Position { x, y },
            },
        )
        .await;
    }

    async fn on_disconnect(&mut self, conn_id: &str) {
        self.conns.remove(conn_id);
        for p in self.players.values_mut() {
            if p.conn_id == conn_id && p.connected {
                p.connected = false;
                p.disconnected_at = Some(Instant::now());
                p.moving = false;
            }
        }
    }

    async fn tick(&mut self) {
        let now = Instant::now();
        // 停止判定: 200ms 没有移动包即站立
        for p in self.players.values_mut() {
            if p.moving && now.duration_since(p.last_move) > Duration::from_millis(200) {
                p.moving = false;
            }
        }
        // 过窗清理 + 存档
        let expired: Vec<String> = self
            .players
            .iter()
            .filter(|(_, p)| {
                p.disconnected_at
                    .is_some_and(|t| now.duration_since(t) > RECONNECT_WINDOW)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(p) = self.players.remove(&id) {
                let _ = self.db.save_position(&id, &p.zone, p.x, p.y).await;
                info!("重连窗过期, 存档并移除: {id}");
            }
        }
        if now.duration_since(self.last_save) > SAVE_EVERY {
            self.last_save = now;
            for (id, p) in &self.players {
                let _ = self.db.save_position(id, &p.zone, p.x, p.y).await;
            }
        }
        // 20Hz 广播, 按区域分组 (只看得见同区域的人)
        if self.players.is_empty() {
            return;
        }
        let ts = now_ms();
        for zone_id in self.zones.keys() {
            let entities: Vec<EntityUpdate> = self
                .players
                .iter()
                .filter(|(_, p)| &p.zone == zone_id)
                .map(|(id, p)| EntityUpdate {
                    id: id.clone(),
                    position: Some(Position { x: p.x, y: p.y }),
                    hp: None,
                    animation: Some(
                        match (p.moving, p.running) {
                            (true, true) => "run",
                            (true, false) => "walk",
                            _ => "stand",
                        }
                        .into(),
                    ),
                    removed: None,
                })
                .collect();
            if entities.is_empty() {
                continue;
            }
            let targets: Vec<String> = self
                .players
                .values()
                .filter(|p| p.connected && &p.zone == zone_id)
                .map(|p| p.conn_id.clone())
                .collect();
            broadcast_to(
                &self.sessions,
                &targets,
                ServerMessage::StateUpdate {
                    entities,
                    timestamp: ts,
                },
            )
            .await;
        }
    }
}
