//! 游戏循环：动作消费 + 20Hz 区域广播 + 会话/断线恢复。
//! M2 范围：登录/建角/进图/移动（sim 校验）/心跳/Resume；玩法系统随 M3 迁入。

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use protocol::{ClientMessage, EntityUpdate, Position, ServerMessage, PROTOCOL_VERSION};
use sim::{dir8_from, WalkGrid, BODY_RADIUS};
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
    pub monster_spawns: Vec<MonsterSpawn>,
}

/// 怪物刷新点（边车配置）：home 附近 radius 内刷 count 只
pub struct MonsterSpawn {
    pub template: String,
    /// 客户端图库号 (Data/Monster/{image:03}.Lib)
    pub image: u16,
    pub x: f64,
    pub y: f64,
    pub count: u32,
    pub radius: f64,
    /// 被动怪不主动仇恨/攻击
    pub passive: bool,
    pub hp: i32,
    pub damage: i32,
    pub exp: u64,
}

// ── 战斗参数 (M3.2 基础值; 装备加成随 3.4 接入) ──
const PLAYER_ATTACK_RANGE: f64 = 2.5;
const PLAYER_ATTACK_CD: Duration = Duration::from_millis(600);
const MONSTER_HIT_RANGE: f64 = 2.2;
const DYING_TIME: Duration = Duration::from_millis(1300);
const RESPAWN_TIME: Duration = Duration::from_secs(30);

fn max_hp_for(level: u32) -> i32 {
    40 + level as i32 * 12
}
fn attack_for(level: u32) -> i32 {
    4 + level as i32 * 2
}
/// 升到下一级所需累计经验
fn exp_required(level: u32) -> u64 {
    level as u64 * 100
}

// ── 怪物 AI 参数 (对齐旧服务器实测值) ──
const AGGRO_RANGE: f64 = 6.0; // 仇恨半径
const LEASH_RANGE: f64 = 12.0; // 拉离脱战半径
const ATTACK_RANGE: f64 = 1.6; // 出手距离
const CHASE_SPEED: f64 = 1.8; // 追击 格/s
const WANDER_SPEED: f64 = 0.8; // 游荡 格/s
const ATTACK_ANIM: Duration = Duration::from_millis(900);
const ATTACK_COOLDOWN: Duration = Duration::from_millis(1500);

/// 在场怪物
struct Monster {
    id: String,
    zone: String,
    home: (f64, f64),
    /// 游荡活动半径 (来自刷新点)
    roam: f64,
    x: f64,
    y: f64,
    dir: u8,
    /// 当前移动目标 (None = 站立)
    target: Option<(f64, f64)>,
    chasing: bool,
    attack_until: Option<Instant>,
    /// 攻击动画结束时结算伤害的目标角色
    pending_hit: Option<String>,
    next_attack: Instant,
    next_decide: Instant,
    passive: bool,
    hp: i32,
    max_hp: i32,
    damage: i32,
    exp: u64,
    /// 死亡动画播放中 (到点转入等待重生)
    dying_until: Option<Instant>,
    /// 等待重生 (期间不广播不参与 AI)
    respawn_at: Option<Instant>,
    /// removed=true 是否已广播过
    removed_sent: bool,
}

impl Monster {
    fn alive(&self) -> bool {
        self.dying_until.is_none() && self.respawn_at.is_none()
    }
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
    hp: i32,
    max_hp: i32,
    level: u32,
    exp: u64,
    last_attack: Instant,
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
    monsters: Vec<Monster>,
    /// xorshift64 随机态 (怪物 AI 用, 无需加密质量)
    rng: u64,
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
        // 按刷新点物化怪物 (出生位置吸附可走格)
        let mut monsters = Vec::new();
        let mut rng: u64 = 0x9E3779B97F4A7C15;
        let mut next = |limit: f64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng >> 11) as f64 / (1u64 << 53) as f64 * limit
        };
        let now = Instant::now();
        for zone in zones.values() {
            for sp in &zone.monster_spawns {
                for i in 0..sp.count {
                    let want = (
                        sp.x + next(sp.radius * 2.0) - sp.radius,
                        sp.y + next(sp.radius * 2.0) - sp.radius,
                    );
                    let (x, y) = nearest_walkable(&zone.walk, want.0, want.1);
                    monsters.push(Monster {
                        id: format!("mon_{}_{}_{}", sp.image, sp.template, i),
                        zone: zone.id.clone(),
                        home: (x, y),
                        roam: sp.radius,
                        x,
                        y,
                        dir: 4,
                        target: None,
                        chasing: false,
                        attack_until: None,
                        pending_hit: None,
                        next_attack: now,
                        next_decide: now,
                        passive: sp.passive,
                        hp: sp.hp,
                        max_hp: sp.hp,
                        damage: sp.damage,
                        exp: sp.exp,
                        dying_until: None,
                        respawn_at: None,
                        removed_sent: false,
                    });
                }
            }
        }
        info!("怪物已刷新: {} 只", monsters.len());
        Game {
            zones,
            default_zone,
            db,
            sessions,
            conns: HashMap::new(),
            players: HashMap::new(),
            tokens: HashMap::new(),
            monsters,
            rng: 0x00C0_FFEE_1234_5678,
            last_save: Instant::now(),
        }
    }

    fn rand01(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 53) as f64
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
            ClientMessage::Attack { target_id, .. } => {
                self.handle_attack(&conn_id, &target_id).await;
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
        let (level, exp) = (c.level, c.exp);
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
                hp: max_hp_for(level),
                max_hp: max_hp_for(level),
                level,
                exp,
                last_attack: Instant::now() - PLAYER_ATTACK_CD,
            },
        );
        info!("进入游戏: {character_id} {zone} @({x:.1},{y:.1})");
        self.send_enter_payload(conn_id, &character_id, x, y).await;
        self.send_player_status(&character_id).await;
    }

    /// 推送 HUD 状态 (等级/经验/HP/MP)
    async fn send_player_status(&self, character_id: &str) {
        let Some(p) = self.players.get(character_id) else {
            return;
        };
        send_to(
            &self.sessions,
            &p.conn_id,
            ServerMessage::PlayerStatus {
                level: p.level,
                experience: p.exp,
                required_experience: exp_required(p.level),
                hp: p.hp,
                max_hp: p.max_hp,
                mp: 30,
                max_mp: 30,
            },
        )
        .await;
    }

    /// 区域内所有在线玩家的连接 id
    fn zone_conns(&self, zone: &str) -> Vec<String> {
        self.players
            .values()
            .filter(|p| p.connected && p.zone == zone)
            .map(|p| p.conn_id.clone())
            .collect()
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

    /// 普攻结算：射程/冷却校验 → 扣血 → 飘字广播 → 击杀经验/升级/尸体与重生
    async fn handle_attack(&mut self, conn_id: &str, target_id: &str) {
        let now = Instant::now();
        // 攻击者
        let Some((char_id, zone, px, py, level)) = self
            .players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id)
            .map(|(id, p)| (id.clone(), p.zone.clone(), p.x, p.y, p.level))
        else {
            return;
        };
        {
            let p = self.players.get_mut(&char_id).unwrap();
            if now.duration_since(p.last_attack) < PLAYER_ATTACK_CD {
                return;
            }
            p.last_attack = now;
        }
        // 目标怪
        let Some(m) = self
            .monsters
            .iter_mut()
            .find(|m| m.id == target_id && m.zone == zone && m.alive())
        else {
            return;
        };
        let dist = ((m.x - px).powi(2) + (m.y - py).powi(2)).sqrt();
        if dist > PLAYER_ATTACK_RANGE {
            return;
        }
        let dmg = attack_for(level);
        m.hp -= dmg;
        let (mon_id, killed, exp_gain) = (m.id.clone(), m.hp <= 0, m.exp);
        if killed {
            m.dying_until = Some(now + DYING_TIME);
            m.target = None;
            m.attack_until = None;
            m.pending_hit = None;
        }
        let conns = self.zone_conns(&zone);
        broadcast_to(
            &self.sessions,
            &conns,
            ServerMessage::DamageNumber {
                target_id: mon_id.clone(),
                amount: dmg,
                is_critical: false,
            },
        )
        .await;
        if killed {
            self.award_exp(&char_id, exp_gain).await;
        }
    }

    /// 经验入账 + 升级结算
    async fn award_exp(&mut self, char_id: &str, gain: u64) {
        let Some(p) = self.players.get_mut(char_id) else {
            return;
        };
        p.exp += gain;
        let mut leveled = false;
        while p.exp >= exp_required(p.level) {
            p.exp -= exp_required(p.level);
            p.level += 1;
            p.max_hp = max_hp_for(p.level);
            p.hp = p.max_hp;
            leveled = true;
        }
        let (conn, level) = (p.conn_id.clone(), p.level);
        send_to(
            &self.sessions,
            &conn,
            ServerMessage::Notification {
                message: format!("获得经验 {gain}"),
                notification_type: "exp".into(),
            },
        )
        .await;
        if leveled {
            send_to(
                &self.sessions,
                &conn,
                ServerMessage::Notification {
                    message: format!("升级! 现在 Lv.{level}"),
                    notification_type: "levelup".into(),
                },
            )
            .await;
        }
        self.send_player_status(char_id).await;
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
                let _ = self.db.save_progress(&id, p.level, p.exp).await;
                info!("重连窗过期, 存档并移除: {id}");
            }
        }
        if now.duration_since(self.last_save) > SAVE_EVERY {
            self.last_save = now;
            for (id, p) in &self.players {
                let _ = self.db.save_position(id, &p.zone, p.x, p.y).await;
                let _ = self.db.save_progress(id, p.level, p.exp).await;
            }
        }
        // 怪物 AI (有玩家在线才跑)
        if !self.players.is_empty() {
            let hits = self.monster_ai(now);
            self.apply_monster_hits(hits).await;
        }
        // 怪物生命周期: 死亡动画到点 → 等重生; 重生到点 → 回家满血复活
        for m in self.monsters.iter_mut() {
            if m.dying_until.is_some_and(|t| now >= t) {
                m.dying_until = None;
                m.respawn_at = Some(now + RESPAWN_TIME);
            }
            if m.respawn_at.is_some_and(|t| now >= t) {
                m.respawn_at = None;
                m.hp = m.max_hp;
                m.x = m.home.0;
                m.y = m.home.1;
                m.dir = 4;
                m.removed_sent = false;
            }
        }
        // 20Hz 广播, 按区域分组 (只看得见同区域的人)
        if self.players.is_empty() {
            return;
        }
        let ts = now_ms();
        for zone_id in self.zones.keys() {
            let mut entities: Vec<EntityUpdate> = self
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
                    dir: None,
                    removed: None,
                })
                .collect();
            if entities.is_empty() {
                continue;
            }
            entities.extend(
                self.monsters
                    .iter_mut()
                    .filter(|m| &m.zone == zone_id)
                    .filter_map(|m| {
                        // 等重生: removed 只广播一次
                        if m.respawn_at.is_some() {
                            if m.removed_sent {
                                return None;
                            }
                            m.removed_sent = true;
                            return Some(EntityUpdate {
                                id: m.id.clone(),
                                position: None,
                                hp: None,
                                animation: None,
                                dir: None,
                                removed: Some(true),
                            });
                        }
                        let anim = if m.dying_until.is_some() {
                            "die"
                        } else if m.attack_until.is_some() {
                            "attack"
                        } else if m.target.is_some() {
                            "walk"
                        } else {
                            "stand"
                        };
                        Some(EntityUpdate {
                            id: m.id.clone(),
                            position: Some(Position { x: m.x, y: m.y }),
                            hp: Some(m.hp.max(0)),
                            animation: Some(anim.into()),
                            dir: Some(m.dir),
                            removed: None,
                        })
                    }),
            );
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

    /// 怪物 AI: 0.5s 决策 (仇恨/追击/拴绳/游荡) + 每 tick 连续移动。
    /// 返回攻击动画到点的命中结算 (角色 id, 伤害)。
    fn monster_ai(&mut self, now: Instant) -> Vec<(String, i32)> {
        let mut hits = Vec::new();
        let dt = TICK.as_secs_f64();
        // 决策所需的玩家位置快照 (避免与 monsters 可变借用冲突)
        let players: Vec<(String, String, f64, f64)> = self
            .players
            .iter()
            .filter(|(_, p)| p.connected)
            .map(|(id, p)| (id.clone(), p.zone.clone(), p.x, p.y))
            .collect();
        let mut rolls: Vec<f64> = Vec::with_capacity(self.monsters.len());
        for _ in 0..self.monsters.len() {
            let r = self.rand01();
            rolls.push(r);
        }
        for (mi, m) in self.monsters.iter_mut().enumerate() {
            if !m.alive() {
                continue;
            }
            let Some(zone) = self.zones.get(&m.zone) else {
                continue;
            };
            // 攻击动画期间原地不动; 到点结算命中 (目标仍在范围内才算打中)
            if let Some(t) = m.attack_until {
                if now < t {
                    continue;
                }
                m.attack_until = None;
                if let Some(target) = m.pending_hit.take() {
                    if let Some((_, _, px, py)) = players.iter().find(|(id, ..)| id == &target) {
                        let d = ((m.x - px).powi(2) + (m.y - py).powi(2)).sqrt();
                        if d <= MONSTER_HIT_RANGE {
                            hits.push((target, m.damage));
                        }
                    }
                }
            }
            if now >= m.next_decide {
                m.next_decide = now + Duration::from_millis(500);
                let home_d = ((m.x - m.home.0).powi(2) + (m.y - m.home.1).powi(2)).sqrt();
                let nearest = players
                    .iter()
                    .filter(|(_, z, _, _)| z == &m.zone)
                    .map(|(id, _, px, py)| {
                        let d = ((m.x - px).powi(2) + (m.y - py).powi(2)).sqrt();
                        (d, id.clone(), *px, *py)
                    })
                    .min_by(|a, b| a.0.total_cmp(&b.0));
                if home_d > LEASH_RANGE {
                    // 拉离过远 → 脱战回家
                    m.chasing = false;
                    m.target = Some(m.home);
                } else if let Some((d, pid, px, py)) =
                    nearest.filter(|(d, ..)| *d < AGGRO_RANGE && !m.passive)
                {
                    if d < ATTACK_RANGE {
                        m.dir = dir8_from(px - m.x, py - m.y) as u8;
                        m.target = None;
                        m.chasing = false;
                        if now >= m.next_attack {
                            m.attack_until = Some(now + ATTACK_ANIM);
                            m.next_attack = now + ATTACK_COOLDOWN;
                            m.pending_hit = Some(pid);
                        }
                    } else {
                        m.chasing = true;
                        m.target = Some((px, py));
                    }
                } else if m.target.is_none() && rolls[mi] < 0.15 {
                    // 游荡: 家附近随机踱步 (同一随机数派生角度, 低质量即可)
                    let ang = rolls[mi] * 41.0;
                    let want = (m.home.0 + ang.sin() * m.roam, m.home.1 + ang.cos() * m.roam);
                    m.chasing = false;
                    m.target = Some(want);
                }
            }
            // 连续移动 (滑行走 sim, 与玩家同源)
            if let Some((tx, ty)) = m.target {
                let (dx, dy) = (tx - m.x, ty - m.y);
                let dist = (dx * dx + dy * dy).sqrt();
                if dist < 0.15 {
                    m.target = None;
                    continue;
                }
                let speed = if m.chasing { CHASE_SPEED } else { WANDER_SPEED };
                let step = (speed * dt).min(dist);
                let (sx, sy) = (dx / dist * step, dy / dist * step);
                m.dir = dir8_from(dx, dy) as u8;
                let (nx, ny) = zone.walk.try_move(m.x, m.y, sx, sy, BODY_RADIUS);
                if (nx - m.x).abs() < 1e-9 && (ny - m.y).abs() < 1e-9 {
                    m.target = None; // 完全卡死则放弃本次目标
                } else {
                    m.x = nx;
                    m.y = ny;
                }
            }
        }
        hits
    }

    /// 怪物命中玩家: 扣血/飘字/死亡回城
    async fn apply_monster_hits(&mut self, hits: Vec<(String, i32)>) {
        for (char_id, dmg) in hits {
            let Some(p) = self.players.get_mut(&char_id) else {
                continue;
            };
            p.hp -= dmg;
            let (zone, conn, dead) = (p.zone.clone(), p.conn_id.clone(), p.hp <= 0);
            let conns = self.zone_conns(&zone);
            broadcast_to(
                &self.sessions,
                &conns,
                ServerMessage::DamageNumber {
                    target_id: char_id.clone(),
                    amount: dmg,
                    is_critical: false,
                },
            )
            .await;
            if dead {
                // 死亡: 回本区域出生点满血复活 (惩罚机制后续)
                let spawn = self
                    .zones
                    .get(&zone)
                    .map(|z| z.spawn)
                    .unwrap_or((330.5, 150.5));
                let p = self.players.get_mut(&char_id).unwrap();
                p.hp = p.max_hp;
                p.x = spawn.0;
                p.y = spawn.1;
                send_to(
                    &self.sessions,
                    &conn,
                    ServerMessage::Notification {
                        message: "你死了, 已在出生点复活".into(),
                        notification_type: "death".into(),
                    },
                )
                .await;
                self.send_enter_zone_only(&conn, &char_id, spawn.0, spawn.1)
                    .await;
            }
            self.send_player_status(&char_id).await;
        }
    }
}
