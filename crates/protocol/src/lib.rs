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
    Register { username: String, password: String },
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
        position: Position,
        targets: Vec<String>,
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
    /// 属性摘要 (攻/防/血), 供 Tips 显示
    pub attack: i32,
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ItemInfo {
    pub id: String,
    pub template: String,
    pub name: String,
    pub slot: String,
    #[serde(default)]
    pub attack: i32,
    #[serde(default)]
    pub defense: i32,
    #[serde(default)]
    pub hp: i32,
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
