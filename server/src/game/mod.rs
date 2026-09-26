//! 游戏循环：动作消费 + 20Hz 区域广播 + 会话/断线恢复。
//! M2 范围：登录/建角/进图/移动（sim 校验）/心跳/Resume；玩法系统随 M3 迁入。

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// 配置数据类型已下沉 crates/gamedata (hub 与区服共用); 此处二次导出保持
// 站内 `crate::game::X` 路径与改造前一致
pub use gamedata::defs::{
    BossDef, DropEntry, GameData, ItemDef, NpcDef, NpcDialogPage, NpcOptionDef, QuestDef,
    ShopEntry, SkillDef, SkillKind, ZoneSidecar,
};

mod combat;
use monster::{materialize_bosses, materialize_monsters};
mod items;
mod monster;
mod trade;
mod pk;
mod npc;
mod quest;
#[cfg(test)]
mod tests;
mod tick;

use protocol::{ClientMessage, EntityUpdate, Position, ServerMessage, PROTOCOL_VERSION};
use sim::layout::fx as fxl;
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
    /// 原始边车 (管理台回显/写回)
    pub sidecar: ZoneSidecar,
}

// ── 边车配置 (zones/<map>.json; 服务器与管理台共用) ──

/// 从地图文件 + 边车构建区域 (行走网格/出生点吸附)
pub fn load_zone(map_path: &std::path::Path, map_name: &str, sidecar: ZoneSidecar) -> Option<Zone> {
    let map = mir_formats::map::parse(&std::fs::read(map_path).ok()?).ok()?;
    let walk = WalkGrid::from_cells(map.width, map.height, |x, y| {
        map.cell(x, y).is_some_and(|c| c.blocked)
    });
    let want = sidecar
        .spawn
        .unwrap_or((map.width as f64 / 2.0, map.height as f64 / 2.0));
    let spawn = nearest_walkable(&walk, want.0, want.1);
    let name = sidecar.name.clone().unwrap_or_else(|| map_name.to_string());
    info!(
        "区域 {name} [{map_name}] ({:?} {}x{}), 出生点 ({:.1},{:.1}), 传送门 {}",
        map.kind,
        map.width,
        map.height,
        spawn.0,
        spawn.1,
        sidecar.portals.len()
    );
    Some(Zone {
        id: map_name.to_string(),
        name,
        walk,
        spawn,
        portals: sidecar
            .portals
            .iter()
            .map(|p| Portal {
                x: p.x,
                y: p.y,
                to_zone: p.to.clone(),
                to_x: p.to_x,
                to_y: p.to_y,
            })
            .collect(),
        monster_spawns: sidecar
            .monsters
            .iter()
            .map(|m| MonsterSpawn {
                template: m.template.clone(),
                image: m.image,
                x: m.x,
                y: m.y,
                count: m.count,
                radius: m.radius,
                passive: m.passive,
                hp: m.hp,
                damage: m.damage,
                exp: m.exp,
                drops: m
                    .drops
                    .iter()
                    .map(|d| DropEntry {
                        item: d.item.clone(),
                        chance: d.chance,
                    })
                    .collect(),
            })
            .collect(),
        sidecar,
    })
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
    pub drops: Vec<DropEntry>,
}

// ── 战斗参数 (M3.2 基础值; 装备加成随 3.4 接入) ──
const PLAYER_ATTACK_RANGE: f64 = 2.5;
const PLAYER_ATTACK_CD: Duration = Duration::from_millis(600);
const MONSTER_HIT_RANGE: f64 = 2.2;
const DYING_TIME: Duration = Duration::from_millis(1300);
/// 受击硬直: 顿帧 + 播受击姿态 (经典"被打顿一下"; 毒跳伤不触发)
const STRUCK_ANIM: Duration = Duration::from_millis(300);
const RESPAWN_TIME: Duration = Duration::from_secs(30);
/// 尸体最长停留时长 (经典传奇: 尸体躺一阵才消失; 到重生时刻或此上限先到者为准)
const CORPSE_CAP: Duration = Duration::from_secs(60);

fn max_hp_for(level: u32) -> i32 {
    40 + level as i32 * 12
}
fn max_mp_for(level: u32) -> i32 {
    30 + level as i32 * 8
}
fn attack_for(level: u32) -> i32 {
    4 + level as i32 * 2
}
/// 升到下一级所需累计经验
fn exp_required(level: u32) -> u64 {
    level as u64 * 100
}

// ─────────── 游戏数据 (JSON 配置驱动, server/data/*.json 可覆盖内置默认) ───────────

/// 毒伤跳间隔 (经典绿毒节奏)
pub const POISON_TICK: Duration = Duration::from_secs(2);

/// 统一状态效果 (僵直/后续减速/定身/隐身/护盾… 的共同地基)。
/// 中毒因带专属跳伤结算仍走 [`Poison`], 其余控制/增益类状态都进这张表。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum StatusKind {
    /// 僵直: 不移动/不攻击/不索敌 (野蛮冲撞附带)
    Stun,
}

impl StatusKind {
    pub(super) fn name(self) -> &'static str {
        match self {
            StatusKind::Stun => "stun",
        }
    }
}

#[derive(Clone)]
pub(super) struct StatusState {
    pub(super) until: Instant,
}

pub(super) use sim::DASH_SPEED;
/// 冲锋撞击判定距离 (与怪物碰撞体贴合)
const DASH_HIT_RANGE: f64 = 1.1;
/// 冲锋击退距离 (格)
const DASH_KNOCKBACK: f64 = 1.0;

/// 玩家冲锋进行态 (野蛮冲撞): 服务端按 tick 推进, 撞击/走完/撞墙即清
pub(super) struct DashState {
    pub(super) dir: (f64, f64),
    pub(super) remaining: f64,
    pub(super) dmg: i32,
    pub(super) stun_secs: f64,
    /// 撞击特效 (广播命中段用)
    pub(super) skill_id: String,
    pub(super) fx: String,
    pub(super) fx_base: u32,
    pub(super) fx_frames: u8,
    pub(super) skill_level: u32,
}

/// 怪物中毒状态 (施毒术上毒; 重复施毒刷新时长)
#[derive(Clone)]
pub(super) struct Poison {
    pub(super) until: Instant,
    pub(super) next_tick: Instant,
    pub(super) tick_dmg: i32,
    /// 跳伤/击杀归属 (经验/掉落/任务)
    pub(super) attacker: String,
}

/// 单技能修炼进度 (随角色存档)
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct SkillProgress {
    pub level: u32,
    pub train: u32,
}

/// 回收价占售价的比例 (经典传奇卖给 NPC 都要打折)
pub const SELL_RATE: f64 = 0.5;

static DATA: std::sync::RwLock<Option<std::sync::Arc<GameData>>> = std::sync::RwLock::new(None);

pub fn data() -> std::sync::Arc<GameData> {
    if let Some(d) = DATA.read().unwrap().as_ref() {
        return d.clone();
    }
    let mut w = DATA.write().unwrap();
    w.get_or_insert_with(|| std::sync::Arc::new(GameData::builtin()))
        .clone()
}

/// 热替换配置 (管理台校验通过后调用)
pub fn set_data(d: GameData) {
    *DATA.write().unwrap() = Some(std::sync::Arc::new(d));
}

/// 在线人数 (hub 心跳上报用; 游戏循环每拍刷新)
static ONLINE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn online_count() -> u32 {
    ONLINE.load(std::sync::atomic::Ordering::Relaxed)
}

fn skills_for(class: protocol::CharacterClass) -> Vec<SkillDef> {
    let d = data();
    match class {
        protocol::CharacterClass::Warrior => d.skills.warrior.clone(),
        protocol::CharacterClass::Mage => d.skills.mage.clone(),
        protocol::CharacterClass::Taoist => d.skills.taoist.clone(),
    }
}

// ─────────── 物品 (M3.4; 模板静态表, 掉落表走边车) ───────────

fn item_def(template: &str) -> Option<ItemDef> {
    data()
        .items
        .iter()
        .find(|d| d.template == template)
        .cloned()
}

fn make_item(template: &str) -> Option<protocol::ItemInfo> {
    let d = item_def(template)?;
    Some(protocol::ItemInfo {
        id: uuid::Uuid::new_v4().to_string(),
        template: d.template.clone(),
        name: d.name.clone(),
        slot: d.slot.clone(),
        attack: d.attack,
        magic: d.magic,
        spirit: d.spirit,
        defense: d.defense,
        hp: d.hp,
        image: d.image,
        shape: d.shape,
        bonus: 0,
        dur: d.durability,
        max_dur: d.durability,
    })
}

/// 损坏判定: 有耐久概念且已归零 → 属性不再计入
fn item_ok(i: &protocol::ItemInfo) -> bool {
    i.max_dur <= 0 || i.dur > 0
}

/// 背包容量上限 (与客户端 panels::BAG_SLOTS 一致)
const MAX_INVENTORY: usize = 50;
/// 仓库容量 (格)
const STORAGE_CAP: usize = 50;
/// 地面物品存留时长
const GROUND_ITEM_TTL: Duration = Duration::from_secs(60);

/// 地面掉落物
struct GroundItem {
    id: String,
    zone: String,
    item: protocol::ItemInfo,
    x: f64,
    y: f64,
    expire: Instant,
}

// ─────────── 任务 (M3.5; 三链新手任务, 迁自旧服务器) ───────────

fn quest_def(id: &str) -> Option<QuestDef> {
    data().quests.iter().find(|q| q.id == id).cloned()
}

/// 任务进度 (持久化)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QuestProgress {
    /// 1=进行中 2=已完成
    pub state: u8,
    pub counts: Vec<u32>,
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
    template: String,
    /// 模板等级 (展示用, 随实体广播)
    level: u32,
    /// 音效基址 (客户端 mon/{基址:03}-动作.ogg; 模板 -1 时落形象号)
    sound: u16,
    /// 显示名 (BOSS 击杀公告用; 普通怪为空)
    name: String,
    /// 是 BOSS: 重生慢, 死亡可全服公告
    boss: bool,
    /// 重生间隔 (普通怪 RESPAWN_TIME, BOSS 按配置)
    respawn: Duration,
    /// 击杀后是否全服公告
    announce: bool,
    /// 客户端图库号 (Data/Monster/{image:03}.Lib)
    image: u16,
    /// 库内外观基址 (一库多怪时该怪的起始帧)
    image_base: u32,
    zone: String,
    home: (f64, f64),
    /// 游荡活动半径 (来自刷新点)
    roam: f64,
    x: f64,
    y: f64,
    dir: u8,
    /// 当前移动目标 (None = 站立)
    target: Option<(f64, f64)>,
    /// 活跃状态效果 (statuses_sent: 广播过非空后需补发一次空表清除)
    statuses: HashMap<StatusKind, StatusState>,
    statuses_sent: bool,
    /// 受击硬直到点 (播受击姿态 + 暂停移动; 不打断已出手的攻击结算)
    struck_until: Option<Instant>,
    /// 仇恨对象 (最后攻击我的实体 id; 被动怪凭它参战, 主动怪凭它锁定)。
    /// 目标失效/脱战回家/重生时清除
    aggro_target: Option<String>,
    /// 宠物主人 (召唤骷髅等; Some = 友方, 跟随主人/攻击敌怪/不重生)
    owner: Option<String>,
    /// 宝宝等级 1-7 (杀怪得经验升级, 形态/数值随级; 死亡/消散即归零)
    pet_level: u32,
    /// 当前级已积累经验 (升级所需 = PET_EXP_BASE × 当前级)
    pet_exp: u64,
    /// 召唤到期时刻 (None = 直到死亡/主人离场)
    summon_until: Option<Instant>,
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
    drops: Vec<DropEntry>,
    /// 死亡动画播放中 (到点转入等待重生)
    dying_until: Option<Instant>,
    /// 尸体停留截止 (死亡动画结束 → 躺尸到此时刻才广播移除)
    corpse_until: Option<Instant>,
    /// 等待重生 (期间不广播不参与 AI)
    respawn_at: Option<Instant>,
    /// removed=true 是否已广播过
    removed_sent: bool,
    /// 中毒状态 (None = 无毒)
    poison: Option<Poison>,
}

impl Monster {
    /// 清掉过期状态并返回活跃状态名 (每拍广播用)
    fn active_statuses(&mut self, now: Instant) -> Vec<String> {
        self.statuses.retain(|_, st| now < st.until);
        self.statuses.keys().map(|k| k.name().to_string()).collect()
    }

    fn stunned(&self, now: Instant) -> bool {
        self.statuses
            .get(&StatusKind::Stun)
            .is_some_and(|st| now < st.until)
    }

    fn alive(&self) -> bool {
        // hp 判定必不可少: 宠物不重生 (respawn_at 恒 None), 尸体期若无它
        // 会被当活体继续索敌出手 — "骨堆延迟咬怪" bug 的根源
        self.hp > 0 && self.dying_until.is_none() && self.respawn_at.is_none()
    }
}

/// 从 (x,y) 就近找可站立点（螺旋外扩, 最远 60 格）
/// 解析传送参数 "地图:x:y" (地图名自身含 `.` 但不含 `:`)
fn parse_teleport(arg: &str) -> Option<(String, f64, f64)> {
    let mut it = arg.split(':');
    let map = it.next()?.trim();
    let x: f64 = it.next()?.trim().parse().ok()?;
    let y: f64 = it.next()?.trim().parse().ok()?;
    (!map.is_empty() && it.next().is_none()).then(|| (map.to_string(), x, y))
}

/// 就近找一个既可站立、又不压在 `blockers` 上的位置
///
/// 用于刷怪/重生: 怪不该刷在玩家身上。找不到 (被围满了) 就退回可站立的位置 ——
/// 宁可短暂重叠也不能不刷, 反正 resolve_move 允许从重叠里脱出。
pub fn nearest_free(walk: &WalkGrid, x: f64, y: f64, blockers: &[(f64, f64)]) -> (f64, f64) {
    let clear = |px: f64, py: f64| {
        blockers.iter().all(|b| {
            let (dx, dy) = (b.0 - px, b.1 - py);
            dx * dx + dy * dy >= sim::ENTITY_CLEARANCE * sim::ENTITY_CLEARANCE
        })
    };
    if walk.is_walkable_circle(x, y, BODY_RADIUS) && clear(x, y) {
        return (x, y);
    }
    let (cx, cy) = (x.floor() as i64, y.floor() as i64);
    for r in 1..=12i64 {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs() != r && dy.abs() != r {
                    continue;
                }
                let (tx, ty) = ((cx + dx) as f64 + 0.5, (cy + dy) as f64 + 0.5);
                if walk.is_walkable_circle(tx, ty, BODY_RADIUS) && clear(tx, ty) {
                    return (tx, ty);
                }
            }
        }
    }
    nearest_walkable(walk, x, y)
}

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
    mp: i32,
    max_mp: i32,
    level: u32,
    exp: u64,
    gold: u64,
    last_attack: Instant,
    /// 技能 id → 冷却结束时刻
    cooldowns: HashMap<String, Instant>,
    inventory: Vec<protocol::ItemInfo>,
    equipment: HashMap<String, protocol::ItemInfo>,
    /// 仓库 (NPC 存取; 与背包同一套持久化)
    storage: Vec<protocol::ItemInfo>,
    quests: HashMap<String, QuestProgress>,
    /// 技能 id → 修炼进度
    skills: HashMap<String, SkillProgress>,
    /// 冲锋进行态 (Some = 冲锋中, 忽略移动包)
    dash: Option<DashState>,
    /// 全体攻击模式 (false = 和平, 默认)
    pk_all: bool,
    /// PK 值 (杀白名 +100, ≥100 红名, 在线每 2 分钟 -1)
    pk_points: u32,
    /// 灰名到期 (打中白名玩家标 60 秒)
    grey_until: Option<Instant>,
    /// 死亡状态 (到点复活); Some 期间移动/攻击/施法全阻断
    dead_until: Option<Instant>,
    /// 中毒 (到期, 每跳伤害, 下一跳时刻, 施毒者)
    poison: Option<(Instant, i32, Instant, String)>,
}

impl PlayerState {
    fn equip_attack(&self) -> i32 {
        self.equipment.values().filter(|i| item_ok(i)).map(|i| i.attack).sum()
    }
    fn equip_magic(&self) -> i32 {
        self.equipment.values().filter(|i| item_ok(i)).map(|i| i.magic).sum()
    }
    fn equip_spirit(&self) -> i32 {
        self.equipment.values().filter(|i| item_ok(i)).map(|i| i.spirit).sum()
    }
    fn equip_defense(&self) -> i32 {
        self.equipment.values().filter(|i| item_ok(i)).map(|i| i.defense).sum()
    }
    fn equip_hp(&self) -> i32 {
        self.equipment.values().filter(|i| item_ok(i)).map(|i| i.hp).sum()
    }
    /// 装备/升级后重算上限并夹住当前值
    fn recalc(&mut self) {
        self.max_hp = max_hp_for(self.level) + self.equip_hp();
        self.hp = self.hp.min(self.max_hp);
        self.max_mp = max_mp_for(self.level);
        self.mp = self.mp.min(self.max_mp);
    }
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
    /// 全部可用 .map 文件 (含未接入区域; 管理台新增地图用)
    map_files: HashMap<String, std::path::PathBuf>,
    /// 地面物品 (掉落/丢弃)
    ground: Vec<GroundItem>,
    next_drop_id: u64,
    /// xorshift64 随机态 (怪物 AI 用, 无需加密质量)
    rng: u64,
    /// 刷新点补位实例的 id 序号 (诱惑把原实例转宠后, 补一只替补重生)
    next_slot_id: u64,
    last_save: Instant,
    last_regen: Instant,
    /// 玩家延迟上毒队列 (到点, 施毒者, 目标, 每跳, 秒)
    pending_player_poisons: Vec<(Instant, String, String, i32, f64)>,
    /// 上次 PK 值衰减时刻
    last_pk_decay: Instant,
    /// 在途交易 (一人至多一笔)
    trades: Vec<trade::Trade>,
    /// 待回应交易邀请: 受邀者 char_id → 发起者 char_id
    trade_invites: HashMap<String, String>,
    /// 技能延迟结算队列: 与客户端起手/飞行编排对齐 (到点才掉血)
    pending_hits: Vec<PendingHit>,
    /// 延迟上毒队列 (到点时刻, 施毒者, 目标怪, 每跳伤害, 持续秒)
    pending_poisons: Vec<(Instant, String, String, i32, f64)>,
    /// 登录器一次性启动票据: ticket → (account_id, 过期时刻)。60 秒 TTL, 用后即焚
    launch_tickets: HashMap<String, (String, Instant)>,
}

/// 延迟结算条目: (到点时刻, 施放者, 命中列表)
type PendingHit = (Instant, String, Vec<(String, i32)>);

/// 起手段帧数实测缓存 (packs/magic 库 10 槽块, 与客户端 block_len 同判据)
static FX_BLOCK_LEN: std::sync::Mutex<Option<std::collections::HashMap<(String, i64), u8>>> =
    std::sync::Mutex::new(None);

fn fx_block_len(fx: &str, base: i64) -> u8 {
    if base < 0 || fx.is_empty() {
        return 0;
    }
    let Ok(mut g) = FX_BLOCK_LEN.lock() else {
        return 0;
    };
    let cache = g.get_or_insert_with(Default::default);
    if let Some(&k) = cache.get(&(fx.to_string(), base)) {
        return k;
    }
    let path = crate::admin::packs_root()
        .join("magic")
        .join(format!("{}.mfl", fxl::file_stem(fx)));
    let k = mir_formats::mfl::AnyLib::open(&path)
        .ok()
        .map(|l| {
            let mut k = 0u8;
            for i in 0..fxl::SLOT as usize {
                if l.dims(base as usize + i)
                    .is_some_and(|(w, h)| w >= 8 && h >= 8)
                {
                    k += 1;
                } else {
                    break;
                }
            }
            k
        })
        .unwrap_or(0);
    cache.insert((fx.to_string(), base), k);
    k
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
        map_files: HashMap<String, std::path::PathBuf>,
    ) -> Self {
        let mut rng: u64 = 0x9E3779B97F4A7C15;
        let mut monsters = Vec::new();
        for zone in zones.values() {
            // 开服时还没有人在线
            monsters.extend(materialize_monsters(zone, &mut rng, &[]));
            monsters.extend(materialize_bosses(zone, &[]));
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
            map_files,
            ground: Vec::new(),
            next_drop_id: 1,
            rng: 0x00C0_FFEE_1234_5678,
            next_slot_id: 1,
            pending_player_poisons: Vec::new(),
            last_pk_decay: Instant::now(),
            trades: Vec::new(),
            trade_invites: HashMap::new(),
            last_save: Instant::now(),
            last_regen: Instant::now(),
            pending_hits: Vec::new(),
            pending_poisons: Vec::new(),
            launch_tickets: HashMap::new(),
        }
    }

    fn rand01(&mut self) -> f64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 11) as f64 / (1u64 << 53) as f64
    }

    pub async fn run(
        mut self,
        mut events: mpsc::UnboundedReceiver<ConnEvent>,
        admin: Option<mpsc::UnboundedReceiver<crate::admin::AdminCmd>>,
    ) {
        let started = Instant::now();
        let mut tick = tokio::time::interval(TICK);
        // 无管理台时用永不来消息的空通道占位
        let mut admin = admin.unwrap_or_else(|| mpsc::unbounded_channel().1);
        loop {
            tokio::select! {
                ev = events.recv() => {
                    match ev {
                        Some(ConnEvent::Action(a)) => self.handle(a.conn_id, a.message).await,
                        Some(ConnEvent::Disconnected { conn_id }) => self.on_disconnect(&conn_id).await,
                        None => break,
                    }
                }
                Some(cmd) = admin.recv() => self.handle_admin(cmd, started).await,
                _ = tick.tick() => {
                    ONLINE.store(self.players.len() as u32, std::sync::atomic::Ordering::Relaxed);
                    self.tick().await;
                }
            }
        }
    }

    /// 边车热应用: 校验 → 写文件 → 重建区域数据与该区怪物
    async fn apply_zone_sidecar(
        &mut self,
        map: &str,
        value: serde_json::Value,
    ) -> Result<(), String> {
        if !self.zones.contains_key(map) {
            return Err(format!("区域未加载: {map}"));
        }
        let sidecar: ZoneSidecar =
            serde_json::from_value(value.clone()).map_err(|e| format!("边车解析失败: {e}"))?;
        // 校验: 掉落物品与传送门目标存在
        let d = data();
        for m in &sidecar.monsters {
            if !d.monsters.iter().any(|md| md.id == m.template) {
                return Err(format!(
                    "刷新点引用不存在的怪物模板: {} (先在「怪物设置」里建)",
                    m.template
                ));
            }
            for dr in &m.drops {
                if !d.items.iter().any(|i| i.template == dr.item) {
                    return Err(format!("掉落引用不存在的物品: {}", dr.item));
                }
            }
        }
        for pt in &sidecar.portals {
            if !self.zones.contains_key(&pt.to) && !self.map_files.contains_key(&pt.to) {
                return Err(format!("传送门指向不存在的地图: {}", pt.to));
            }
        }
        let map_path = self
            .map_files
            .get(map)
            .cloned()
            .ok_or_else(|| format!("找不到地图文件: {map}"))?;
        let zone = load_zone(&map_path, map, sidecar.clone()).ok_or("地图解析失败".to_string())?;
        // 持久化到配置库
        crate::config_store::save_zone(self.db.pool(), map, &sidecar)
            .await
            .map_err(|e| format!("保存区域失败: {e}"))?;
        self.replace_zone(zone).await;
        Ok(())
    }

    /// 热替换一个区域: 换数据 + 重建该区怪物 (先广播 removed)
    async fn replace_zone(&mut self, zone: Zone) {
        let map = zone.id.clone();
        let map = map.as_str();
        let removed: Vec<String> = self
            .monsters
            .iter()
            .filter(|m| m.zone == map)
            .map(|m| m.id.clone())
            .collect();
        if !removed.is_empty() {
            let conns = self.zone_conns(map);
            let entities: Vec<EntityUpdate> = removed
                .iter()
                .map(|id| EntityUpdate {
                    id: id.clone(),
                    position: None,
                    hp: None,
                    animation: None,
                    dir: None,
                    removed: Some(true),
                    armour: None,
                    weapon: None,
                    image: None,
                    image_base: None,
                    poisoned: None,
                    statuses: None,
                    owner: None,
                    name: None,
                    level: None,
                    sound: None,
                    pk: None,
                })
                .collect();
            broadcast_to(
                &self.sessions,
                &conns,
                ServerMessage::StateUpdate {
                    entities,
                    timestamp: now_ms(),
                },
            )
            .await;
        }
        self.monsters.retain(|m| m.zone != map);
        let mut rng = self.rng | 1;
        let here = Self::player_positions(&self.players, &zone.id);
        let mut fresh = materialize_monsters(&zone, &mut rng, &here);
        fresh.extend(materialize_bosses(&zone, &here));
        info!("区域 {map} 热重载: 怪物 {} 只", fresh.len());
        self.monsters.extend(fresh);
        self.zones.insert(map.to_string(), zone);
    }

    /// 接入新地图: 默认边车 → 加载 → 写文件
    async fn add_zone(&mut self, map: &str) -> Result<(), String> {
        let map = map.to_lowercase();
        if self.zones.contains_key(&map) {
            return Err(format!("区域已存在: {map}"));
        }
        let map_path = self
            .map_files
            .get(&map)
            .cloned()
            .ok_or_else(|| format!("资源目录中没有该地图: {map}"))?;
        let sidecar = ZoneSidecar::default();
        let zone = load_zone(&map_path, &map, sidecar.clone()).ok_or("地图解析失败".to_string())?;
        crate::config_store::save_zone(self.db.pool(), &map, &sidecar)
            .await
            .map_err(|e| format!("保存区域失败: {e}"))?;
        info!("新区域已接入: {map} ({})", zone.name);
        self.zones.insert(map.clone(), zone);
        Ok(())
    }

    /// 移除区域: 缺省区/有人在场/被传送门指向 时拒绝
    async fn delete_zone(&mut self, map: &str) -> Result<(), String> {
        if !self.zones.contains_key(map) {
            return Err(format!("区域不存在: {map}"));
        }
        if map == self.default_zone {
            return Err("缺省出生区域不可移除".into());
        }
        let present = self.players.values().filter(|p| p.zone == map).count();
        if present > 0 {
            return Err(format!("尚有 {present} 名角色在该区域内"));
        }
        let refs: Vec<String> = self
            .zones
            .values()
            .filter(|z| z.id != map && z.portals.iter().any(|p| p.to_zone == map))
            .map(|z| z.name.clone())
            .collect();
        if !refs.is_empty() {
            return Err(format!("以下区域仍有传送门指向此地图: {}", refs.join(", ")));
        }
        crate::config_store::delete_zone(self.db.pool(), map)
            .await
            .map_err(|e| format!("删除配置失败: {e}"))?;
        self.monsters.retain(|m| m.zone != map);
        self.zones.remove(map);
        info!("区域已移除: {map}");
        Ok(())
    }

    /// 全员立即存档
    async fn save_all(&self) {
        for (id, p) in &self.players {
            let _ = self.db.save_position(id, &p.zone, p.x, p.y).await;
            let _ = self.db.save_progress(id, p.level, p.exp).await;
            let _ = self.db.save_items(id, &p.inventory, &p.equipment).await;
            let _ = self.db.save_quests(id, &p.quests).await;
        }
    }

    /// 管理台命令 (与玩法同循环, 无并发状态)
    async fn handle_admin(&mut self, cmd: crate::admin::AdminCmd, started: Instant) {
        use crate::admin::{AdminCmd, PlayerRow, StatusSnapshot};
        match cmd {
            AdminCmd::Status(reply) => {
                let players = self
                    .players
                    .values()
                    .map(|p| PlayerRow {
                        name: p.character.name.clone(),
                        level: p.level,
                        zone: self
                            .zones
                            .get(&p.zone)
                            .map(|z| z.name.clone())
                            .unwrap_or_else(|| p.zone.clone()),
                        x: p.x,
                        y: p.y,
                        hp: p.hp,
                        max_hp: p.max_hp,
                        connected: p.connected,
                    })
                    .collect();
                let _ = reply.send(StatusSnapshot {
                    uptime_secs: started.elapsed().as_secs(),
                    players,
                    monsters_alive: self.monsters.iter().filter(|m| m.alive()).count(),
                    monsters_total: self.monsters.len(),
                    ground_items: self.ground.len(),
                    zones: self.zones.values().map(|z| z.name.clone()).collect(),
                });
            }
            AdminCmd::ConfigReloaded { kind } => {
                self.refresh_bosses().await;
                if kind == "monsters" {
                    self.refresh_spawns().await;
                }
                // 在线玩家即时重推技能表 (数值/新技能立即可见)
                let ids: Vec<String> = self
                    .players
                    .iter()
                    .filter(|(_, p)| p.connected)
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in ids {
                    self.send_skill_list(&id).await;
                    if let Some((conn, zone)) = self
                        .players
                        .get(&id)
                        .map(|p| (p.conn_id.clone(), p.zone.clone()))
                    {
                        self.send_npc_list(&conn, &zone).await;
                    }
                }
                info!(
                    "配置热重载: BOSS 已重刷, 已重推 {} 名在线玩家技能与 NPC",
                    self.players.len()
                );
            }
            AdminCmd::Broadcast(message) => {
                let conns: Vec<String> = self
                    .players
                    .values()
                    .filter(|p| p.connected)
                    .map(|p| p.conn_id.clone())
                    .collect();
                broadcast_to(
                    &self.sessions,
                    &conns,
                    ServerMessage::Notification {
                        message: format!("[公告] {message}"),
                        notification_type: "system".into(),
                    },
                )
                .await;
            }
            AdminCmd::Kick { name, done } => {
                let conn = self
                    .players
                    .values()
                    .find(|p| p.character.name == name && p.connected)
                    .map(|p| p.conn_id.clone());
                let kicked = match conn {
                    Some(c) => {
                        send_to(
                            &self.sessions,
                            &c,
                            ServerMessage::Error {
                                message: "已被管理员断开连接".into(),
                            },
                        )
                        .await;
                        self.on_disconnect(&c).await;
                        true
                    }
                    None => false,
                };
                let _ = done.send(kicked);
            }
            AdminCmd::ApplySnapshot { snap } => {
                let rev = snap.rev;
                set_data(snap.data);
                // 区域对齐: 新增/变更重建, 消失且无人时移除
                for (map, sc) in &snap.zones {
                    let changed = match self.zones.get(map) {
                        None => true,
                        Some(z) => {
                            serde_json::to_value(&z.sidecar).ok() != serde_json::to_value(sc).ok()
                        }
                    };
                    if !changed {
                        continue;
                    }
                    let Some(path) = self.map_files.get(map).cloned() else {
                        warn!("hub 快照区域 {map} 找不到地图文件, 跳过");
                        continue;
                    };
                    match load_zone(&path, map, sc.clone()) {
                        Some(zone) => self.replace_zone(zone).await,
                        None => warn!("hub 快照区域 {map} 地图解析失败, 跳过"),
                    }
                }
                let gone: Vec<String> = self
                    .zones
                    .keys()
                    .filter(|m| !snap.zones.contains_key(*m) && **m != self.default_zone)
                    .cloned()
                    .collect();
                for m in gone {
                    let present = self.players.values().filter(|p| p.zone == m).count();
                    if present > 0 {
                        warn!("hub 快照移除区域 {m}, 但尚有 {present} 人在场, 保留");
                        continue;
                    }
                    self.monsters.retain(|mo| mo.zone != m);
                    self.zones.remove(&m);
                    info!("区域 {m} 已按 hub 快照移除");
                }
                // 与 ConfigReloaded 同步收尾: BOSS/刷新点重刷 + 重推在线玩家
                self.refresh_bosses().await;
                self.refresh_spawns().await;
                let ids: Vec<String> = self
                    .players
                    .iter()
                    .filter(|(_, p)| p.connected)
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in ids {
                    self.send_skill_list(&id).await;
                    if let Some((conn, zone)) = self
                        .players
                        .get(&id)
                        .map(|p| (p.conn_id.clone(), p.zone.clone()))
                    {
                        self.send_npc_list(&conn, &zone).await;
                    }
                }
                crate::hubclient::APPLIED_REV.store(rev, std::sync::atomic::Ordering::Relaxed);
                info!("hub 配置 rev={rev} 已生效");
            }
            AdminCmd::SaveAll(done) => {
                self.save_all().await;
                let _ = done.send(());
            }
            AdminCmd::CheckPlacement {
                npcs,
                bosses,
                quests,
                done,
            } => {
                let mut errs = self.check_npc_placement(&npcs);
                errs.extend(self.check_boss_placement(&bosses));
                errs.extend(self.check_quest_targets(&quests));
                let _ = done.send(errs);
            }
            AdminCmd::ZonesInfo(reply) => {
                let zones: Vec<crate::admin::ZoneRow> = self
                    .zones
                    .values()
                    .map(|z| crate::admin::ZoneRow {
                        map: z.id.clone(),
                        name: z.name.clone(),
                        sidecar: serde_json::to_value(&z.sidecar).unwrap_or_default(),
                    })
                    .collect();
                let available: Vec<String> = self
                    .map_files
                    .keys()
                    .filter(|m| !self.zones.contains_key(*m))
                    .cloned()
                    .collect();
                let _ = reply.send(crate::admin::ZonesInfo { zones, available });
            }
            AdminCmd::PutZone { map, sidecar, done } => {
                let r = self.apply_zone_sidecar(&map, sidecar).await;
                if r.is_ok() {
                    crate::admin::audit("put_zone", &map);
                }
                let _ = done.send(r);
            }
            AdminCmd::DeleteZone { map, done } => {
                let _ = done.send(self.delete_zone(&map).await);
            }
            AdminCmd::MapThumbInfo { map, done } => {
                let info = self.zones.get(&map).and_then(|z| {
                    Some(crate::admin::MapThumb {
                        path: self.map_files.get(&map)?.clone(),
                        spawn: z.spawn,
                        portals: z.portals.iter().map(|p| (p.x, p.y)).collect(),
                        spawns: z.monster_spawns.iter().map(|m| (m.x, m.y)).collect(),
                    })
                });
                let _ = done.send(info);
            }
            AdminCmd::AddZone { map, done } => {
                let r = self.add_zone(&map).await;
                if r.is_ok() {
                    crate::admin::audit("add_zone", &map);
                }
                let _ = done.send(r);
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
            ClientMessage::Register {
                username,
                password,
                security_question,
                security_answer,
            } => {
                let reply = match self.db.register(&username, &password).await {
                    Ok(Some(account_id)) => {
                        // 密保可选: 注册时一并落库 (找回密码用)
                        if let (Some(q), Some(a)) = (&security_question, &security_answer) {
                            if !q.trim().is_empty() && !a.trim().is_empty() {
                                let _ = self.db.set_security(&account_id, q, a).await;
                            }
                        }
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
            ClientMessage::RequestTicket => {
                // 已登录连接才可申请; 票据 60 秒有效
                let Some(account_id) = self.account_of(&conn_id) else {
                    return;
                };
                let ticket = uuid::Uuid::new_v4().to_string();
                self.launch_tickets.insert(
                    ticket.clone(),
                    (account_id, Instant::now() + Duration::from_secs(60)),
                );
                // 顺手清过期票
                let now = Instant::now();
                self.launch_tickets.retain(|_, (_, exp)| *exp > now);
                send_to(
                    &self.sessions,
                    &conn_id,
                    ServerMessage::LaunchTicket { ticket },
                )
                .await;
            }
            ClientMessage::TicketAuth { ticket } => {
                let hit = self
                    .launch_tickets
                    .remove(&ticket)
                    .filter(|(_, exp)| *exp > Instant::now());
                match hit {
                    Some((account_id, _)) => {
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
                    None => {
                        send_to(
                            &self.sessions,
                            &conn_id,
                            ServerMessage::LoginResult {
                                success: false,
                                account_id: None,
                                message: "启动票据无效或已过期, 请回登录器重新登录".into(),
                            },
                        )
                        .await;
                    }
                }
            }
            ClientMessage::ResetPassword {
                username,
                security_answer,
                new_password,
            } => {
                let ok = self
                    .db
                    .reset_password(&username, &security_answer, &new_password)
                    .await
                    .unwrap_or(false);
                send_to(
                    &self.sessions,
                    &conn_id,
                    ServerMessage::LoginResult {
                        success: ok,
                        account_id: None,
                        message: if ok {
                            "密码已重设, 请用新密码登录".into()
                        } else {
                            "重设失败: 用户不存在/未设密保/答案错误".into()
                        },
                    },
                )
                .await;
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
                        if let Some(z) = self.players.get(&character_id).map(|p| p.zone.clone()) {
                            self.broadcast_ground(&z).await;
                        }
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
                // 冲锋中位移由服务端推进, 玩家移动包忽略
                if self
                    .players
                    .values()
                    .any(|p| p.conn_id == conn_id && p.dash.is_some())
                {
                    return;
                }
                if let Some((cid, character_id, x, y)) = self.handle_move(&conn_id, direction) {
                    // 触发传送门 → 通知切区
                    self.send_enter_zone_only(&cid, &character_id, x, y).await;
                }
            }
            ClientMessage::Attack { target_id, .. } => {
                self.handle_attack(&conn_id, &target_id).await;
            }
            ClientMessage::AcceptQuest { quest_id } => {
                self.handle_accept_quest(&conn_id, &quest_id).await;
            }
            ClientMessage::CompleteQuest { quest_id } => {
                self.handle_complete_quest(&conn_id, &quest_id).await;
            }
            ClientMessage::AbandonQuest { quest_id } => {
                self.handle_abandon_quest(&conn_id, &quest_id).await;
            }
            ClientMessage::Equip { item_id, .. } => {
                self.handle_equip(&conn_id, &item_id).await;
            }
            ClientMessage::Unequip { slot } => {
                self.handle_unequip(&conn_id, &slot).await;
            }
            ClientMessage::TalkNpc { npc_id } => {
                self.handle_talk_npc(&conn_id, &npc_id).await;
            }
            ClientMessage::NpcOption { npc_id, page, idx } => {
                self.handle_npc_option(&conn_id, &npc_id, page, idx).await;
            }
            ClientMessage::BuyItem { npc_id, template } => {
                self.handle_buy_item(&conn_id, &npc_id, &template).await;
            }
            ClientMessage::SellItem { npc_id, item_id } => {
                self.handle_sell_item(&conn_id, &npc_id, &item_id).await;
            }
            ClientMessage::DropItem { item_id } => {
                self.handle_drop_item(&conn_id, &item_id).await;
            }
            ClientMessage::PickupItem { drop_id } => {
                self.handle_pickup_item(&conn_id, &drop_id).await;
            }
            ClientMessage::UseSkill {
                skill_id,
                target_id,
                ..
            } => {
                self.handle_use_skill(&conn_id, &skill_id, target_id).await;
            }
            ClientMessage::Chat {
                channel, content, ..
            } => {
                self.handle_chat(&conn_id, channel, content).await;
            }
            ClientMessage::TradeRequest { target_player_id } => {
                self.handle_trade_request(&conn_id, &target_player_id).await;
            }
            ClientMessage::TradeAccept => {
                self.handle_trade_accept(&conn_id).await;
            }
            ClientMessage::TradeDecline => {
                self.handle_trade_decline(&conn_id).await;
            }
            ClientMessage::TradePlaceItem { item_id } => {
                self.handle_trade_place(&conn_id, &item_id).await;
            }
            ClientMessage::TradeTakeItem { item_id } => {
                self.handle_trade_take(&conn_id, &item_id).await;
            }
            ClientMessage::TradeSetGold { gold } => {
                self.handle_trade_set_gold(&conn_id, gold).await;
            }
            ClientMessage::TradeConfirm => {
                self.handle_trade_confirm(&conn_id).await;
            }
            ClientMessage::TradeCancel => {
                if let Some(me) = self.char_by_conn(&conn_id) {
                    self.cancel_trade_of(&me, "对方取消了交易").await;
                }
            }
            ClientMessage::StoreItem { npc_id, item_id } => {
                self.handle_store_item(&conn_id, &npc_id, &item_id).await;
            }
            ClientMessage::StorageTake { npc_id, item_id } => {
                self.handle_storage_take(&conn_id, &npc_id, &item_id).await;
            }
            ClientMessage::SetPkMode { mode } => {
                self.handle_set_pk_mode(&conn_id, &mode).await;
            }
            other => {
                // M3 玩法消息占位
                warn!("暂未实现的消息: {other:?}");
            }
        }
    }

    /// 聊天: World 全服广播, Zone 同区广播; 其余频道未实现仅回显自己
    async fn handle_chat(&self, conn_id: &str, channel: protocol::ChatChannel, content: String) {
        let Some(sender) = self
            .players
            .values()
            .find(|p| p.conn_id == conn_id && p.connected)
        else {
            return;
        };
        let content: String = content.trim().chars().take(120).collect();
        if content.is_empty() {
            return;
        }
        let msg = ServerMessage::ChatMessage {
            channel,
            sender: sender.character.name.clone(),
            content,
            timestamp: now_ms(),
        };
        for target in self.chat_targets(conn_id, channel) {
            send_to(&self.sessions, &target, msg.clone()).await;
        }
    }

    /// 频道可见目标的 conn_id 列表 (含发送者)
    fn chat_targets(&self, conn_id: &str, channel: protocol::ChatChannel) -> Vec<String> {
        let Some(sender) = self
            .players
            .values()
            .find(|p| p.conn_id == conn_id && p.connected)
        else {
            return Vec::new();
        };
        self.players
            .values()
            .filter(|p| p.connected)
            .filter(|p| match channel {
                protocol::ChatChannel::World => true,
                protocol::ChatChannel::Zone => p.zone == sender.zone,
                _ => p.conn_id == conn_id,
            })
            .map(|p| p.conn_id.clone())
            .collect()
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
        let (level, exp, gold) = (c.level, c.exp, c.gold);
        let c_pk = c.pk_points;
        let (c_inventory, c_equipment, c_quests, c_skills, c_storage) = (
            c.inventory.clone(),
            c.equipment.clone(),
            c.quests.clone(),
            c.skills.clone(),
            c.storage.clone(),
        );
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
                mp: max_mp_for(level),
                max_mp: max_mp_for(level),
                level,
                exp,
                gold,
                last_attack: Instant::now() - PLAYER_ATTACK_CD,
                cooldowns: HashMap::new(),
                inventory: c_inventory,
                equipment: c_equipment,
                storage: c_storage,
                quests: c_quests,
                skills: c_skills,
                dash: None,
                pk_all: false,
                pk_points: c_pk,
                grey_until: None,
                dead_until: None,
                poison: None,
            },
        );
        if let Some(p) = self.players.get_mut(&character_id) {
            p.recalc();
            p.hp = p.max_hp;
        }
        info!("进入游戏: {character_id} {zone} @({x:.1},{y:.1})");
        self.send_enter_payload(conn_id, &character_id, x, y).await;
        if let Some(z) = self.players.get(&character_id).map(|p| p.zone.clone()) {
            self.broadcast_ground(&z).await;
        }
        self.send_player_status(&character_id).await;
        self.send_skill_list(&character_id).await;
        self.send_inventory(&character_id).await;
        self.send_quests(&character_id).await;
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
                mp: p.mp,
                max_mp: p.max_mp,
            },
        )
        .await;
    }

    /// 下发本职业技能表
    async fn send_skill_list(&self, character_id: &str) {
        let Some(p) = self.players.get(character_id) else {
            return;
        };
        let skills = skills_for(p.character.class)
            .iter()
            .map(|s| {
                let sp = p.skills.get(&s.id).copied().unwrap_or_default();
                protocol::SkillInfo {
                    id: s.id.to_string(),
                    name: s.name.to_string(),
                    mp_cost: s.mp,
                    cooldown_ms: s.cd().as_millis() as u64,
                    required_level: s.level,
                    range: s.range,
                    self_cast: s.self_cast,
                    level: sp.level,
                    max_level: s.max_level,
                    train: sp.train,
                    train_need: if sp.level >= s.max_level {
                        0
                    } else {
                        s.train_need(sp.level)
                    },
                    icon: s.icon,
                    anim: s.anim.clone(),
                    kind: match &s.kind {
                        SkillKind::Damage(_) => "damage",
                        SkillKind::Aoe { .. } => "aoe",
                        SkillKind::Heal => "heal",
                        SkillKind::Dot { .. } => "dot",
                        SkillKind::Charge { .. } => "charge",
                        SkillKind::Summon { .. } => "summon",
                        SkillKind::Tame { .. } => "tame",
                    }
                    .into(),
                }
            })
            .collect();
        send_to(
            &self.sessions,
            &p.conn_id,
            ServerMessage::SkillList { skills },
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
        let Some(p) = self.players.get(character_id) else {
            return;
        };
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
        let (zone_id, zone_name, minimap, bgm, safe_zones) = self
            .zones
            .get(&p.zone)
            .map(|z| {
                (
                    z.id.clone(),
                    z.name.clone(),
                    z.sidecar.minimap,
                    z.sidecar.bgm.clone(),
                    z.sidecar.safe_zones.clone(),
                )
            })
            .unwrap_or((p.zone.clone(), p.zone.clone(), None, None, Vec::new()));
        let zone_of_npc = zone_id.clone();
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::ZoneChanged {
                zone_id,
                zone_name,
                position: Position { x, y },
                minimap,
                bgm,
                safe_zones,
            },
        )
        .await;
        self.send_npc_list(conn_id, &zone_of_npc).await;
        let gold = self.players.get(character_id).map(|p| p.gold).unwrap_or(0);
        self.send_gold(conn_id, gold).await;
    }

    /// 移动校验：步长按时间窗限幅 → sim 同源判定（客户端预测调同一函数）。
    /// 踏中传送门时切区并返回 (conn, character, x, y) 供调用方发 ZoneChanged。
    fn handle_move(
        &mut self,
        conn_id: &str,
        direction: Position,
    ) -> Option<(String, String, f64, f64)> {
        if self
            .players
            .values()
            .any(|p| p.conn_id == conn_id && p.dead_until.is_some())
        {
            return None; // 死亡中不移动
        }
        // 先把要用的值抄出来, 释放对 players 的借用 —— 后面算碰撞要读整张表
        let (id_of, last_move, px0, py0, zone_id) = self
            .players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id)
            .map(|(id, p)| (id.clone(), p.last_move, p.x, p.y, p.zone.clone()))?;
        let now = Instant::now();
        let dt = now
            .duration_since(last_move)
            .min(MAX_STEP_WINDOW)
            .as_secs_f64();
        let (mut dx, mut dy) = (direction.x, direction.y);
        let len = (dx * dx + dy * dy).sqrt();
        let max = MAX_SPEED * dt;
        if len > max && len > 0.0 {
            let k = max / len;
            dx *= k;
            dy *= k;
        }
        // 实体碰撞: 别的玩家 / 活着的怪 / NPC 都挡路
        let blockers =
            Self::blockers_for(&self.players, &self.monsters, &zone_id, Some(id_of.clone()));
        let zone = self.zones.get(&zone_id)?;
        let (nx, ny) = sim::resolve_move(&zone.walk, (px0, py0), (dx, dy), BODY_RADIUS, &blockers);
        let p = self.players.get_mut(&id_of)?;
        p.last_move = now;
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
            id_of, p.zone
        );
        p.zone = to_zone;
        p.x = tx;
        p.y = ty;
        p.moving = false;
        Some((conn_id.to_string(), id_of, tx, ty))
    }

    fn char_by_conn(&self, conn_id: &str) -> Option<String> {
        self.players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id)
            .map(|(id, _)| id.clone())
    }

    /// 只发 ZoneChanged（切区通知）
    async fn send_enter_zone_only(&self, conn_id: &str, character_id: &str, x: f64, y: f64) {
        let Some(p) = self.players.get(character_id) else {
            return;
        };
        let (zone_id, zone_name, minimap, bgm, safe_zones) = self
            .zones
            .get(&p.zone)
            .map(|z| {
                (
                    z.id.clone(),
                    z.name.clone(),
                    z.sidecar.minimap,
                    z.sidecar.bgm.clone(),
                    z.sidecar.safe_zones.clone(),
                )
            })
            .unwrap_or((p.zone.clone(), p.zone.clone(), None, None, Vec::new()));
        let zone_of_npc = zone_id.clone();
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::ZoneChanged {
                zone_id,
                zone_name,
                position: Position { x, y },
                minimap,
                bgm,
                safe_zones,
            },
        )
        .await;
        self.send_npc_list(conn_id, &zone_of_npc).await;
    }

    async fn on_disconnect(&mut self, conn_id: &str) {
        self.conns.remove(conn_id);
        let mut dropped = Vec::new();
        for (id, p) in self.players.iter_mut() {
            if p.conn_id == conn_id && p.connected {
                p.connected = false;
                p.disconnected_at = Some(Instant::now());
                p.moving = false;
                dropped.push(id.clone());
            }
        }
        for id in dropped {
            self.cancel_trade_of(&id, "对方已离线, 交易取消").await;
        }
    }
}
