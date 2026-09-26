//! # protocol
//!
//! 客户端/服务器共享的全部消息类型（开发计划 M2 任务 2.1）。
//! 原则：双端唯一真源；带协议版本协商；零重依赖（仅 serde/serde_json）。
//!
//! 线格式：JSON 文本帧（WebSocket text），`type` 字段做 tag——与旧客户端
//! 线格式兼容，调试期可读性优先；将来切二进制通道时只改编解码层。
//!
//! 版本协商：客户端连接后必须先发 [`ClientMessage::Hello`]；服务器回
//! [`ServerMessage::HelloAck`]（版本不符则回 [`ServerMessage::Error`] 并断开）。
//!
//! `EnterGame`/`InventoryState` 暂保留 `serde_json::Value` 载荷（旧协议原样），
//! M3 玩法迁移时随角色/背包模型一起类型化。

use serde::{Deserialize, Serialize};

/// 协议版本。破坏性变更 +1；Hello/HelloAck 协商。
pub const PROTOCOL_VERSION: u32 = 1;

// ─────────── 共享基础类型 ───────────

/// 世界格坐标（连续浮点，与 sim crate 同一坐标系）
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum CharacterClass {
    Warrior,
    Mage,
    Taoist,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChatChannel {
    World,
    Zone,
    Party,
    Guild,
    Whisper,
}

// ─────────── 客户端 → 服务器 ───────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    /// 连接后的第一条消息：协议版本协商
    #[serde(rename = "hello")]
    Hello { version: u32 },
    #[serde(rename = "login")]
    Login { username: String, password: String },
    #[serde(rename = "register")]
    Register {
        username: String,
        password: String,
        /// 密保问题/答案 (可选, 密码找回用; 登录器注册页填写)
        #[serde(default)]
        security_question: Option<String>,
        #[serde(default)]
        security_answer: Option<String>,
    },
    /// 登录器: 登录成功后申请一次性启动票据 (60 秒有效, 用后即焚)
    #[serde(rename = "requestTicket")]
    RequestTicket,
    /// 客户端: 凭登录器签发的票据免密进入 (等效登录成功)
    #[serde(rename = "ticketAuth")]
    TicketAuth { ticket: String },
    /// 凭密保答案重设密码
    #[serde(rename = "resetPassword")]
    ResetPassword {
        username: String,
        security_answer: String,
        new_password: String,
    },
    #[serde(rename = "createCharacter")]
    CreateCharacter {
        name: String,
        class: CharacterClass,
        #[serde(default = "default_gender")]
        gender: String,
    },
    #[serde(rename = "selectCharacter")]
    SelectCharacter { character_id: String },
    /// 断线重连：凭 token 恢复会话（免重登录，原位恢复）
    #[serde(rename = "resume")]
    Resume { token: String, character_id: String },
    #[serde(rename = "heartbeat")]
    Heartbeat,
    #[serde(rename = "move")]
    Move { direction: Position },
    #[serde(rename = "attack")]
    Attack { target_id: String, skill_id: String },
    #[serde(rename = "useSkill")]
    UseSkill {
        skill_id: String,
        target_id: Option<String>,
        position: Option<Position>,
    },
    #[serde(rename = "chat")]
    Chat {
        channel: ChatChannel,
        content: String,
        target_name: Option<String>,
    },
    /// 点击 NPC 开始对话
    #[serde(rename = "talkNpc")]
    TalkNpc { npc_id: String },
    /// 选择对话选项
    #[serde(rename = "npcOption")]
    NpcOption { npc_id: String, page: u32, idx: u32 },
    /// 向 NPC 买入 (数量固定 1 件, 与原版一致)
    #[serde(rename = "buyItem")]
    BuyItem { npc_id: String, template: String },
    /// 卖给 NPC
    #[serde(rename = "sellItem")]
    SellItem { npc_id: String, item_id: String },
    #[serde(rename = "equip")]
    Equip { item_id: String, slot: String },
    #[serde(rename = "unequip")]
    Unequip { slot: String },
    /// 丢弃背包物品到脚下
    #[serde(rename = "dropItem")]
    DropItem { item_id: String },
    /// 拾取地面物品
    #[serde(rename = "pickupItem")]
    PickupItem { drop_id: String },
    #[serde(rename = "acceptQuest")]
    AcceptQuest { quest_id: String },
    #[serde(rename = "completeQuest")]
    CompleteQuest { quest_id: String },
    #[serde(rename = "abandonQuest")]
    AbandonQuest { quest_id: String },
    #[serde(rename = "tradeRequest")]
    TradeRequest { target_player_id: String },
    #[serde(rename = "tradeConfirm")]
    TradeConfirm,
    #[serde(rename = "tradeCancel")]
    TradeCancel,
    #[serde(rename = "tradePlaceItem")]
    TradePlaceItem { item_id: String },
    #[serde(rename = "tradeSetGold")]
    TradeSetGold { gold: i64 },
    /// 接受交易邀请
    #[serde(rename = "tradeAccept")]
    TradeAccept,
    /// 拒绝交易邀请
    #[serde(rename = "tradeDecline")]
    TradeDecline,
    /// 从己方托管区取回物品 (成交前)
    #[serde(rename = "tradeTakeItem")]
    TradeTakeItem { item_id: String },
    /// 存物品进仓库 (需在仓库 NPC 旁)
    #[serde(rename = "storeItem")]
    StoreItem { npc_id: String, item_id: String },
    /// 从仓库取物品
    #[serde(rename = "storageTake")]
    StorageTake { npc_id: String, item_id: String },
    /// 切换攻击模式 ("peace"/"all")
    #[serde(rename = "setPkMode")]
    SetPkMode { mode: String },
    #[serde(rename = "createParty")]
    CreateParty,
    #[serde(rename = "joinParty")]
    JoinParty { party_id: String },
    #[serde(rename = "leaveParty")]
    LeaveParty,
}

// ─────────── 服务器 → 客户端 ───────────

/// 区域广播里的单实体状态增量
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityUpdate {
    pub id: String,
    pub position: Option<Position>,
    pub hp: Option<i32>,
    pub animation: Option<String>,
    /// Mir 8 向 (0=上, 顺时针)。怪物必带; 玩家缺省由位移推导
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<u8>,
    /// true = 实体已消失（如怪物死亡），客户端移除
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed: Option<bool>,
    /// 玩家外观 (衣甲 CArmour 库号); 怪物无
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub armour: Option<u16>,
    /// 玩家外观 (武器 CWeapon 库号); 无武器/怪物 = None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weapon: Option<u16>,
    /// 怪物图库号 (Data/Monster/{image:03}.Lib); 玩家 = None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<u16>,
    /// 怪物库内外观基址 (一库多怪时选中段的起始帧; 0/缺省 = 库首)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_base: Option<u32>,
    /// 中毒状态 (施毒术 DoT; 客户端据此给精灵叠绿色染色)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poisoned: Option<bool>,
    /// 活跃状态效果名列表 (统一状态系统: "stun" 僵直, 后续 slow/root/
    /// hide/shield…); None = 本次更新不改变状态, 空列表 = 全部清除
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statuses: Option<Vec<String>>,
    /// 宠物主人 char_id (召唤物; 客户端友方染色/不可作为攻击目标)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// 显示名 (玩家角色名 / 怪物模板名 / 宠物「名 Lv N」); 客户端画名牌
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 等级 (玩家=角色级, 怪物=模板级, 宠物=宝宝级); 目标栏徽标
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u32>,
    /// 音效基址 (怪物; packs/sound/mon/{基址:03}-动作.ogg)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound: Option<u16>,
    /// 善恶名色 (玩家: "white"/"grey"/"red"); 名牌/目标栏按它染色
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pk: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    /// Hello 应答；version 为服务器协议版本
    #[serde(rename = "helloAck")]
    HelloAck { version: u32 },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "loginResult")]
    LoginResult {
        success: bool,
        account_id: Option<String>,
        message: String,
    },
    #[serde(rename = "loginSuccess")]
    LoginSuccess { player_id: String, timestamp: u64 },
    #[serde(rename = "characterList")]
    CharacterList { characters: Vec<CharacterSummary> },
    #[serde(rename = "characterCreated")]
    CharacterCreated { character_id: String, name: String },
    #[serde(rename = "enterGame")]
    EnterGame {
        character: serde_json::Value,
        inventory: serde_json::Value,
    },
    /// 重连恢复用的会话令牌（客户端保存，Resume 时回放）
    #[serde(rename = "sessionToken")]
    SessionToken { token: String },
    /// 一次性启动票据 (RequestTicket 应答; 登录器转交客户端)
    #[serde(rename = "launchTicket")]
    LaunchTicket { ticket: String },
    #[serde(rename = "resumeFailed")]
    ResumeFailed { message: String },
    #[serde(rename = "heartbeatAck")]
    HeartbeatAck,
    #[serde(rename = "stateUpdate")]
    StateUpdate {
        entities: Vec<EntityUpdate>,
        timestamp: u64,
    },
    #[serde(rename = "zoneChanged")]
    ZoneChanged {
        zone_id: String,
        zone_name: String,
        position: Position,
        /// 小地图帧号 (Data/mmap.Lib)；None = 该区未配置小地图
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimap: Option<u16>,
        /// 区域 BGM 曲名 (packs/sound/bgm/<名>.ogg); None = 无
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bgm: Option<String>,
        /// 安全区列表 (中心 x, y, 半径); 客户端进出提示用
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        safe_zones: Vec<(f64, f64, f64)>,
    },
    #[serde(rename = "playerStatus")]
    PlayerStatus {
        level: u32,
        experience: u64,
        required_experience: u64,
        hp: i32,
        max_hp: i32,
        mp: i32,
        max_mp: i32,
    },
    #[serde(rename = "chatMessage")]
    ChatMessage {
        channel: ChatChannel,
        sender: String,
        content: String,
        timestamp: u64,
    },
    #[serde(rename = "damageNumber")]
    DamageNumber {
        target_id: String,
        amount: i32,
        is_critical: bool,
    },
    /// 当前区域地面物品全量快照 (增删时下发)
    #[serde(rename = "groundItems")]
    GroundItems { items: Vec<GroundItemInfo> },
    #[serde(rename = "notification")]
    Notification {
        message: String,
        notification_type: String,
    },
    #[serde(rename = "questState")]
    QuestState { quests: Vec<QuestInfo> },
    #[serde(rename = "skillList")]
    SkillList { skills: Vec<SkillInfo> },
    #[serde(rename = "skillEffect")]
    SkillEffect {
        caster_id: String,
        skill_id: String,
        /// 机制类型 ("damage"/"aoe"/…/"charge"); 客户端按它分流表现
        /// (冲锋: 起手包起跟随拖尾, 不走通用命中特效)
        #[serde(default)]
        kind: String,
        position: Position,
        targets: Vec<String>,
        /// 施放者的技能修炼等级 (4 级起客户端播强化特效)
        #[serde(default)]
        level: u32,
        /// 特效名 (packs/magic/<名>.mfl, 通常与技能 id 同名; 纯数字 = 旧编号库)
        #[serde(default)]
        fx: String,
        #[serde(default)]
        fx_base: u32,
        #[serde(default)]
        fx_frames: u8,
        /// 施放动作 (attack/cast), 旁观客户端据此播放施放者动作
        #[serde(default)]
        anim: String,
        /// 技能类型: 1 一段(命中) / 2 二段(起手+命中) / 3 三段(起手+飞行+命中);
        /// 0 按 1 处理。段位置由客户端按经典布局自动推导 (起手=命中-10 或 -170,
        /// 飞行=命中-160 起 16 向×10 槽)
        #[serde(default)]
        stages: u8,
        /// 施放者坐标 (起手/飞行的起点; 一段技能可缺省)
        #[serde(default)]
        src: Option<Position>,
    },
    /// 本区 NPC 全量快照 (进区/切区/配置热重载时下发)
    /// 对话页 (点击 NPC 或选了跳页选项后下发)
    #[serde(rename = "npcDialog")]
    NpcDialog {
        npc_id: String,
        /// NPC 名字 (对话框标题)
        name: String,
        page: u32,
        text: String,
        options: Vec<NpcDialogOption>,
    },
    /// 对话结束 (选了「关闭」或走完动作)
    #[serde(rename = "npcDialogEnd")]
    NpcDialogEnd,
    /// 打开商店 (对话里选了「商店」动作)
    #[serde(rename = "npcShop")]
    NpcShop {
        npc_id: String,
        name: String,
        items: Vec<ShopItemInfo>,
        /// 回收价比例, 客户端据此显示"卖价"
        sell_rate: f64,
    },
    /// 金币变动
    #[serde(rename = "goldChanged")]
    GoldChanged { gold: u64 },
    /// 收到交易邀请
    #[serde(rename = "tradeInvited")]
    TradeInvited { from_id: String, from_name: String },
    /// 交易全量状态 (每次变动双方各自视角推一份)
    #[serde(rename = "tradeState")]
    TradeState {
        partner: String,
        my_items: Vec<ItemInfo>,
        their_items: Vec<ItemInfo>,
        my_gold: u64,
        their_gold: u64,
        my_ok: bool,
        their_ok: bool,
    },
    /// 交易结束 (done: true=成交 false=取消)
    #[serde(rename = "tradeClosed")]
    TradeClosed { reason: String, done: bool },
    /// 攻击模式回执 ("peace"/"all")
    #[serde(rename = "pkMode")]
    PkMode { mode: String },
    /// 仓库全量状态 (打开与每次存取后推)
    #[serde(rename = "storageState")]
    StorageState {
        npc_id: String,
        items: Vec<ItemInfo>,
        cap: u32,
    },
    #[serde(rename = "npcList")]
    NpcList { npcs: Vec<NpcInfo> },
    #[serde(rename = "inventoryState")]
    InventoryState {
        inventory: Vec<ItemInfo>,
        equipment: std::collections::HashMap<String, ItemInfo>,
    },
}

/// 商店里的一件货
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShopItemInfo {
    pub template: String,
    pub name: String,
    pub image: u16,
    pub price: u32,
    /// -1 = 无限
    pub stock: i32,
    /// 属性摘要 (攻/魔/道/防/血), 供 Tips 显示
    pub attack: i32,
    #[serde(default)]
    pub magic: i32,
    #[serde(default)]
    pub spirit: i32,
    pub defense: i32,
    pub hp: i32,
    pub slot: String,
}

/// 对话页上的一个可选项 (idx 用于回传, 客户端不关心动作)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpcDialogOption {
    pub idx: u32,
    pub label: String,
}

/// 场景 NPC（静态，站立 4 帧循环；`image` = Data/NPC/{image:02}.Lib）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NpcInfo {
    pub id: String,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub image: u16,
}

/// 一件物品（实例）。slot: weapon/armor/helmet/necklace/ring
fn default_dur() -> i32 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ItemInfo {
    pub id: String,
    pub template: String,
    pub name: String,
    pub slot: String,
    #[serde(default)]
    pub attack: i32,
    /// 魔法攻击 (法师系)
    #[serde(default)]
    pub magic: i32,
    /// 道术攻击 (道士系)
    #[serde(default)]
    pub spirit: i32,
    #[serde(default)]
    pub defense: i32,
    #[serde(default)]
    pub hp: i32,
    /// 极品附加点数合计 (>0 即极品, 客户端名字淡蓝)
    #[serde(default)]
    pub bonus: i32,
    /// 当前耐久 / 耐久上限 (上限 0 = 无耐久概念; 老存档缺省视为满)
    #[serde(default = "default_dur")]
    pub dur: i32,
    #[serde(default = "default_dur")]
    pub max_dur: i32,
    /// Items.Lib 图标帧号
    #[serde(default)]
    pub image: u16,
    /// 外观库编号 (weapon → CWeapon/{shape:02}.Lib, armor → CArmour)
    #[serde(default)]
    pub shape: u16,
}

/// 地面掉落物 (渲染 Items.Lib 图标帧)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GroundItemInfo {
    pub id: String,
    pub name: String,
    pub image: u16,
    pub x: f64,
    pub y: f64,
}

// ─────────── 载荷结构 ───────────

/// 选角界面的角色摘要
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterSummary {
    pub id: String,
    pub name: String,
    pub class: CharacterClass,
    #[serde(default = "default_gender")]
    pub gender: String,
    pub level: u32,
}

/// 技能栏/面板的单技能条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillInfo {
    pub id: String,
    pub name: String,
    pub mp_cost: i32,
    pub cooldown_ms: u64,
    pub required_level: u32,
    pub range: f64,
    /// 免目标施放（治疗/自身为圆心的 AoE）
    pub self_cast: bool,
    /// 机制类型 ("damage"/"aoe"/"heal"/"dot"/"charge"…); 客户端据此
    /// 做本地预表现 (如冲锋位移预测)
    #[serde(default)]
    pub kind: String,
    /// 修炼等级 (0 起步)
    #[serde(default)]
    pub level: u32,
    /// 修炼满级 (官设 3, 私服玩法可调 4/5)
    #[serde(default)]
    pub max_level: u32,
    /// 当前级已积累熟练度
    #[serde(default)]
    pub train: u32,
    /// 升下一级所需熟练度 (已满级为 0)
    #[serde(default)]
    pub train_need: u32,
    /// 技能图标 (packs/magicon.mfl 帧号)
    #[serde(default)]
    pub icon: u32,
    /// 施放动作: "attack" 挥砍 / 其余按施法 (双手前推) 播
    #[serde(default)]
    pub anim: String,
}

/// 任务面板的单任务条目
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestInfo {
    pub id: String,
    pub name: String,
    /// "available" | "active" | "completed"
    pub state: String,
    pub objectives: Vec<QuestObjectiveInfo>,
    pub exp_reward: u64,
    /// 金币奖励
    #[serde(default)]
    pub gold_reward: u64,
    /// 物品奖励 (名称已由服务端解析好, 客户端直接显示)
    #[serde(default)]
    pub item_rewards: Vec<QuestRewardInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestRewardInfo {
    pub name: String,
    pub count: u32,
    /// Items.Lib 图标帧号
    pub image: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestObjectiveInfo {
    /// 怪物/物品模板 id（客户端映射显示名）
    pub target_id: String,
    pub current: u32,
    pub required: u32,
}

pub fn default_gender() -> String {
    "male".into()
}

// ─────────── 编解码 ───────────

pub fn encode<T: Serialize>(msg: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(msg)
}

pub fn decode_client(json: &str) -> Result<ClientMessage, serde_json::Error> {
    serde_json::from_str(json)
}

pub fn decode_server(json: &str) -> Result<ServerMessage, serde_json::Error> {
    serde_json::from_str(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_negotiation_roundtrip() {
        let json = encode(&ClientMessage::Hello {
            version: PROTOCOL_VERSION,
        })
        .unwrap();
        assert!(json.contains("\"type\":\"hello\""));
        match decode_client(&json).unwrap() {
            ClientMessage::Hello { version } => assert_eq!(version, PROTOCOL_VERSION),
            m => panic!("期望 Hello, 得到 {m:?}"),
        }
        let ack = encode(&ServerMessage::HelloAck {
            version: PROTOCOL_VERSION,
        })
        .unwrap();
        assert!(matches!(
            decode_server(&ack).unwrap(),
            ServerMessage::HelloAck { version: 1 }
        ));
    }

    #[test]
    fn wire_format_matches_legacy() {
        // 与旧客户端线格式一致: type tag + 蛇形字段
        let json = r#"{"type":"move","direction":{"x":1.0,"y":0.0}}"#;
        match decode_client(json).unwrap() {
            ClientMessage::Move { direction } => {
                assert_eq!(direction, Position { x: 1.0, y: 0.0 });
            }
            m => panic!("期望 Move, 得到 {m:?}"),
        }
        let json = r#"{"type":"resume","token":"t1","character_id":"c1"}"#;
        assert!(matches!(
            decode_client(json).unwrap(),
            ClientMessage::Resume { .. }
        ));
    }

    #[test]
    fn server_message_roundtrip() {
        let msg = ServerMessage::StateUpdate {
            entities: vec![EntityUpdate {
                id: "p1".into(),
                position: Some(Position { x: 330.5, y: 150.5 }),
                hp: Some(100),
                animation: Some("walk".into()),
                dir: None,
                removed: None,
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
            }],
            timestamp: 12345,
        };
        let json = encode(&msg).unwrap();
        // removed/image=None 不出现在线上 (逐字段 skip_serializing_if)
        assert!(!json.contains("removed"));
        assert!(!json.contains("image"));
        match decode_server(&json).unwrap() {
            ServerMessage::StateUpdate { entities, .. } => {
                assert_eq!(entities.len(), 1);
                assert_eq!(entities[0].id, "p1");
            }
            m => panic!("期望 StateUpdate, 得到 {m:?}"),
        }
    }

    #[test]
    fn character_create_defaults_gender() {
        let json = r#"{"type":"createCharacter","name":"侠客","class":"Warrior"}"#;
        match decode_client(json).unwrap() {
            ClientMessage::CreateCharacter { gender, class, .. } => {
                assert_eq!(gender, "male");
                assert_eq!(class, CharacterClass::Warrior);
            }
            m => panic!("期望 CreateCharacter, 得到 {m:?}"),
        }
    }

    #[test]
    fn unknown_message_is_error_not_panic() {
        assert!(decode_client(r#"{"type":"notAThing"}"#).is_err());
        assert!(decode_client("garbage").is_err());
    }
}
