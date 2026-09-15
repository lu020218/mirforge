//! 游戏内容配置的纯数据类型 (hub 与区服共用)。
//!
//! 只放「数据 + serde + 无状态校验」; 运行时行为 (Zone 网格/Game 循环/
//! 玩家状态) 留在 server。`GameData` 是全部配置的内存快照单元。

use std::time::Duration;

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ZoneSidecar {
    pub name: Option<String>,
    pub spawn: Option<(f64, f64)>,
    /// 小地图帧号 (Data/mmap.Lib); None = 无小地图
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimap: Option<u16>,
    /// 区域 BGM 曲名 (packs/sound/bgm/<名>.ogg); None = 无 BGM
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bgm: Option<String>,
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
    /// 怪物类型: "normal" 普通 / "tameable" 可诱惑 (诱惑之光合法目标) /
    /// "undead" 不死系 (不可诱惑; 为圣言术预留)
    #[serde(default = "default_mon_type")]
    pub mon_type: String,
    /// 怪物等级 (展示用: 目标栏等级徽标)
    #[serde(default = "default_mon_level")]
    pub level: u32,
    /// 音效基址: packs/sound/mon/{基址:03}-1/2/3.ogg (攻击/受击/死亡);
    /// -1 = 跟随形象号 (购入图库编号常与经典音效编号不一致, 故可单配)
    #[serde(default = "default_mon_sound")]
    pub sound: i32,
    /// 宠物成长 (仅被召唤技能引用时生效): 等级上限 (经典 7)
    #[serde(default = "default_pet_max_level")]
    pub pet_max_level: u32,
    /// 宠物升级经验基数: 升到 L+1 需 基数 × L
    #[serde(default = "default_pet_exp_base")]
    pub pet_exp_base: u64,
    /// 宠物每级 HP/攻击乘法成长 (升级回满血)
    #[serde(default = "default_pet_grow")]
    pub pet_grow: f64,
}

fn default_mon_type() -> String {
    "normal".into()
}
fn default_mon_level() -> u32 {
    1
}
fn default_mon_sound() -> i32 {
    -1
}
fn default_pet_max_level() -> u32 {
    7
}
fn default_pet_exp_base() -> u64 {
    100
}
fn default_pet_grow() -> f64 {
    1.2
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillKind {
    /// 单体伤害 (攻击 × 倍率)
    Damage(f64),
    /// 以目标/自身为圆心的范围伤害
    Aoe { radius: f64, mult: f64 },
    /// 治疗自身
    Heal,
    /// 持续毒伤 (施毒术): 每 [`POISON_TICK`] 跳一次, 每跳 = 攻击 × 倍率
    Dot { tick_mult: f64, secs: f64 },
    /// 位移冲锋 (野蛮冲撞): 朝目标方向冲锋至多 `range` 格, 撞到的
    /// 第一个实体受 攻击×mult 伤害、沿冲向击退 1 格并僵直 stun_secs。
    ///
    /// 机制类型全景 (后续批次按需增加变体, serde 纯增量):
    /// Blink(瞬移)/Knockback(击退)/Buff/Debuff/Summon(召唤)/
    /// GroundAoe(场地)/Empower(普攻强化) — 字符串参数走 kind_s1 列。
    Charge { mult: f64, stun_secs: f64 },
    /// 召唤 (召唤骷髅/神兽): 召出 count 只 template 怪物模板做宠物,
    /// 跟随主人并攻击附近敌怪; secs 秒后消散 (0 = 直到死亡/下线)。
    /// 宠物数值随修炼等级按 level_bonus 放大, 外观按等级换形态
    /// (image_base += 等级×360)。template 存 kind_s1 列。
    Summon {
        template: String,
        count: u32,
        secs: f64,
    },
    /// 诱惑 (诱惑之光): 对「可诱惑」类型的无主怪按概率魅惑为宝宝
    /// (chance + 修炼每级加成, 上限 0.9); 同时持有至多 max_pets 只
    /// (超限替换最早); 失败拉仇恨。
    Tame { chance: f64, max_pets: u32 },
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
    pub fn cd(&self) -> Duration {
        Duration::from_millis(self.cd_ms)
    }
    /// 从 lvl 升到 lvl+1 所需熟练度
    pub fn train_need(&self, lvl: u32) -> u32 {
        self.train_base.max(1) * (lvl + 1)
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillsCfg {
    pub warrior: Vec<SkillDef>,
    pub mage: Vec<SkillDef>,
    pub taoist: Vec<SkillDef>,
}

/// 全部数据配置 (物品/技能/任务)。内置默认与 server/data/*.json 同源
#[derive(Clone, serde::Serialize, serde::Deserialize)]
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

impl GameData {
    pub fn builtin() -> Self {
        GameData {
            items: serde_json::from_str(include_str!("../../../server/data/items.json"))
                .expect("内置 items"),
            skills: serde_json::from_str(include_str!("../../../server/data/skills.json"))
                .expect("内置 skills"),
            quests: serde_json::from_str(include_str!("../../../server/data/quests.json"))
                .expect("内置 quests"),
            npcs: serde_json::from_str(include_str!("../../../server/data/npcs.json"))
                .expect("内置 npcs"),
            bosses: serde_json::from_str(include_str!("../../../server/data/bosses.json"))
                .expect("内置 bosses"),
            monsters: serde_json::from_str(include_str!("../../../server/data/monsters.json"))
                .expect("内置 monsters"),
        }
    }

    /// 从 JSON 目录读取种子数据 (仅首次建库时用; 缺文件回退内置默认)
    pub fn seed_source(dir: &std::path::Path) -> Self {
        fn read<T: serde::de::DeserializeOwned>(p: std::path::PathBuf, what: &str) -> Option<T> {
            let text = std::fs::read_to_string(&p).ok()?;
            match serde_json::from_str(&text) {
                Ok(v) => {
                    tracing::info!("{what} 种子: {p:?}");
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

/// 掉落表条目 (边车配置)
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct DropEntry {
    pub item: String,
    pub chance: f64,
}

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

/// 背包容量上限 (validate 用; 与客户端 panels::BAG_SLOTS 一致)
pub const MAX_INVENTORY: usize = 50;

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

fn default_boss_respawn() -> u64 {
    1800
}

fn unlimited_stock() -> i32 {
    -1
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

fn one_u32() -> u32 {
    1
}

/// hub → 区服的全量配置快照 (rev 单调递增; 区服据此判断是否需要拉取)
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub rev: i64,
    pub data: GameData,
    /// 区域边车: 地图文件名(小写) → 边车
    pub zones: std::collections::HashMap<String, ZoneSidecar>,
}
