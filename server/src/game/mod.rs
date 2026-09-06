//! 游戏循环：动作消费 + 20Hz 区域广播 + 会话/断线恢复。
//! M2 范围：登录/建角/进图/移动（sim 校验）/心跳/Resume；玩法系统随 M3 迁入。

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod combat;
use monster::{materialize_bosses, materialize_monsters};
mod items;
mod monster;
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
    /// 技能图标 (packs/magicon.mfl 帧号)
    #[serde(default)]
    pub icon: u32,
    /// 施放动作: "attack" 挥砍 / "cast" 施法; 空按 cast 处理
    #[serde(default)]
    pub anim: String,
    /// 技能类型: 1 一段(命中) / 2 二段(起手+命中) / 3 三段(起手+飞行+命中)
    #[serde(default)]
    pub stages: u8,
    /// 特效名 (packs/magic/<名>.mfl, 通常与技能 id 同名; 纯数字 = 旧编号库)
    #[serde(default)]
    pub fx: String,
    #[serde(default)]
    pub fx_base: u32,
    #[serde(default)]
    pub fx_frames: u8,
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
            items: serde_json::from_str(include_str!("../../data/items.json")).expect("内置 items"),
            skills: serde_json::from_str(include_str!("../../data/skills.json"))
                .expect("内置 skills"),
            quests: serde_json::from_str(include_str!("../../data/quests.json"))
                .expect("内置 quests"),
            npcs: serde_json::from_str(include_str!("../../data/npcs.json")).expect("内置 npcs"),
            bosses: serde_json::from_str(include_str!("../../data/bosses.json"))
                .expect("内置 bosses"),
            monsters: serde_json::from_str(include_str!("../../data/monsters.json"))
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
    /// 尸体停留截止 (死亡动画结束 → 躺尸到此时刻才广播移除)
    corpse_until: Option<Instant>,
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
    /// 技能延迟结算队列: 与客户端起手/飞行编排对齐 (到点才掉血)
    pending_hits: Vec<PendingHit>,
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
            last_save: Instant::now(),
            last_regen: Instant::now(),
            pending_hits: Vec::new(),
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
}
