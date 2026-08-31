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
    /// 原始边车 (管理台回显/写回)
    pub sidecar: ZoneSidecar,
}

// ── 边车配置 (zones/<map>.json; 服务器与管理台共用) ──

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ZoneSidecar {
    pub name: Option<String>,
    pub spawn: Option<(f64, f64)>,
    /// 小地图帧号 (Data/mmap.Lib); None = 无小地图
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimap: Option<u16>,
    #[serde(default)]
    pub portals: Vec<PortalSidecar>,
    #[serde(default)]
    pub monsters: Vec<MonsterSidecar>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PortalSidecar {
    pub x: f64,
    pub y: f64,
    pub to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_y: Option<f64>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct DropSidecar {
    pub item: String,
    pub chance: f64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct MonsterSidecar {
    pub template: String,
    /// 客户端怪物图库号; 模板化后由怪物模板提供, 此列仅旧数据兜底
    #[serde(default)]
    pub image: u16,
    pub x: f64,
    pub y: f64,
    #[serde(default = "one")]
    pub count: u32,
    #[serde(default)]
    pub passive: bool,
    #[serde(default = "default_hp")]
    pub hp: i32,
    #[serde(default)]
    pub damage: i32,
    #[serde(default = "default_exp")]
    pub exp: u64,
    #[serde(default)]
    pub drops: Vec<DropSidecar>,
    #[serde(default = "default_radius")]
    pub radius: f64,
}

/// 怪物模板 (全局一次定义, 刷新点按 id 引用; 管理台「怪物设置」页)
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct MonsterDef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub image: u16,
    /// 库内外观基址 (一库多怪时该怪的起始帧; 0 = 库首)
    #[serde(default)]
    pub base: u32,
    #[serde(default = "default_hp")]
    pub hp: i32,
    #[serde(default)]
    pub damage: i32,
    #[serde(default = "default_exp")]
    pub exp: u64,
    #[serde(default)]
    pub passive: bool,
    #[serde(default)]
    pub drops: Vec<DropSidecar>,
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
const RESPAWN_TIME: Duration = Duration::from_secs(30);

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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillKind {
    /// 单体伤害 (攻击 × 倍率)
    Damage(f64),
    /// 以目标/自身为圆心的范围伤害
    Aoe { radius: f64, mult: f64 },
    /// 治疗自身
    Heal,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillDef {
    pub id: String,
    pub name: String,
    pub mp: i32,
    pub cd_ms: u64,
    pub level: u32,
    pub range: f64,
    pub self_cast: bool,
    pub kind: SkillKind,
    /// 修炼满级 (官设 3; 调成 4/5 可做高级特效玩法)
    #[serde(default = "default_max_level")]
    pub max_level: u32,
    /// 修炼基数: 从 L 升到 L+1 需施放 train_base × (L+1) 次
    #[serde(default = "default_train_base")]
    pub train_base: u32,
    /// 每修炼级的伤害/治疗加成比例 (0.3 = 每级 +30%)
    #[serde(default = "default_level_bonus")]
    pub level_bonus: f64,
}

fn default_max_level() -> u32 {
    3
}
fn default_train_base() -> u32 {
    30
}
fn default_level_bonus() -> f64 {
    0.3
}

impl SkillDef {
    fn cd(&self) -> Duration {
        Duration::from_millis(self.cd_ms)
    }
    /// 从 lvl 升到 lvl+1 所需熟练度
    fn train_need(&self, lvl: u32) -> u32 {
        self.train_base.max(1) * (lvl + 1)
    }
}

/// 单技能修炼进度 (随角色存档)
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
pub struct SkillProgress {
    pub level: u32,
    pub train: u32,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillsCfg {
    pub warrior: Vec<SkillDef>,
    pub mage: Vec<SkillDef>,
    pub taoist: Vec<SkillDef>,
}

/// 全部数据配置 (物品/技能/任务)。内置默认与 server/data/*.json 同源
#[derive(Clone)]
pub struct GameData {
    pub items: Vec<ItemDef>,
    pub skills: SkillsCfg,
    pub quests: Vec<QuestDef>,
    pub npcs: Vec<NpcDef>,
    pub bosses: Vec<BossDef>,
    pub monsters: Vec<MonsterDef>,
}

/// BOSS = 定点刷新 + 长重生 + 大属性 + 可选击杀公告的怪物
///
/// 与地图里的普通刷新点分开配: 那些是"一片区域里刷 N 只", BOSS 是"某个点
/// 上唯一的一只, 打掉要等很久"。运行时两者都落成 `Monster`, 走同一套 AI
/// 与生命周期, 只是重生间隔与公告不同。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct BossDef {
    pub id: String,
    pub name: String,
    /// 所在地图 (区域 id)
    pub map: String,
    pub x: f64,
    pub y: f64,
    /// Data/Monster/{image:03}.Lib
    pub image: u16,
    pub hp: i32,
    pub damage: i32,
    pub exp: u64,
    /// 重生间隔 (秒)
    #[serde(default = "default_boss_respawn")]
    pub respawn_secs: u64,
    /// 游荡半径 (0 = 钉在原地)
    #[serde(default)]
    pub roam: f64,
    /// 击杀后全服公告
    #[serde(default = "default_true")]
    pub announce: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub drops: Vec<DropEntry>,
}

fn default_boss_respawn() -> u64 {
    1800
}

/// 场景 NPC 配置 (存在/展示/对话; 商店见 docs/NPC_DESIGN.md P3)
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct NpcDef {
    pub id: String,
    pub name: String,
    /// 所在地图 (区域 id)
    pub map: String,
    pub x: f64,
    pub y: f64,
    /// Data/NPC/{image:02}.Lib
    pub image: u16,
    /// 交互类型 (talk/shop/quest/teleport)
    #[serde(default = "default_npc_kind")]
    pub kind: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 对话页; 空 = 点击只弹一句缺省招呼
    #[serde(default)]
    pub dialogs: Vec<NpcDialogPage>,
    /// 售货清单 (kind=shop 时有效)
    #[serde(default)]
    pub shop: Vec<ShopEntry>,
}

/// 商店一行: 卖什么 / 多少钱 / 还有几件
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ShopEntry {
    /// 物品模板 id
    pub item: String,
    /// 售价; 0 = 用物品基准价
    #[serde(default)]
    pub price: u32,
    /// 库存; -1 = 无限
    #[serde(default = "unlimited_stock")]
    pub stock: i32,
}

fn unlimited_stock() -> i32 {
    -1
}

/// 回收价占售价的比例 (经典传奇卖给 NPC 都要打折)
pub const SELL_RATE: f64 = 0.5;

/// 一页对话 = 一段文本 + 若干选项
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct NpcDialogPage {
    pub page: u32,
    pub text: String,
    #[serde(default)]
    pub options: Vec<NpcOptionDef>,
}

/// 选项动作: page(跳页, arg=页号) / quest_accept / quest_complete (arg=任务 id) / close
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct NpcOptionDef {
    pub label: String,
    #[serde(default = "default_option_action")]
    pub action: String,
    #[serde(default)]
    pub arg: String,
}

fn default_option_action() -> String {
    "close".into()
}

fn default_npc_kind() -> String {
    "talk".into()
}
fn default_true() -> bool {
    true
}

static DATA: std::sync::RwLock<Option<std::sync::Arc<GameData>>> = std::sync::RwLock::new(None);

impl GameData {
    fn builtin() -> Self {
        GameData {
            items: serde_json::from_str(include_str!("../data/items.json")).expect("内置 items"),
            skills: serde_json::from_str(include_str!("../data/skills.json")).expect("内置 skills"),
            quests: serde_json::from_str(include_str!("../data/quests.json")).expect("内置 quests"),
            npcs: serde_json::from_str(include_str!("../data/npcs.json")).expect("内置 npcs"),
            bosses: serde_json::from_str(include_str!("../data/bosses.json")).expect("内置 bosses"),
            monsters: serde_json::from_str(include_str!("../data/monsters.json"))
                .expect("内置 monsters"),
        }
    }

    /// 从 JSON 目录读取种子数据 (仅首次建库时用; 缺文件回退内置默认)
    pub fn seed_source(dir: &std::path::Path) -> Self {
        fn read<T: serde::de::DeserializeOwned>(p: std::path::PathBuf, what: &str) -> Option<T> {
            let text = std::fs::read_to_string(&p).ok()?;
            match serde_json::from_str(&text) {
                Ok(v) => {
                    info!("{what} 种子: {p:?}");
                    Some(v)
                }
                Err(e) => {
                    tracing::error!("{what} 种子解析失败 {p:?}: {e}, 使用内置默认");
                    None
                }
            }
        }
        let b = Self::builtin();
        GameData {
            items: read(dir.join("items.json"), "物品").unwrap_or(b.items),
            skills: read(dir.join("skills.json"), "技能").unwrap_or(b.skills),
            quests: read(dir.join("quests.json"), "任务").unwrap_or(b.quests),
            npcs: read(dir.join("npcs.json"), "NPC").unwrap_or(b.npcs),
            bosses: read(dir.join("bosses.json"), "BOSS").unwrap_or(b.bosses),
            monsters: read(dir.join("monsters.json"), "怪物").unwrap_or(b.monsters),
        }
    }
}

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

impl GameData {
    /// 业务校验: id 唯一 / 槽位合法 / 任务前置存在。错误列表为空即通过
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        let slots = ["weapon", "armor", "helmet", "necklace", "ring"];
        let mut tpl = std::collections::HashSet::new();
        for d in &self.items {
            if !tpl.insert(&d.template) {
                errs.push(format!("物品模板重复: {}", d.template));
            }
            if !slots.contains(&d.slot.as_str()) {
                errs.push(format!("物品 {} 槽位非法: {}", d.template, d.slot));
            }
        }
        let mut sid = std::collections::HashSet::new();
        for s in self
            .skills
            .warrior
            .iter()
            .chain(&self.skills.mage)
            .chain(&self.skills.taoist)
        {
            if !sid.insert(&s.id) {
                errs.push(format!("技能 id 重复: {}", s.id));
            }
        }
        let qids: std::collections::HashSet<&str> =
            self.quests.iter().map(|q| q.id.as_str()).collect();
        let mut qseen = std::collections::HashSet::new();
        for q in &self.quests {
            if !qseen.insert(&q.id) {
                errs.push(format!("任务 id 重复: {}", q.id));
            }
            if let Some(pr) = q.prereq.as_deref() {
                if !qids.contains(pr) {
                    errs.push(format!("任务 {} 前置不存在: {pr}", q.id));
                }
            }
            for rw in &q.rewards {
                if !self.items.iter().any(|i| i.template == rw.item) {
                    errs.push(format!("任务 {} 奖励引用的物品不存在: {}", q.id, rw.item));
                }
                if rw.count == 0 {
                    errs.push(format!("任务 {} 奖励 {} 的数量不能为 0", q.id, rw.item));
                }
            }
            // 一次发不下的奖励等于永远交不了任务
            let total: u32 = q.rewards.iter().map(|r| r.count).sum();
            if total as usize > MAX_INVENTORY {
                errs.push(format!(
                    "任务 {} 的物品奖励共 {total} 件, 超过背包上限 {MAX_INVENTORY}",
                    q.id
                ));
            }
        }
        let mut mid = std::collections::HashSet::new();
        for m in &self.monsters {
            if !mid.insert(&m.id) {
                errs.push(format!("怪物模板 id 重复: {}", m.id));
            }
            if m.name.trim().is_empty() {
                errs.push(format!("怪物 {} 名称为空", m.id));
            }
            if m.hp <= 0 {
                errs.push(format!("怪物 {} 的 HP 必须大于 0", m.id));
            }
            for d in &m.drops {
                if !self.items.iter().any(|i| i.template == d.item) {
                    errs.push(format!("怪物 {} 掉落引用的物品不存在: {}", m.id, d.item));
                }
                if !(0.0..=1.0).contains(&d.chance) {
                    errs.push(format!("怪物 {} 掉落 {} 的概率应在 0~1 之间", m.id, d.item));
                }
            }
        }
        let mut bid = std::collections::HashSet::new();
        for b in &self.bosses {
            if !bid.insert(&b.id) {
                errs.push(format!("BOSS id 重复: {}", b.id));
            }
            if b.name.trim().is_empty() {
                errs.push(format!("BOSS {} 名称为空", b.id));
            }
            if b.hp <= 0 {
                errs.push(format!("BOSS {} 的 HP 必须大于 0", b.id));
            }
            if b.respawn_secs == 0 {
                errs.push(format!("BOSS {} 的重生间隔必须大于 0 秒", b.id));
            }
            for d in &b.drops {
                if !self.items.iter().any(|i| i.template == d.item) {
                    errs.push(format!("BOSS {} 掉落引用的物品不存在: {}", b.id, d.item));
                }
                if !(0.0..=1.0).contains(&d.chance) {
                    errs.push(format!(
                        "BOSS {} 掉落 {} 的概率应在 0~1 之间, 收到 {}",
                        b.id, d.item, d.chance
                    ));
                }
            }
        }
        let mut nid = std::collections::HashSet::new();
        for n in &self.npcs {
            if !nid.insert(&n.id) {
                errs.push(format!("NPC id 重复: {}", n.id));
            }
            if n.name.trim().is_empty() {
                errs.push(format!("NPC {} 名称为空", n.id));
            }
            // 商店: 引用的物品模板要存在, 且不能重复上架
            let mut sseen = std::collections::HashSet::new();
            for e in &n.shop {
                if !self.items.iter().any(|i| i.template == e.item) {
                    errs.push(format!("NPC {} 售货清单引用的物品不存在: {}", n.id, e.item));
                }
                if !sseen.insert(&e.item) {
                    errs.push(format!("NPC {} 售货清单重复上架: {}", n.id, e.item));
                }
                if e.stock < -1 {
                    errs.push(format!(
                        "NPC {} 售货 {} 的库存只能是 -1(无限) 或 ≥0, 收到 {}",
                        n.id, e.item, e.stock
                    ));
                }
            }
            if n.kind != "shop" && !n.shop.is_empty() {
                errs.push(format!("NPC {} 配了售货清单, 但类型不是「商店」", n.id));
            }
            // 反向的坑: 配了货却没有任何入口 —— 玩家点它只会看到一段普通对话。
            // (没配对话页时服务端会给缺省进店入口, 所以只在配了对话页时才算错)
            if n.kind == "shop"
                && !n.shop.is_empty()
                && !n.dialogs.is_empty()
                && !n
                    .dialogs
                    .iter()
                    .any(|d| d.options.iter().any(|o| o.action == "shop"))
            {
                errs.push(format!(
                    "NPC {} 是商店且已上架 {} 件, 但对话里没有任何「打开商店」选项 —— 玩家进不了店",
                    n.id,
                    n.shop.len()
                ));
            }
            // 对话页: 页号唯一, 跳页目标存在, 任务动作引用的任务存在
            let pages: std::collections::HashSet<u32> = n.dialogs.iter().map(|d| d.page).collect();
            if pages.len() != n.dialogs.len() {
                errs.push(format!("NPC {} 对话页号重复", n.id));
            }
            for d in &n.dialogs {
                for o in &d.options {
                    match o.action.as_str() {
                        "page" => match o.arg.parse::<u32>() {
                            Ok(t) if pages.contains(&t) => {}
                            _ => errs.push(format!(
                                "NPC {} 第 {} 页选项「{}」跳转到不存在的页: {}",
                                n.id, d.page, o.label, o.arg
                            )),
                        },
                        "quest_accept" | "quest_complete" => {
                            if !qids.contains(o.arg.as_str()) {
                                errs.push(format!(
                                    "NPC {} 第 {} 页选项「{}」引用的任务不存在: {}",
                                    n.id, d.page, o.label, o.arg
                                ));
                            }
                        }
                        "shop" => {
                            if n.kind != "shop" {
                                errs.push(format!(
                                    "NPC {} 第 {} 页有「商店」选项, 但类型不是「商店」",
                                    n.id, d.page
                                ));
                            } else if n.shop.is_empty() {
                                errs.push(format!("NPC {} 有「商店」选项但售货清单是空的", n.id));
                            }
                        }
                        // teleport 的目标地图/落点要走格数据, 见 check_npc_placement
                        "close" | "teleport" => {}
                        other => errs.push(format!(
                            "NPC {} 第 {} 页选项「{}」动作未知: {other}",
                            n.id, d.page, o.label
                        )),
                    }
                }
            }
        }
        errs
    }
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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ItemDef {
    pub template: String,
    pub name: String,
    pub slot: String,
    pub attack: i32,
    /// 魔法攻击 (法师系)
    #[serde(default)]
    pub magic: i32,
    /// 道术攻击 (道士系)
    #[serde(default)]
    pub spirit: i32,
    pub defense: i32,
    pub hp: i32,
    /// Items.Lib 图标帧号
    pub image: u16,
    /// 外观库号 (weapon → CWeapon, armor → CArmour)
    pub shape: u16,
    /// 基准价 (金币)。商店未单独定价时按它卖; 回收价 = 基准价 × SELL_RATE
    #[serde(default)]
    pub price: u32,
}

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
    })
}

/// 背包容量上限 (与客户端 panels::BAG_SLOTS 一致)
const MAX_INVENTORY: usize = 50;
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

/// 掉落表条目 (边车配置)
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct DropEntry {
    pub item: String,
    pub chance: f64,
}

// ─────────── 任务 (M3.5; 三链新手任务, 迁自旧服务器) ───────────

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct QuestDef {
    pub id: String,
    pub name: String,
    /// (怪物模板, 数量)
    pub objectives: Vec<(String, u32)>,
    pub exp_reward: u64,
    /// 金币奖励
    #[serde(default)]
    pub gold_reward: u64,
    /// 物品奖励
    #[serde(default)]
    pub rewards: Vec<QuestReward>,
    pub prereq: Option<String>,
}

/// 任务的物品奖励一项
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct QuestReward {
    pub item: String,
    #[serde(default = "one_u32")]
    pub count: u32,
}

fn one_u32() -> u32 {
    1
}

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

/// 按刷新点物化一个区域的怪物 (出生位置吸附可走格)
/// `avoid` 是要让开的位置 (在场玩家); 开服时为空
fn materialize_monsters(zone: &Zone, rng: &mut u64, avoid: &[(f64, f64)]) -> Vec<Monster> {
    let mut next = |limit: f64| {
        *rng ^= *rng << 13;
        *rng ^= *rng >> 7;
        *rng ^= *rng << 17;
        (*rng >> 11) as f64 / (1u64 << 53) as f64 * limit
    };
    let now = Instant::now();
    let mut out = Vec::new();
    let d = data();
    for (si, sp) in zone.monster_spawns.iter().enumerate() {
        // 数值以怪物模板为准; 旧边车的内联字段仅在模板缺失时兜底
        let def = d.monsters.iter().find(|m| m.id == sp.template);
        let image = def.map(|m| m.image).unwrap_or(sp.image);
        let image_base = def.map(|m| m.base).unwrap_or(0);
        let (hp, damage, exp) = def
            .map(|m| (m.hp, m.damage, m.exp))
            .unwrap_or((sp.hp, sp.damage, sp.exp));
        let passive = def.map(|m| m.passive).unwrap_or(sp.passive);
        let drops: Vec<DropEntry> = def
            .map(|m| {
                m.drops
                    .iter()
                    .map(|d| DropEntry {
                        item: d.item.clone(),
                        chance: d.chance,
                    })
                    .collect()
            })
            .unwrap_or_else(|| sp.drops.clone());
        for i in 0..sp.count {
            let want = (
                sp.x + next(sp.radius * 2.0) - sp.radius,
                sp.y + next(sp.radius * 2.0) - sp.radius,
            );
            let (x, y) = nearest_free(&zone.walk, want.0, want.1, avoid);
            out.push(Monster {
                id: format!("mon_{}_{}_{}_{}", zone.id, image, si, i),
                template: sp.template.clone(),
                name: def.map(|m| m.name.clone()).unwrap_or_default(),
                boss: false,
                respawn: RESPAWN_TIME,
                announce: false,
                image,
                image_base,
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
                passive,
                hp,
                max_hp: hp,
                damage,
                exp,
                drops: drops.clone(),
                dying_until: None,
                respawn_at: None,
                removed_sent: false,
            });
        }
    }
    out
}

/// 把本区的 BOSS 配置落成 Monster (与普通刷新点走同一套 AI/生命周期)
fn materialize_bosses(zone: &Zone, avoid: &[(f64, f64)]) -> Vec<Monster> {
    let now = Instant::now();
    data()
        .bosses
        .iter()
        .filter(|b| b.enabled && b.map == zone.id)
        .map(|b| {
            let (x, y) = nearest_free(&zone.walk, b.x, b.y, avoid);
            Monster {
                id: format!("boss_{}_{}", zone.id, b.id),
                image_base: 0,
                template: b.id.clone(),
                name: b.name.clone(),
                boss: true,
                respawn: Duration::from_secs(b.respawn_secs.max(1)),
                announce: b.announce,
                image: b.image,
                zone: zone.id.clone(),
                home: (x, y),
                roam: b.roam,
                x,
                y,
                dir: 4,
                target: None,
                chasing: false,
                attack_until: None,
                pending_hit: None,
                next_attack: now,
                next_decide: now,
                passive: false,
                hp: b.hp,
                max_hp: b.hp,
                damage: b.damage,
                exp: b.exp,
                drops: b.drops.clone(),
                dying_until: None,
                respawn_at: None,
                removed_sent: false,
            }
        })
        .collect()
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
    quests: HashMap<String, QuestProgress>,
    /// 技能 id → 修炼进度
    skills: HashMap<String, SkillProgress>,
}

impl PlayerState {
    fn equip_attack(&self) -> i32 {
        self.equipment.values().map(|i| i.attack).sum()
    }
    fn equip_magic(&self) -> i32 {
        self.equipment.values().map(|i| i.magic).sum()
    }
    fn equip_spirit(&self) -> i32 {
        self.equipment.values().map(|i| i.spirit).sum()
    }
    fn equip_defense(&self) -> i32 {
        self.equipment.values().map(|i| i.defense).sum()
    }
    fn equip_hp(&self) -> i32 {
        self.equipment.values().map(|i| i.hp).sum()
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
    last_save: Instant,
    last_regen: Instant,
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
            last_save: Instant::now(),
            last_regen: Instant::now(),
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
                _ = tick.tick() => self.tick().await,
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
        // 热应用: 替换区域 + 重建该区怪物 (先广播 removed)
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
        Ok(())
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

    /// 某区域的启用 NPC (下发客户端)
    fn npcs_of_zone(zone: &str) -> Vec<protocol::NpcInfo> {
        data()
            .npcs
            .iter()
            .filter(|n| n.enabled && n.map == zone)
            .map(|n| protocol::NpcInfo {
                id: n.id.clone(),
                name: n.name.clone(),
                x: n.x,
                y: n.y,
                image: n.image,
            })
            .collect()
    }

    /// 向某连接下发其所在区的 NPC 列表
    async fn send_npc_list(&self, conn_id: &str, zone: &str) {
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::NpcList {
                npcs: Self::npcs_of_zone(zone),
            },
        )
        .await;
    }

    /// 取某 NPC 的某一页; page=0 表示「第一页」(取页号最小的一页)
    /// 没配对话页时的缺省页
    ///
    /// 商店 NPC 直接带一个进店入口 —— 否则"类型选了商店、货也上架了"却依然
    /// 只能看到一句招呼, 没有任何办法把店打开。
    fn default_page(npc: &NpcDef) -> NpcDialogPage {
        let shop = npc.kind == "shop" && !npc.shop.is_empty();
        let mut options = Vec::new();
        if shop {
            options.push(NpcOptionDef {
                label: "我看看货".into(),
                action: "shop".into(),
                arg: String::new(),
            });
        }
        options.push(NpcOptionDef {
            label: "告辞".into(),
            action: "close".into(),
            arg: String::new(),
        });
        NpcDialogPage {
            page: 1,
            text: if shop {
                format!("{}：看看要点什么？", npc.name)
            } else {
                format!("{}：勇士，愿玛法大陆保佑你。", npc.name)
            },
            options,
        }
    }

    /// 取某页; page=0 表示第一页。没配对话页时回缺省页, 让下游只有一条路径
    fn npc_page(npc: &NpcDef, page: u32) -> Option<NpcDialogPage> {
        if npc.dialogs.is_empty() {
            return matches!(page, 0 | 1).then(|| Self::default_page(npc));
        }
        if page == 0 {
            npc.dialogs.iter().min_by_key(|d| d.page).cloned()
        } else {
            npc.dialogs.iter().find(|d| d.page == page).cloned()
        }
    }

    /// 下发一页对话; 该页不存在则结束对话
    async fn send_npc_page(&self, conn_id: &str, npc: &NpcDef, page: u32) {
        let Some(d) = Self::npc_page(npc, page) else {
            send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
            return;
        };
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::NpcDialog {
                npc_id: npc.id.clone(),
                name: npc.name.clone(),
                page: d.page,
                text: d.text.clone(),
                options: d
                    .options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| protocol::NpcDialogOption {
                        idx: i as u32,
                        label: o.label.clone(),
                    })
                    .collect(),
            },
        )
        .await;
    }

    /// 取玩家同区且在交互距离内的 NPC (越权/隔图对话在此挡掉)
    fn npc_in_reach(&self, conn_id: &str, npc_id: &str) -> Option<NpcDef> {
        let p = self
            .players
            .values()
            .find(|p| p.conn_id == conn_id && p.connected)?;
        let d = data();
        let npc = d
            .npcs
            .iter()
            .find(|n| n.enabled && n.id == npc_id && n.map == p.zone)?;
        let dist = ((npc.x - p.x).powi(2) + (npc.y - p.y).powi(2)).sqrt();
        (dist <= sim::NPC_TALK_RANGE).then(|| npc.clone())
    }

    /// 点击 NPC: 发第一页; 没配对话就用缺省招呼
    async fn handle_talk_npc(&mut self, conn_id: &str, npc_id: &str) {
        let Some(npc) = self.npc_in_reach(conn_id, npc_id) else {
            return;
        };
        self.send_npc_page(conn_id, &npc, 0).await;
    }

    /// NPC 传送: arg = "地图:x:y"; 落点不可站立时吸附到最近可走格
    async fn teleport_by_npc(&mut self, conn_id: &str, arg: &str) {
        let Some((map, x, y)) = parse_teleport(arg) else {
            return;
        };
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        let Some(target) = self.zones.get(&map) else {
            return;
        };
        let (tx, ty) = nearest_walkable(&target.walk, x, y);
        {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            info!("NPC 传送: {} {} → {map} ({tx:.1},{ty:.1})", char_id, p.zone);
            p.zone = map;
            p.x = tx;
            p.y = ty;
            p.moving = false;
        }
        self.send_enter_zone_only(conn_id, &char_id, tx, ty).await;
    }

    /// 商店某件货的实际售价 (商店未单独定价则用物品基准价)
    fn shop_price(e: &ShopEntry) -> u32 {
        if e.price > 0 {
            e.price
        } else {
            item_def(&e.item).map(|d| d.price).unwrap_or(0)
        }
    }

    /// 打开商店: 把货架下发给客户端
    async fn send_npc_shop(&self, conn_id: &str, npc: &NpcDef) {
        let items: Vec<protocol::ShopItemInfo> = npc
            .shop
            .iter()
            .filter(|e| e.stock != 0) // 卖光的不上架
            .filter_map(|e| {
                let d = item_def(&e.item)?;
                Some(protocol::ShopItemInfo {
                    template: d.template,
                    name: d.name,
                    image: d.image,
                    price: Self::shop_price(e),
                    stock: e.stock,
                    attack: d.attack,
                    magic: d.magic,
                    spirit: d.spirit,
                    defense: d.defense,
                    hp: d.hp,
                    slot: d.slot,
                })
            })
            .collect();
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::NpcShop {
                npc_id: npc.id.clone(),
                name: npc.name.clone(),
                items,
                sell_rate: SELL_RATE,
            },
        )
        .await;
    }

    /// 某区在线玩家的位置 (刷怪时让开用)
    fn player_positions(players: &HashMap<String, PlayerState>, zone: &str) -> Vec<(f64, f64)> {
        players
            .values()
            .filter(|p| p.connected && p.zone == zone)
            .map(|p| (p.x, p.y))
            .collect()
    }

    /// 某区里会挡路的实体圆心 (可排除自己)
    ///
    /// NPC 是站着不动的摊主一类, 也该挡; 尸体 (死亡动画/等重生) 不挡。
    /// 怪之间不互相阻挡 —— 刷新点是按半径随机撒的, 互挡会让整窝当场卡死。
    fn blockers_for(
        players: &HashMap<String, PlayerState>,
        monsters: &[Monster],
        zone: &str,
        except: Option<String>,
    ) -> Vec<(f64, f64)> {
        let mut out: Vec<(f64, f64)> = players
            .iter()
            .filter(|(id, p)| {
                p.connected && p.zone == zone && except.as_deref() != Some(id.as_str())
            })
            .map(|(_, p)| (p.x, p.y))
            .collect();
        out.extend(
            monsters
                .iter()
                .filter(|m| m.zone == zone && m.alive())
                .map(|m| (m.x, m.y)),
        );
        out.extend(
            data()
                .npcs
                .iter()
                .filter(|n| n.enabled && n.map == zone)
                .map(|n| (n.x, n.y)),
        );
        out
    }

    /// 全服公告 (BOSS 击杀等)
    async fn broadcast_all(&self, msg: &str) {
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
                message: msg.into(),
                notification_type: "system".into(),
            },
        )
        .await;
    }

    async fn notify(&self, conn_id: &str, msg: &str) {
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::Notification {
                message: msg.into(),
                notification_type: "system".into(),
            },
        )
        .await;
    }

    async fn send_gold(&self, conn_id: &str, gold: u64) {
        send_to(&self.sessions, conn_id, ServerMessage::GoldChanged { gold }).await;
    }

    /// 买入一件: 距离/货架/库存/金币/背包位 逐项校验
    async fn handle_buy_item(&mut self, conn_id: &str, npc_id: &str, template: &str) {
        let Some(npc) = self.npc_in_reach(conn_id, npc_id) else {
            return;
        };
        let Some(entry) = npc.shop.iter().find(|e| e.item == template) else {
            return; // 不卖这个 —— 伪造的请求, 静默丢弃
        };
        if entry.stock == 0 {
            self.notify(conn_id, "这件货已经卖完了").await;
            return;
        }
        let price = Self::shop_price(entry);
        let Some(item) = make_item(template) else {
            return;
        };
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        // 先判定再动状态: notify 要借 &self, 不能在持有 players 可变借用时调
        enum Deny {
            BagFull,
            NoGold,
        }
        let deny = {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            if p.inventory.len() >= MAX_INVENTORY {
                Some(Deny::BagFull)
            } else if p.gold < price as u64 {
                Some(Deny::NoGold)
            } else {
                p.gold -= price as u64;
                p.inventory.push(item);
                None
            }
        };
        match deny {
            Some(Deny::BagFull) => return self.notify(conn_id, "背包已满").await,
            Some(Deny::NoGold) => return self.notify(conn_id, "金币不足").await,
            None => {}
        }
        // 有限库存要扣, 并把新货架推回去
        if entry.stock > 0 {
            let mut d = (*data()).clone();
            if let Some(e) = d
                .npcs
                .iter_mut()
                .find(|n| n.id == npc_id)
                .and_then(|n| n.shop.iter_mut().find(|e| e.item == template))
            {
                e.stock -= 1;
            }
            set_data(d);
        }
        let gold = self.players.get(&char_id).map(|p| p.gold).unwrap_or(0);
        let _ = self.db.save_gold(&char_id, gold).await;
        self.send_gold(conn_id, gold).await;
        self.send_inventory(&char_id).await;
        if let Some(npc) = self.npc_in_reach(conn_id, npc_id) {
            self.send_npc_shop(conn_id, &npc).await; // 库存变了, 刷新货架
        }
    }

    /// 卖出一件: 按售价 × SELL_RATE 回收
    async fn handle_sell_item(&mut self, conn_id: &str, npc_id: &str, item_id: &str) {
        if self.npc_in_reach(conn_id, npc_id).is_none() {
            return;
        }
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        let sold = {
            let Some(p) = self.players.get_mut(&char_id) else {
                return;
            };
            let Some(idx) = p.inventory.iter().position(|i| i.id == item_id) else {
                return;
            };
            let item = p.inventory.remove(idx);
            let base = item_def(&item.template).map(|d| d.price).unwrap_or(0);
            let paid = ((base as f64) * SELL_RATE).floor() as u64;
            p.gold = p.gold.saturating_add(paid);
            (item.name, paid, p.gold)
        };
        let (name, paid, gold) = sold;
        let _ = self.db.save_gold(&char_id, gold).await;
        self.send_gold(conn_id, gold).await;
        self.send_inventory(&char_id).await;
        self.notify(conn_id, &format!("卖出 {name}, 得 {paid} 金币"))
            .await;
    }

    /// 选项动作分发
    async fn handle_npc_option(&mut self, conn_id: &str, npc_id: &str, page: u32, idx: u32) {
        let Some(npc) = self.npc_in_reach(conn_id, npc_id) else {
            // 走远了 / NPC 被禁用 — 明确收场, 免得客户端挂着个死对话框
            send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
            return;
        };
        let Some(opt) =
            Self::npc_page(&npc, page).and_then(|d| d.options.get(idx as usize).cloned())
        else {
            send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
            return;
        };
        match opt.action.as_str() {
            "page" => {
                let next = opt.arg.parse::<u32>().unwrap_or(0);
                self.send_npc_page(conn_id, &npc, next).await;
            }
            "quest_accept" => {
                self.handle_accept_quest(conn_id, &opt.arg).await;
                send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
            }
            "quest_complete" => {
                self.handle_complete_quest(conn_id, &opt.arg).await;
                send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
            }
            "teleport" => {
                send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
                self.teleport_by_npc(conn_id, &opt.arg).await;
            }
            "shop" => {
                send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await;
                self.send_npc_shop(conn_id, &npc).await;
            }
            _ => send_to(&self.sessions, conn_id, ServerMessage::NpcDialogEnd).await,
        }
    }

    /// NPC 落位校验: 地图已接入 / 坐标可走 / 传送目标合法
    ///
    /// 与 `GameData::validate` 分开, 因为走格与区域表只在游戏循环里有。
    fn check_npc_placement(&self, npcs: &[NpcDef]) -> Vec<String> {
        let mut errs = Vec::new();
        for n in npcs {
            let Some(z) = self.zones.get(&n.map) else {
                errs.push(format!("NPC {} 的地图未接入为区域: {}", n.id, n.map));
                continue;
            };
            if !z.walk.is_walkable_circle(n.x, n.y, BODY_RADIUS) {
                errs.push(format!(
                    "NPC {} 的坐标不可站立 ({:.1},{:.1})",
                    n.id, n.x, n.y
                ));
            }
            for d in &n.dialogs {
                for o in &d.options {
                    if o.action != "teleport" {
                        continue;
                    }
                    match parse_teleport(&o.arg) {
                        Some((map, x, y)) => match self.zones.get(&map) {
                            None => errs.push(format!(
                                "NPC {} 第 {} 页选项「{}」传送到未接入的地图: {map}",
                                n.id, d.page, o.label
                            )),
                            Some(t) => {
                                if !t.walk.is_walkable_circle(x, y, BODY_RADIUS) {
                                    errs.push(format!(
                                        "NPC {} 第 {} 页选项「{}」传送落点不可站立: {map} ({x:.1},{y:.1})",
                                        n.id, d.page, o.label
                                    ));
                                }
                            }
                        },
                        None => errs.push(format!(
                            "NPC {} 第 {} 页选项「{}」传送参数应为 地图:x:y, 收到: {}",
                            n.id, d.page, o.label, o.arg
                        )),
                    }
                }
            }
        }
        errs
    }

    /// 配置改动后重刷 BOSS: 撤掉旧的, 按新配置重新落地
    ///
    /// 代价是在场的 BOSS 会被重置 (满血回原位), 但配置改了本来就该以新配置
    /// 为准; 普通刷新点不受影响。
    /// 怪物模板配置变更后重刷所有普通刷新点怪 (BOSS 由 refresh_bosses 管)
    async fn refresh_spawns(&mut self) {
        let removed: Vec<protocol::EntityUpdate> = self
            .monsters
            .iter()
            .filter(|m| !m.boss)
            .map(|m| protocol::EntityUpdate {
                id: m.id.clone(),
                position: None,
                hp: None,
                animation: None,
                dir: None,
                removed: Some(true),
                armour: None,
                weapon: None,
                image: None,
                image_base: None,
            })
            .collect();
        self.monsters.retain(|m| m.boss);
        let mut rng = self.rng | 1;
        for zone in self.zones.values() {
            let here = Self::player_positions(&self.players, &zone.id);
            let fresh = materialize_monsters(zone, &mut rng, &here);
            self.monsters.extend(fresh);
        }
        if !removed.is_empty() {
            let conns: Vec<String> = self
                .players
                .values()
                .filter(|p| p.connected)
                .map(|p| p.conn_id.clone())
                .collect();
            broadcast_to(
                &self.sessions,
                &conns,
                ServerMessage::StateUpdate {
                    entities: removed,
                    timestamp: now_ms(),
                },
            )
            .await;
        }
    }

    async fn refresh_bosses(&mut self) {
        let removed: Vec<protocol::EntityUpdate> = self
            .monsters
            .iter()
            .filter(|m| m.boss)
            .map(|m| protocol::EntityUpdate {
                id: m.id.clone(),
                position: None,
                hp: None,
                animation: None,
                dir: None,
                removed: Some(true),
                armour: None,
                weapon: None,
                image: None,
                image_base: None,
            })
            .collect();
        self.monsters.retain(|m| !m.boss);
        for zone in self.zones.values() {
            let here = Self::player_positions(&self.players, &zone.id);
            let fresh = materialize_bosses(zone, &here);
            self.monsters.extend(fresh);
        }
        if !removed.is_empty() {
            let conns: Vec<String> = self
                .players
                .values()
                .filter(|p| p.connected)
                .map(|p| p.conn_id.clone())
                .collect();
            broadcast_to(
                &self.sessions,
                &conns,
                ServerMessage::StateUpdate {
                    entities: removed,
                    timestamp: now_ms(),
                },
            )
            .await;
        }
    }

    /// 任务目标校验: 击杀目标必须是某区的刷新点模板或某个 BOSS
    ///
    /// 目标写错不会报错, 只会让任务永远推不动 —— 与商店没有入口是同一类
    /// 静默失败, 所以在保存时拦下。刷新点在区域表里, 故与走格校验放一起。
    fn check_quest_targets(&self, quests: &[QuestDef]) -> Vec<String> {
        let d = data();
        let mut known: std::collections::HashSet<&str> = self
            .zones
            .values()
            .flat_map(|z| z.monster_spawns.iter().map(|s| s.template.as_str()))
            .collect();
        known.extend(d.bosses.iter().map(|b| b.id.as_str()));
        let mut errs = Vec::new();
        for q in quests {
            for (target, need) in &q.objectives {
                if !known.contains(target.as_str()) {
                    errs.push(format!(
                        "任务 {} 的击杀目标不存在于任何刷新点或 BOSS: {target}",
                        q.id
                    ));
                }
                if *need == 0 {
                    errs.push(format!("任务 {} 的目标 {target} 数量不能为 0", q.id));
                }
            }
        }
        errs
    }

    /// BOSS 落位校验: 地图已接入 / 坐标可走
    fn check_boss_placement(&self, bosses: &[BossDef]) -> Vec<String> {
        let mut errs = Vec::new();
        for b in bosses {
            let Some(z) = self.zones.get(&b.map) else {
                errs.push(format!("BOSS {} 的地图未接入为区域: {}", b.id, b.map));
                continue;
            };
            if !z.walk.is_walkable_circle(b.x, b.y, BODY_RADIUS) {
                errs.push(format!(
                    "BOSS {} 的坐标不可站立 ({:.1},{:.1})",
                    b.id, b.x, b.y
                ));
            }
        }
        errs
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
        let (c_inventory, c_equipment, c_quests, c_skills) = (
            c.inventory.clone(),
            c.equipment.clone(),
            c.quests.clone(),
            c.skills.clone(),
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
                quests: c_quests,
                skills: c_skills,
            },
        );
        self.players.get_mut(&character_id).unwrap().recalc();
        {
            let p = self.players.get_mut(&character_id).unwrap();
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
        let (zone_id, zone_name, minimap) = self
            .zones
            .get(&p.zone)
            .map(|z| (z.id.clone(), z.name.clone(), z.sidecar.minimap))
            .unwrap_or((p.zone.clone(), p.zone.clone(), None));
        let zone_of_npc = zone_id.clone();
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::ZoneChanged {
                zone_id,
                zone_name,
                position: Position { x, y },
                minimap,
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
        let mon_id = m.id.clone();
        let dmg = attack_for(level) + self.players[&char_id].equip_attack();
        self.hit_monster(&char_id, &mon_id, dmg).await;
    }

    /// 对怪结算一次伤害: 扣血/飘字广播/击杀 → 尸体+经验。返回是否击杀。
    async fn hit_monster(&mut self, char_id: &str, mon_id: &str, dmg: i32) -> bool {
        let now = Instant::now();
        let Some(m) = self
            .monsters
            .iter_mut()
            .find(|m| m.id == mon_id && m.alive())
        else {
            return false;
        };
        m.hp -= dmg;
        let (zone, killed, exp_gain) = (m.zone.clone(), m.hp <= 0, m.exp);
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
                target_id: mon_id.to_string(),
                amount: dmg,
                is_critical: false,
            },
        )
        .await;
        if killed {
            let (template, boss_name) = self
                .monsters
                .iter()
                .find(|m| m.id == mon_id)
                .map(|m| {
                    (
                        m.template.clone(),
                        (m.boss && m.announce).then(|| m.name.clone()),
                    )
                })
                .unwrap_or_default();
            self.award_exp(char_id, exp_gain).await;
            self.roll_drops(char_id, mon_id).await;
            self.progress_quests(char_id, &template).await;
            if let Some(name) = boss_name {
                let who = self
                    .players
                    .get(char_id)
                    .map(|p| p.character.name.clone())
                    .unwrap_or_default();
                self.broadcast_all(&format!("勇士 {who} 击杀了 {name}！"))
                    .await;
            }
        }
        killed
    }

    /// 击杀怪物 → 推进进行中任务的对应目标
    async fn progress_quests(&mut self, char_id: &str, template: &str) {
        let Some(p) = self.players.get_mut(char_id) else {
            return;
        };
        let mut changed = false;
        for (qid, prog) in p.quests.iter_mut() {
            if prog.state != 1 {
                continue;
            }
            let Some(def) = quest_def(qid) else { continue };
            for (i, (target, required)) in def.objectives.iter().enumerate() {
                if *target == template && prog.counts[i] < *required {
                    prog.counts[i] += 1;
                    changed = true;
                }
            }
        }
        if changed {
            self.send_quests(char_id).await;
        }
    }

    /// 推送任务面板全量态 (可接/进行中/已完成)
    async fn send_quests(&self, char_id: &str) {
        let Some(p) = self.players.get(char_id) else {
            return;
        };
        let quests = data()
            .quests
            .iter()
            .filter_map(|def| {
                let prog = p.quests.get(&def.id);
                let state = match prog {
                    Some(q) if q.state == 2 => "completed",
                    Some(_) => "active",
                    None => {
                        // 前置完成才可接
                        let ok = def
                            .prereq
                            .as_deref()
                            .is_none_or(|pr| p.quests.get(pr).is_some_and(|q| q.state == 2));
                        if !ok {
                            return None;
                        }
                        "available"
                    }
                };
                Some(protocol::QuestInfo {
                    id: def.id.to_string(),
                    name: def.name.to_string(),
                    state: state.to_string(),
                    objectives: def
                        .objectives
                        .iter()
                        .enumerate()
                        .map(|(i, (target, required))| protocol::QuestObjectiveInfo {
                            target_id: target.to_string(),
                            current: prog.map(|q| q.counts[i]).unwrap_or(0).min(*required),
                            required: *required,
                        })
                        .collect(),
                    exp_reward: def.exp_reward,
                    gold_reward: def.gold_reward,
                    // 名称/图标在服务端解析好, 客户端不必再查物品表
                    item_rewards: def
                        .rewards
                        .iter()
                        .map(|rw| {
                            let d = item_def(&rw.item);
                            protocol::QuestRewardInfo {
                                name: d
                                    .as_ref()
                                    .map(|d| d.name.clone())
                                    .unwrap_or_else(|| rw.item.clone()),
                                count: rw.count,
                                image: d.map(|d| d.image).unwrap_or(0),
                            }
                        })
                        .collect(),
                })
            })
            .collect();
        send_to(
            &self.sessions,
            &p.conn_id,
            ServerMessage::QuestState { quests },
        )
        .await;
    }

    async fn handle_accept_quest(&mut self, conn_id: &str, quest_id: &str) {
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        let Some(def) = quest_def(quest_id) else {
            return;
        };
        {
            let p = self.players.get_mut(&char_id).unwrap();
            if p.quests.contains_key(quest_id) {
                return;
            }
            let prereq_ok = def
                .prereq
                .as_deref()
                .is_none_or(|pr| p.quests.get(pr).is_some_and(|q| q.state == 2));
            if !prereq_ok {
                return;
            }
            p.quests.insert(
                quest_id.to_string(),
                QuestProgress {
                    state: 1,
                    counts: vec![0; def.objectives.len()],
                },
            );
        }
        self.send_quests(&char_id).await;
    }

    async fn handle_complete_quest(&mut self, conn_id: &str, quest_id: &str) {
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        let Some(def) = quest_def(quest_id) else {
            return;
        };
        {
            let p = self.players.get_mut(&char_id).unwrap();
            let Some(prog) = p.quests.get_mut(quest_id) else {
                return;
            };
            if prog.state != 1 {
                return;
            }
            let done = def
                .objectives
                .iter()
                .enumerate()
                .all(|(i, (_, required))| prog.counts[i] >= *required);
            if !done {
                return;
            }
        }
        let conn = self.players[&char_id].conn_id.clone();
        // 物品奖励先备好并占位检查 —— 位置不够就整单不发, 否则奖励会凭空消失
        let payout: Vec<protocol::ItemInfo> = def
            .rewards
            .iter()
            .flat_map(|rw| (0..rw.count).filter_map(|_| make_item(&rw.item)))
            .collect();
        {
            let p = self.players.get(&char_id).unwrap();
            if p.inventory.len() + payout.len() > MAX_INVENTORY {
                self.notify(
                    &conn,
                    &format!("背包空位不足 ({} 件奖励), 整理后再来交任务", payout.len()),
                )
                .await;
                return;
            }
        }
        let gold = {
            let p = self.players.get_mut(&char_id).unwrap();
            p.quests.get_mut(quest_id).unwrap().state = 2;
            p.inventory.extend(payout.iter().cloned());
            p.gold = p.gold.saturating_add(def.gold_reward);
            p.gold
        };
        send_to(
            &self.sessions,
            &conn,
            ServerMessage::Notification {
                message: format!("任务完成: {}", def.name),
                notification_type: "quest".into(),
            },
        )
        .await;
        self.award_exp(&char_id, def.exp_reward).await;
        if def.gold_reward > 0 {
            let _ = self.db.save_gold(&char_id, gold).await;
            self.send_gold(&conn, gold).await;
            self.notify(&conn, &format!("获得金币 {}", def.gold_reward))
                .await;
        }
        if !payout.is_empty() {
            let names: Vec<String> = payout.iter().map(|i| i.name.clone()).collect();
            self.notify(&conn, &format!("获得物品: {}", names.join("、")))
                .await;
            self.send_inventory(&char_id).await;
        }
        self.send_quests(&char_id).await;
    }

    async fn handle_abandon_quest(&mut self, conn_id: &str, quest_id: &str) {
        let Some(char_id) = self.char_by_conn(conn_id) else {
            return;
        };
        {
            let p = self.players.get_mut(&char_id).unwrap();
            match p.quests.get(quest_id) {
                Some(q) if q.state == 1 => {
                    p.quests.remove(quest_id);
                }
                _ => return,
            }
        }
        self.send_quests(&char_id).await;
    }

    fn char_by_conn(&self, conn_id: &str) -> Option<String> {
        self.players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id)
            .map(|(id, _)| id.clone())
    }

    /// 击杀掷落: 命中的物品直接入包 (经典拾取交互后续再做)
    async fn roll_drops(&mut self, _char_id: &str, mon_id: &str) {
        let Some(drops) = self
            .monsters
            .iter()
            .find(|m| m.id == mon_id)
            .map(|m| m.drops.clone())
        else {
            return;
        };
        let mut gained = Vec::new();
        for d in drops {
            if self.rand01() < d.chance {
                if let Some(item) = make_item(&d.item) {
                    gained.push(item);
                }
            }
        }
        if gained.is_empty() {
            return;
        }
        // 原版语义: 掉落物落地, 玩家点击拾取 (背包满也不丢失)
        let Some((zone, mx, my)) = self
            .monsters
            .iter()
            .find(|m| m.id == mon_id)
            .map(|m| (m.zone.clone(), m.x, m.y))
        else {
            return;
        };
        for (i, item) in gained.into_iter().enumerate() {
            let ang = i as f64 * 2.4;
            let (dx, dy) = (ang.cos() * 0.4 * i as f64, ang.sin() * 0.4 * i as f64);
            self.spawn_ground(&zone, item, mx + dx, my + dy);
        }
        self.broadcast_ground(&zone).await;
    }

    fn spawn_ground(&mut self, zone: &str, item: protocol::ItemInfo, x: f64, y: f64) {
        let id = format!("drop_{}", self.next_drop_id);
        self.next_drop_id += 1;
        self.ground.push(GroundItem {
            id,
            zone: zone.to_string(),
            item,
            x,
            y,
            expire: Instant::now() + GROUND_ITEM_TTL,
        });
    }

    /// 区域地面物品全量快照 → 该区在线玩家
    async fn broadcast_ground(&self, zone: &str) {
        let items: Vec<protocol::GroundItemInfo> = self
            .ground
            .iter()
            .filter(|g| g.zone == zone)
            .map(|g| protocol::GroundItemInfo {
                id: g.id.clone(),
                name: g.item.name.clone(),
                image: g.item.image,
                x: g.x,
                y: g.y,
            })
            .collect();
        let conns = self.zone_conns(zone);
        broadcast_to(&self.sessions, &conns, ServerMessage::GroundItems { items }).await;
    }

    /// 丢弃: 背包移除 → 脚下落地
    async fn handle_drop_item(&mut self, conn_id: &str, item_id: &str) {
        let Some((char_id, zone, x, y, idx)) = self
            .players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id && p.connected)
            .and_then(|(id, p)| {
                let idx = p.inventory.iter().position(|i| i.id == item_id)?;
                Some((id.clone(), p.zone.clone(), p.x, p.y, idx))
            })
        else {
            return;
        };
        let item = self
            .players
            .get_mut(&char_id)
            .map(|p| p.inventory.remove(idx));
        if let Some(item) = item {
            self.spawn_ground(&zone, item, x, y);
            self.send_inventory(&char_id).await;
            self.broadcast_ground(&zone).await;
        }
    }

    /// 拾取: 距离与容量校验 → 入包
    async fn handle_pickup_item(&mut self, conn_id: &str, drop_id: &str) {
        let Some((char_id, conn, zone, px, py, inv_len)) = self
            .players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id && p.connected)
            .map(|(id, p)| {
                (
                    id.clone(),
                    p.conn_id.clone(),
                    p.zone.clone(),
                    p.x,
                    p.y,
                    p.inventory.len(),
                )
            })
        else {
            return;
        };
        let Some(gi) = self
            .ground
            .iter()
            .position(|g| g.id == drop_id && g.zone == zone)
        else {
            return;
        };
        let g = &self.ground[gi];
        if ((g.x - px).powi(2) + (g.y - py).powi(2)).sqrt() > 2.0 {
            send_to(
                &self.sessions,
                &conn,
                ServerMessage::Notification {
                    message: "距离太远, 无法拾取".into(),
                    notification_type: "warn".into(),
                },
            )
            .await;
            return;
        }
        if inv_len >= MAX_INVENTORY {
            send_to(
                &self.sessions,
                &conn,
                ServerMessage::Notification {
                    message: "背包已满".into(),
                    notification_type: "warn".into(),
                },
            )
            .await;
            return;
        }
        let g = self.ground.remove(gi);
        let name = g.item.name.clone();
        if let Some(p) = self.players.get_mut(&char_id) {
            p.inventory.push(g.item);
        }
        send_to(
            &self.sessions,
            &conn,
            ServerMessage::Notification {
                message: format!("获得物品: {name}"),
                notification_type: "loot".into(),
            },
        )
        .await;
        self.send_inventory(&char_id).await;
        self.broadcast_ground(&zone).await;
    }

    /// 推送背包与装备
    async fn send_inventory(&self, char_id: &str) {
        let Some(p) = self.players.get(char_id) else {
            return;
        };
        send_to(
            &self.sessions,
            &p.conn_id,
            ServerMessage::InventoryState {
                inventory: p.inventory.clone(),
                equipment: p.equipment.clone(),
            },
        )
        .await;
    }

    /// 穿装: 槽位由物品模板决定, 原槽装备回包
    async fn handle_equip(&mut self, conn_id: &str, item_id: &str) {
        let Some(char_id) = self
            .players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id)
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        {
            let p = self.players.get_mut(&char_id).unwrap();
            let Some(idx) = p.inventory.iter().position(|i| i.id == item_id) else {
                return;
            };
            let item = p.inventory.remove(idx);
            if let Some(old) = p.equipment.insert(item.slot.clone(), item) {
                p.inventory.push(old);
            }
            p.recalc();
        }
        self.send_inventory(&char_id).await;
        self.send_player_status(&char_id).await;
    }

    /// 卸装回包
    async fn handle_unequip(&mut self, conn_id: &str, slot: &str) {
        let Some(char_id) = self
            .players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id)
            .map(|(id, _)| id.clone())
        else {
            return;
        };
        {
            let p = self.players.get_mut(&char_id).unwrap();
            let Some(item) = p.equipment.remove(slot) else {
                return;
            };
            p.inventory.push(item);
            p.recalc();
        }
        self.send_inventory(&char_id).await;
        self.send_player_status(&char_id).await;
    }

    /// 技能施放: 等级/MP/冷却/射程校验 → 按类型结算 → SkillEffect 广播
    async fn handle_use_skill(&mut self, conn_id: &str, skill_id: &str, target_id: Option<String>) {
        let now = Instant::now();
        // 施法者快照 + 校验
        let Some((char_id, zone, px, py, level)) = self
            .players
            .iter()
            .find(|(_, p)| p.conn_id == conn_id)
            .map(|(id, p)| (id.clone(), p.zone.clone(), p.x, p.y, p.level))
        else {
            return;
        };
        let class = self.players[&char_id].character.class;
        let skills = skills_for(class);
        let Some(def) = skills.iter().find(|s| s.id == skill_id) else {
            return;
        };
        {
            let p = &self.players[&char_id];
            if p.level < def.level {
                return;
            }
            if p.cooldowns.get(&def.id).is_some_and(|&t| now < t) {
                return;
            }
            if p.mp < def.mp {
                send_to(
                    &self.sessions,
                    conn_id,
                    ServerMessage::Notification {
                        message: "魔法值不足".into(),
                        notification_type: "warn".into(),
                    },
                )
                .await;
                return;
            }
        }
        // 施法中心: 自我施法 = 自身; 否则目标怪 (射程校验)。
        // 目标/射程无效在扣费之前拒绝 —— 白扣蓝进冷却是 bug
        let center = if def.self_cast {
            (px, py)
        } else {
            let Some(m) = self.monsters.iter().find(|m| {
                Some(m.id.as_str()) == target_id.as_deref() && m.zone == zone && m.alive()
            }) else {
                return;
            };
            let d = ((m.x - px).powi(2) + (m.y - py).powi(2)).sqrt();
            if d > def.range {
                return;
            }
            (m.x, m.y)
        };
        // 校验全过 → 扣蓝 + 进冷却; 顺带积累修炼度 (每次施放 +1, 到量升级)
        let (skill_level, leveled_to) = {
            let p = self.players.get_mut(&char_id).unwrap();
            p.mp -= def.mp;
            p.cooldowns.insert(def.id.clone(), now + def.cd());
            let sp = p.skills.entry(def.id.clone()).or_default();
            let mut leveled = None;
            if sp.level < def.max_level {
                sp.train += 1;
                if sp.train >= def.train_need(sp.level) {
                    sp.train = 0;
                    sp.level += 1;
                    leveled = Some(sp.level);
                }
            }
            (sp.level, leveled)
        };
        // 结算: 技能按职业吃对应攻击系 —— 战士=物理, 法师=魔法, 道士=道术
        // (skills_for 已按职业过滤, 技能职业即角色职业)
        let equip_bonus = match class {
            protocol::CharacterClass::Warrior => self.players[&char_id].equip_attack(),
            protocol::CharacterClass::Mage => self.players[&char_id].equip_magic(),
            protocol::CharacterClass::Taoist => self.players[&char_id].equip_spirit(),
        };
        let dmg_base = attack_for(level) + equip_bonus;
        // 修炼加成: 每级 +level_bonus (烈火 3 级 ×1.9, 私服 4/5 级更凶)
        let train_mult = 1.0 + def.level_bonus * skill_level as f64;
        let mut hit_ids: Vec<(String, i32)> = Vec::new();
        match def.kind {
            SkillKind::Damage(mult) => {
                if let Some(tid) = &target_id {
                    hit_ids.push((tid.clone(), (dmg_base as f64 * mult * train_mult) as i32));
                }
            }
            SkillKind::Aoe { radius, mult } => {
                for m in self
                    .monsters
                    .iter()
                    .filter(|m| m.zone == zone && m.alive())
                    .filter(|m| {
                        ((m.x - center.0).powi(2) + (m.y - center.1).powi(2)).sqrt() <= radius
                    })
                {
                    hit_ids.push((m.id.clone(), (dmg_base as f64 * mult * train_mult) as i32));
                }
            }
            SkillKind::Heal => {
                let p = self.players.get_mut(&char_id).unwrap();
                let amount = ((30 + level as i32 * 5) as f64 * train_mult) as i32;
                p.hp = (p.hp + amount).min(p.max_hp);
            }
        }
        // 特效广播 (客户端按 skill_id 播放)
        let conns = self.zone_conns(&zone);
        broadcast_to(
            &self.sessions,
            &conns,
            ServerMessage::SkillEffect {
                caster_id: char_id.clone(),
                skill_id: def.id.to_string(),
                position: Position {
                    x: center.0,
                    y: center.1,
                },
                targets: hit_ids.iter().map(|(id, _)| id.clone()).collect(),
                level: skill_level,
            },
        )
        .await;
        for (mon_id, dmg) in hit_ids {
            self.hit_monster(&char_id, &mon_id, dmg).await;
        }
        // 升级: 通知 + 重发技能表; 修炼度落库 (每次施放都存, 掉线不丢练度)
        if let Some(new_lv) = leveled_to {
            let conn = self.players[&char_id].conn_id.clone();
            send_to(
                &self.sessions,
                &conn,
                ServerMessage::Notification {
                    message: format!("《{}》修炼至 {} 级!", def.name, new_lv),
                    notification_type: "levelup".into(),
                },
            )
            .await;
        }
        self.send_skill_list(&char_id).await;
        if let Some(p) = self.players.get(&char_id) {
            let _ = self.db.save_skill_progress(&char_id, &p.skills).await;
        }
        self.send_player_status(&char_id).await;
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
            p.recalc();
            p.hp = p.max_hp;
            p.mp = p.max_mp;
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
        let (zone_id, zone_name, minimap) = self
            .zones
            .get(&p.zone)
            .map(|z| (z.id.clone(), z.name.clone(), z.sidecar.minimap))
            .unwrap_or((p.zone.clone(), p.zone.clone(), None));
        let zone_of_npc = zone_id.clone();
        send_to(
            &self.sessions,
            conn_id,
            ServerMessage::ZoneChanged {
                zone_id,
                zone_name,
                position: Position { x, y },
                minimap,
            },
        )
        .await;
        self.send_npc_list(conn_id, &zone_of_npc).await;
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
        // 地面物品过期清理 (按区广播变化)
        if self.ground.iter().any(|g| now >= g.expire) {
            let zones: Vec<String> = self
                .ground
                .iter()
                .filter(|g| now >= g.expire)
                .map(|g| g.zone.clone())
                .collect();
            self.ground.retain(|g| now < g.expire);
            for z in zones {
                self.broadcast_ground(&z).await;
            }
        }
        // 停止判定: 200ms 没有移动包即站立
        for p in self.players.values_mut() {
            if p.moving && now.duration_since(p.last_move) > Duration::from_millis(200) {
                p.moving = false;
            }
        }
        // 每 2s 自然回复 HP/MP
        if now.duration_since(self.last_regen) > Duration::from_secs(2) {
            self.last_regen = now;
            let mut changed = Vec::new();
            for (id, p) in self.players.iter_mut() {
                if !p.connected {
                    continue;
                }
                let (hp0, mp0) = (p.hp, p.mp);
                p.hp = (p.hp + (p.max_hp * 3 / 100).max(1)).min(p.max_hp);
                p.mp = (p.mp + (p.max_mp * 8 / 100).max(1)).min(p.max_mp);
                if p.hp != hp0 || p.mp != mp0 {
                    changed.push(id.clone());
                }
            }
            for id in changed {
                self.send_player_status(&id).await;
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
                let _ = self.db.save_items(&id, &p.inventory, &p.equipment).await;
                let _ = self.db.save_quests(&id, &p.quests).await;
                info!("重连窗过期, 存档并移除: {id}");
            }
        }
        if now.duration_since(self.last_save) > SAVE_EVERY {
            self.last_save = now;
            self.save_all().await;
        }
        // 怪物 AI (有玩家在线才跑)
        if !self.players.is_empty() {
            let hits = self.monster_ai(now);
            self.apply_monster_hits(hits).await;
        }
        // 重生落点要避开玩家, 先抄一份位置快照 (下面 monsters 是可变借用)
        let alive_players: Vec<(String, f64, f64)> = self
            .players
            .values()
            .filter(|p| p.connected)
            .map(|p| (p.zone.clone(), p.x, p.y))
            .collect();
        let zones = &self.zones;
        // 怪物生命周期: 死亡动画到点 → 等重生; 重生到点 → 回家满血复活
        for m in self.monsters.iter_mut() {
            if m.dying_until.is_some_and(|t| now >= t) {
                m.dying_until = None;
                m.respawn_at = Some(now + m.respawn);
            }
            if m.respawn_at.is_some_and(|t| now >= t) {
                m.respawn_at = None;
                m.hp = m.max_hp;
                // 老家被人占着就挪开一点刷, 免得一出生就和玩家重叠
                let here: Vec<(f64, f64)> = alive_players
                    .iter()
                    .filter(|(z, _, _)| z == &m.zone)
                    .map(|(_, x, y)| (*x, *y))
                    .collect();
                let (hx, hy) = match zones.get(&m.zone) {
                    Some(z) => nearest_free(&z.walk, m.home.0, m.home.1, &here),
                    None => m.home,
                };
                m.x = hx;
                m.y = hy;
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
                    // 外观: 衣甲缺省 0 (基础模), 武器无则不带
                    armour: Some(p.equipment.get("armor").map(|i| i.shape).unwrap_or(0)),
                    weapon: p.equipment.get("weapon").map(|i| i.shape),
                    image: None, // 玩家走 CArmour/CWeapon, 不用怪物图库
                    image_base: None,
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
                                armour: None,
                                weapon: None,
                                image: None,
                                image_base: None,
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
                            armour: None,
                            weapon: None,
                            image: Some(m.image),
                            image_base: Some(m.image_base),
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
        // NPC 位置快照 (同样避免借用冲突)
        let npc_pos: Vec<(String, f64, f64)> = data()
            .npcs
            .iter()
            .filter(|n| n.enabled)
            .map(|n| (n.map.clone(), n.x, n.y))
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
                // 8 向量化移动 (原版语义): 位移严格沿朝向轴。转向贪心懒惰:
                // 沿当前朝向仍有进展就不换向 (走完该轴分量才重选主导方向),
                // 一段斜线最多转向一两次 — 避免逐 tick 重算导致的高频摆头
                let cur = (m.dir as usize) % 8;
                let (cvx, cvy) = sim::DIR8[cur];
                let dir = if dx * cvx + dy * cvy > 1e-6 {
                    cur
                } else {
                    dir8_from(dx, dy)
                };
                m.dir = dir as u8;
                let (vx, vy) = sim::DIR8[dir];
                let adv = step.min(dx * vx + dy * vy);
                let (sx, sy) = (vx * adv, vy * adv);
                // 怪也不许穿人穿摊主 (怪之间不互挡, 见 blockers_for 注记)
                let blockers: Vec<(f64, f64)> = players
                    .iter()
                    .filter(|(_, z, _, _)| z == &m.zone)
                    .map(|(_, _, x, y)| (*x, *y))
                    .chain(
                        npc_pos
                            .iter()
                            .filter(|(z, _, _)| z == &m.zone)
                            .map(|(_, x, y)| (*x, *y)),
                    )
                    .collect();
                let (nx, ny) =
                    sim::resolve_move(&zone.walk, (m.x, m.y), (sx, sy), BODY_RADIUS, &blockers);
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
            let dmg = (dmg - p.equip_defense()).max(1);
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

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::CharacterClass;

    /// 合成测试世界: 20×20 全可走 z1 (带 z2 传送门 + 1 只稻草人) + 10×10 z2
    fn test_zones() -> HashMap<String, Zone> {
        let mut zones = HashMap::new();
        zones.insert(
            "z1".to_string(),
            Zone {
                id: "z1".into(),
                name: "测试区".into(),
                walk: WalkGrid::from_cells(40, 40, |_, _| false),
                spawn: (5.5, 5.5),
                portals: vec![Portal {
                    x: 15.5,
                    y: 5.5,
                    to_zone: "z2".into(),
                    to_x: None,
                    to_y: None,
                }],
                monster_spawns: vec![MonsterSpawn {
                    // 故意用不存在于怪物模板表的名字 — 覆盖"旧边车内联数值兜底"路径
                    template: "test_dummy".into(),
                    image: 5,
                    x: 10.0,
                    y: 10.0,
                    count: 1,
                    radius: 0.5,
                    passive: false,
                    hp: 12,
                    damage: 4,
                    exp: 20,
                    drops: vec![DropEntry {
                        item: "iron_sword".into(),
                        chance: 1.0,
                    }],
                }],
                sidecar: ZoneSidecar::default(),
            },
        );
        zones.insert(
            "z2".to_string(),
            Zone {
                id: "z2".into(),
                name: "测试区2".into(),
                walk: WalkGrid::from_cells(10, 10, |_, _| false),
                spawn: (5.5, 5.5),
                portals: vec![],
                monster_spawns: vec![],
                sidecar: ZoneSidecar::default(),
            },
        );
        zones
    }

    async fn test_game() -> Game {
        let db = Db::open(":memory:").await.unwrap();
        let (gw, _rx) = crate::gateway::Gateway::new();
        Game::new(test_zones(), "z1".into(), db, gw.sessions(), HashMap::new())
    }

    fn test_player(conn: &str, zone: &str, x: f64, y: f64) -> PlayerState {
        let level = 1;
        PlayerState {
            conn_id: conn.into(),
            account_id: "acc".into(),
            character: CharacterRow {
                id: "char1".into(),
                name: "测试".into(),
                class: CharacterClass::Warrior,
                gender: "male".into(),
                level,
                exp: 0,
                gold: 0,
                zone: zone.into(),
                inventory: Vec::new(),
                equipment: HashMap::new(),
                quests: HashMap::new(),
                skills: HashMap::new(),
                x,
                y,
            },
            zone: zone.into(),
            x,
            y,
            gold: 0,
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
            exp: 0,
            last_attack: Instant::now() - PLAYER_ATTACK_CD,
            cooldowns: HashMap::new(),
            inventory: Vec::new(),
            equipment: HashMap::new(),
            quests: HashMap::new(),
            skills: HashMap::new(),
        }
    }

    #[test]
    fn stat_formulas() {
        assert_eq!(max_hp_for(1), 52);
        assert_eq!(max_mp_for(1), 38);
        assert_eq!(attack_for(1), 6);
        assert_eq!(exp_required(1), 100);
        assert!(max_hp_for(10) > max_hp_for(1));
    }

    #[test]
    fn static_tables_consistent() {
        // 三职业各 3 技能, id 全局唯一, 等级门槛非降序
        let mut ids = std::collections::HashSet::new();
        for class in [
            CharacterClass::Warrior,
            CharacterClass::Mage,
            CharacterClass::Taoist,
        ] {
            let skills = skills_for(class);
            assert_eq!(skills.len(), 3);
            let mut last_level = 0;
            for s in skills {
                assert!(ids.insert(s.id.clone()), "技能 id 重复: {}", s.id);
                assert!(s.level >= last_level);
                last_level = s.level;
            }
        }
        // 物品模板唯一 + 槽位合法
        let slots = ["weapon", "armor", "helmet", "necklace", "ring"];
        let mut templates = std::collections::HashSet::new();
        let dref = data();
        for d in &dref.items {
            assert!(
                templates.insert(d.template.clone()),
                "物品模板重复: {}",
                d.template
            );
            assert!(slots.contains(&d.slot.as_str()), "非法槽位: {}", d.slot);
        }
        // 任务前置指向存在的任务
        for q in &dref.quests {
            if let Some(pr) = q.prereq.as_deref() {
                assert!(quest_def(pr).is_some(), "任务 {} 前置 {pr} 不存在", q.id);
            }
        }
    }

    #[test]
    fn parse_teleport_accepts_map_x_y() {
        assert_eq!(
            super::parse_teleport("2.map:300:300.5"),
            Some(("2.map".into(), 300.0, 300.5))
        );
        // 地图名带 . 不影响; 空白容忍
        assert_eq!(
            super::parse_teleport(" 0.map : 12 : 34 "),
            Some(("0.map".into(), 12.0, 34.0))
        );
        // 段数不对 / 非数字 / 空地图名一律拒
        for bad in ["2.map:300", "2.map:300:300:1", "2.map:a:b", ":1:2", ""] {
            assert!(super::parse_teleport(bad).is_none(), "应拒绝: {bad}");
        }
    }

    #[test]
    fn nearest_free_dodges_players() {
        let walk = WalkGrid::from_cells(20, 20, |_, _| false);
        // 老家上正站着人 → 该挪开, 且挪到的位置与人保持碰撞间距
        let here = [(5.5, 5.5)];
        let (x, y) = nearest_free(&walk, 5.5, 5.5, &here);
        let d = ((x - 5.5f64).powi(2) + (y - 5.5f64).powi(2)).sqrt();
        assert!(d >= sim::ENTITY_CLEARANCE, "刷点应让开玩家, 实际距离 {d}");
        assert!(walk.is_walkable_circle(x, y, BODY_RADIUS));
        // 没人时原样返回
        assert_eq!(nearest_free(&walk, 5.5, 5.5, &[]), (5.5, 5.5));
    }

    #[test]
    fn nearest_free_falls_back_when_surrounded() {
        let walk = WalkGrid::from_cells(20, 20, |_, _| false);
        // 周围 12 圈全被占满: 退回"可站立"的原位, 宁可重叠也要刷出来
        let mut here = Vec::new();
        for dy in -13..=13 {
            for dx in -13..=13 {
                here.push((5.5 + dx as f64, 5.5 + dy as f64));
            }
        }
        let (x, y) = nearest_free(&walk, 5.5, 5.5, &here);
        assert!(
            walk.is_walkable_circle(x, y, BODY_RADIUS),
            "兜底也必须站得住"
        );
    }

    #[test]
    fn nearest_free_still_avoids_walls() {
        // 目标格是墙且旁边站着人: 两个条件都要满足
        let walk = WalkGrid::from_cells(10, 10, |x, y| x == 5 && y == 5);
        let here = [(4.5, 5.5)];
        let (x, y) = nearest_free(&walk, 5.5, 5.5, &here);
        assert!(walk.is_walkable_circle(x, y, BODY_RADIUS), "不能落在墙里");
        let d = ((x - 4.5f64).powi(2) + (y - 5.5f64).powi(2)).sqrt();
        assert!(d >= sim::ENTITY_CLEARANCE, "也不能压着人");
    }

    #[test]
    fn nearest_walkable_snaps_out_of_walls() {
        // 中心格阻挡的 5×5
        let walk = WalkGrid::from_cells(5, 5, |x, y| x == 2 && y == 2);
        let (x, y) = nearest_walkable(&walk, 2.5, 2.5);
        assert!(walk.is_walkable_circle(x, y, BODY_RADIUS));
        assert!((x - 2.5).abs() + (y - 2.5).abs() > 0.4, "应吸附到邻格");
        // 本就可走则原样返回
        assert_eq!(nearest_walkable(&walk, 0.5, 0.5), (0.5, 0.5));
    }

    #[tokio::test]
    async fn move_speed_clamped() {
        let mut g = test_game().await;
        let mut p = test_player("c1", "z1", 5.5, 5.5);
        p.last_move = Instant::now() - Duration::from_secs(1);
        g.players.insert("char1".into(), p);
        // 一包要求瞬移 50 格 → 按 0.5s 窗 × 上限 4.08 限幅
        g.handle_move("c1", Position { x: 50.0, y: 0.0 });
        let p = &g.players["char1"];
        assert!(p.x < 5.5 + 2.1, "超速未限幅: {}", p.x);
        assert!(p.x > 5.5 + 1.9);
    }

    #[tokio::test]
    async fn portal_switches_zone() {
        let mut g = test_game().await;
        let mut p = test_player("c1", "z1", 15.2, 5.5);
        p.last_move = Instant::now() - Duration::from_millis(200);
        g.players.insert("char1".into(), p);
        let hit = g.handle_move("c1", Position { x: 0.2, y: 0.0 });
        let (_, _, x, y) = hit.expect("应触发传送门");
        let p = &g.players["char1"];
        assert_eq!(p.zone, "z2");
        assert_eq!((p.x, p.y), (x, y));
        assert_eq!((x, y), (5.5, 5.5)); // 落在 z2 出生点
    }

    #[tokio::test]
    async fn monster_ai_aggro_and_leash() {
        let mut g = test_game().await;
        // 距怪 ~3 格 (仇恨 6 格内, 出手 1.6 格外) → 追击
        g.players
            .insert("char1".into(), test_player("c1", "z1", 13.0, 10.0));
        let now = Instant::now();
        g.monsters[0].next_decide = now;
        g.monster_ai(now);
        assert!(g.monsters[0].chasing, "仇恨范围内玩家应触发追击");
        // 拉离 12 格 → 回家
        g.monsters[0].x = g.monsters[0].home.0 + 15.0;
        g.monsters[0].attack_until = None;
        g.monsters[0].next_decide = now;
        g.monster_ai(now);
        assert!(!g.monsters[0].chasing);
        assert_eq!(g.monsters[0].target, Some(g.monsters[0].home));
        // 被动怪不追击
        g.monsters[0].x = g.monsters[0].home.0;
        g.monsters[0].passive = true;
        g.monsters[0].target = None;
        g.monsters[0].attack_until = None;
        g.monsters[0].next_decide = now;
        g.monster_ai(now);
        assert!(!g.monsters[0].chasing, "被动怪不应追击");
    }

    #[tokio::test]
    async fn monster_moves_along_dir8() {
        let mut g = test_game().await;
        // 玩家在斜向偏 10° 处 (非 8 向轴) → 追击位移仍须严格沿 8 向
        g.players
            .insert("char1".into(), test_player("c1", "z1", 14.0, 10.7));
        let now = Instant::now();
        g.monsters[0].next_decide = now;
        let (x0, y0) = (g.monsters[0].x, g.monsters[0].y);
        g.monster_ai(now);
        let m = &g.monsters[0];
        let (dx, dy) = (m.x - x0, m.y - y0);
        assert!(dx.hypot(dy) > 1e-6, "追击应产生位移");
        let (vx, vy) = sim::DIR8[m.dir as usize];
        // 位移与朝向向量共线 (叉积≈0) 且同向
        assert!(
            (dx * vy - dy * vx).abs() < 1e-9,
            "位移 ({dx},{dy}) 未沿 dir{} 轴",
            m.dir
        );
        assert!(dx * vx + dy * vy > 0.0);
    }

    #[tokio::test]
    async fn kill_awards_exp_and_drops() {
        let mut g = test_game().await;
        g.players
            .insert("char1".into(), test_player("c1", "z1", 10.0, 10.0));
        let mon_id = g.monsters[0].id.clone();
        let killed = g.hit_monster("char1", &mon_id, 12).await;
        assert!(killed);
        assert!(g.monsters[0].dying_until.is_some());
        let p = &g.players["char1"];
        assert_eq!(p.exp, 20, "击杀应得 20 经验");
        assert!(p.inventory.is_empty(), "掉落应落地而非直接入包");
        assert_eq!(g.ground.len(), 1, "100% 掉落应落地");
        assert_eq!(g.ground[0].item.template, "iron_sword");
        // 已死怪不能再打
        assert!(!g.hit_monster("char1", &mon_id, 12).await);
    }

    #[tokio::test]
    async fn equip_affects_stats() {
        let mut g = test_game().await;
        let mut p = test_player("c1", "z1", 5.5, 5.5);
        p.inventory.push(make_item("iron_sword").unwrap());
        p.inventory.push(make_item("leather_armor").unwrap());
        let (sword, armor) = (p.inventory[0].id.clone(), p.inventory[1].id.clone());
        g.players.insert("char1".into(), p);
        g.handle_equip("c1", &sword).await;
        g.handle_equip("c1", &armor).await;
        let p = &g.players["char1"];
        assert_eq!(p.equip_attack(), 6);
        assert_eq!(p.equip_defense(), 4);
        assert_eq!(p.max_hp, max_hp_for(1) + 10, "皮甲 +10 上限");
        assert!(p.inventory.is_empty());
        g.handle_unequip("c1", "weapon").await;
        let p = &g.players["char1"];
        assert_eq!(p.equip_attack(), 0);
        assert_eq!(p.inventory.len(), 1);
    }

    #[tokio::test]
    async fn quest_chain_flow() {
        let mut g = test_game().await;
        g.players
            .insert("char1".into(), test_player("c1", "z1", 5.5, 5.5));
        // 前置未完成不可接猎鹿
        g.handle_accept_quest("c1", "hunt_deer").await;
        assert!(!g.players["char1"].quests.contains_key("hunt_deer"));
        // 接猎鸡 → 杀 3 鸡 → 目标未齐不可交付 → 齐了可交付
        g.handle_accept_quest("c1", "hunt_chicken").await;
        assert_eq!(g.players["char1"].quests["hunt_chicken"].state, 1);
        g.progress_quests("char1", "chicken").await;
        g.handle_complete_quest("c1", "hunt_chicken").await;
        assert_eq!(
            g.players["char1"].quests["hunt_chicken"].state, 1,
            "目标未齐不应交付"
        );
        g.progress_quests("char1", "chicken").await;
        g.progress_quests("char1", "chicken").await;
        // 多杀不越界
        g.progress_quests("char1", "chicken").await;
        assert_eq!(g.players["char1"].quests["hunt_chicken"].counts[0], 3);
        g.handle_complete_quest("c1", "hunt_chicken").await;
        let p = &g.players["char1"];
        assert_eq!(p.quests["hunt_chicken"].state, 2);
        assert_eq!(p.exp, 50, "交付应得 50 经验");
        // 前置完成 → 猎鹿可接
        g.handle_accept_quest("c1", "hunt_deer").await;
        assert_eq!(g.players["char1"].quests["hunt_deer"].state, 1);
    }

    #[tokio::test]
    async fn drop_pickup_roundtrip_and_limits() {
        let mut g = test_game().await;
        let mut p = test_player("c1", "z1", 5.5, 5.5);
        p.inventory.push(make_item("wooden_sword").unwrap());
        let item_id = p.inventory[0].id.clone();
        g.players.insert("char1".into(), p);
        // 丢弃 → 落地脚下
        g.handle_drop_item("c1", &item_id).await;
        assert!(g.players["char1"].inventory.is_empty());
        assert_eq!(g.ground.len(), 1);
        let drop_id = g.ground[0].id.clone();
        // 拾取 → 回包
        g.handle_pickup_item("c1", &drop_id).await;
        assert!(g.ground.is_empty());
        assert_eq!(g.players["char1"].inventory.len(), 1);
        // 距离超 2 格拒绝
        let far_id = {
            let item = make_item("copper_ring").unwrap();
            g.spawn_ground("z1", item, 15.5, 15.5);
            g.ground[0].id.clone()
        };
        g.handle_pickup_item("c1", &far_id).await;
        assert_eq!(g.ground.len(), 1, "超距拾取应被拒绝");
        // 背包满拒绝
        g.ground[0].x = 5.5;
        g.ground[0].y = 5.5;
        for _ in 0..MAX_INVENTORY {
            let it = make_item("copper_ring").unwrap();
            g.players.get_mut("char1").unwrap().inventory.push(it);
        }
        g.handle_pickup_item("c1", &far_id).await;
        assert_eq!(g.ground.len(), 1, "背包满应拒绝拾取");
    }

    #[tokio::test]
    async fn chat_targets_by_channel() {
        let mut g = test_game().await;
        // 同区两人 + 异区一人
        g.players
            .insert("char1".into(), test_player("c1", "z1", 5.5, 5.5));
        let mut p2 = test_player("c2", "z1", 6.5, 5.5);
        p2.character.id = "char2".into();
        g.players.insert("char2".into(), p2);
        let mut p3 = test_player("c3", "z2", 5.5, 5.5);
        p3.character.id = "char3".into();
        g.players.insert("char3".into(), p3);

        let mut world = g.chat_targets("c1", protocol::ChatChannel::World);
        world.sort();
        assert_eq!(world, ["c1", "c2", "c3"], "世界频道应全服可见");
        let mut zone = g.chat_targets("c1", protocol::ChatChannel::Zone);
        zone.sort();
        assert_eq!(zone, ["c1", "c2"], "区域频道仅同区可见");
        assert_eq!(
            g.chat_targets("c1", protocol::ChatChannel::Whisper),
            ["c1"],
            "未实现频道仅回显自己"
        );
        // 断线者不可见
        g.players.get_mut("char2").unwrap().connected = false;
        let mut zone = g.chat_targets("c1", protocol::ChatChannel::Zone);
        zone.sort();
        assert_eq!(zone, ["c1"]);
        // 未在场的连接无目标
        assert!(g
            .chat_targets("nobody", protocol::ChatChannel::World)
            .is_empty());
    }
}
