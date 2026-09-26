//! 方向 A「鎏金经典」HUD —— 逐参数对齐 design/HUD.dc.html 定稿。
//! 布局: 底部中央动作条 (92px 圆形头像+等级胶囊居中高出两翼, 左翼 HP 条+技能格
//! 1234, 右翼 MP 条+QWER), EXP 细条横贯屏幕底边, 右上 208 方形小地图 + 其下
//! 任务追踪, 左下玻璃聊天框, 右下七键功能组, 通知堆栈。

use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use bevy::ui::widget::NodeImageMode;
use bevy::ui::RelativeCursorPosition;
use bevy::window::Ime;

use crate::panels::{Panel, PanelKind};
use crate::Net;
use protocol::{ChatChannel, ClientMessage};

// ── 设计令牌 (design 定稿色板) ──
pub const GOLD: Color = Color::srgb(0.788, 0.647, 0.361); // #c9a55c
pub const GOLD_BRIGHT: Color = Color::srgb(1.0, 0.847, 0.463); // #ffd876
pub const EDGE_GOLD: Color = Color::srgb(0.420, 0.353, 0.196); // #6b5a32
pub const EDGE_DARK: Color = Color::srgb(0.165, 0.176, 0.235); // #2a2d3c
pub const TEXT_MAIN: Color = Color::srgb(0.910, 0.886, 0.816); // #e8e2d0
pub const TEXT_SUB: Color = Color::srgb(0.812, 0.784, 0.706); // #cfc8b4
pub const TEXT_DIM: Color = Color::srgb(0.604, 0.569, 0.486); // #9a917c
pub const DISABLED: Color = Color::srgb(0.357, 0.337, 0.282); // #5b5648
pub const HP_RED: Color = Color::srgb(0.878, 0.251, 0.251); // #e04040
pub const EXP_GOLD: Color = Color::srgb(0.933, 0.804, 0.322); // #eecd52
pub const LOOT_GREEN: Color = Color::srgb(0.482, 0.847, 0.561); // #7bd88f
/// 槽底 rgba(13,15,24,0.7) / 弱 0.55
const SLOT_BG: Color = Color::srgba(0.051, 0.059, 0.094, 0.70);
const SLOT_BG_WEAK: Color = Color::srgba(0.051, 0.059, 0.094, 0.55);
const BAR_BG: Color = Color::srgb(0.055, 0.063, 0.090); // #0e1017
const GLASS_BG: Color = Color::srgba(0.039, 0.043, 0.071, 0.55); // rgba(10,11,18,.55)
const TRACK_BG: Color = Color::srgba(0.051, 0.059, 0.094, 0.55); // rgba(13,15,24,.55)

/// 皮肤句柄 + 中文字体 (系统字体运行时加载, 不入库)
#[derive(Resource)]
pub struct Skin {
    pub font: Handle<Font>,
    pub bar_hp: Handle<Image>,
    pub bar_mp: Handle<Image>,
}

pub fn sliced(border: f32) -> NodeImageMode {
    NodeImageMode::Sliced(TextureSlicer {
        border: BorderRect::square(border),
        ..default()
    })
}

pub fn load_skin(
    mut commands: Commands,
    assets: Res<AssetServer>,
    mut fonts: ResMut<Assets<Font>>,
) {
    // 系统中文字体 (simhei 是普通 ttf, Bevy 可直载; msyh.ttc 不支持)
    let font = [
        "C:/Windows/Fonts/simhei.ttf",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.otf",
    ]
    .iter()
    .find_map(|p| {
        let bytes = std::fs::read(p).ok()?;
        Font::try_from_bytes(bytes).ok()
    })
    .map(|f| fonts.add(f))
    .unwrap_or_default();
    commands.insert_resource(Skin {
        font,
        bar_hp: assets.load("ui/skin/bar_fill_hp.png"),
        bar_mp: assets.load("ui/skin/bar_fill_mp.png"),
    });
}

// ── 标记组件 ──
#[derive(Component)]
pub struct HudRoot;
#[derive(Component)]
pub struct HpFill;
#[derive(Component)]
pub struct MpFill;
#[derive(Component)]
pub struct ExpFill;
#[derive(Component)]
pub struct HpText;
#[derive(Component)]
pub struct MpText;
#[derive(Component)]
pub struct LvText;
#[derive(Component)]
pub struct SkillNameText(usize);
#[derive(Component)]
pub struct SkillMask(usize);
#[derive(Component)]
pub struct NoticeLine(usize);
#[derive(Component)]
pub struct ZoneNameText;
#[derive(Component)]
pub struct CoordText;
#[derive(Component)]
pub struct TargetFrame;
#[derive(Component)]
pub struct TargetLevel;
#[derive(Component)]
pub struct TargetAvatar;
#[derive(Component)]
pub struct TargetTradeBtn;
#[derive(Component)]
pub struct TargetPartyBtn;
#[derive(Component)]
pub struct TargetTradeClick;
#[derive(Component)]
pub struct PkModeText;
#[derive(Component)]
pub struct TargetName;
#[derive(Component)]
pub struct TargetHpFill;
#[derive(Component)]
pub struct TargetHpText;
#[derive(Component)]
pub struct MiniDot(usize);
#[derive(Component)]
pub struct MiniMapImg;
#[derive(Component)]
pub struct PlayerDot;
#[derive(Component)]
pub struct TrackerBody;
#[derive(Component)]
pub struct ChatBody;
#[derive(Component)]
pub struct ChatInputText;

/// 聊天输入态: active 时键盘由聊天独占 (toggle/技能/缩放键均应跳过)
#[derive(Resource, Default)]
pub struct ChatState {
    pub active: bool,
    pub buffer: String,
}
/// 右下功能键 (点击开关对应面板; None = 未实现灰态)
#[derive(Component)]
pub struct MenuBtn(pub Option<PanelKind>);

fn text(font: &Handle<Font>, s: impl Into<String>, size: f32, color: Color) -> impl Bundle {
    (
        Text::new(s),
        TextFont {
            font: font.clone(),
            font_size: size,
            ..default()
        },
        TextColor(color),
    )
}

/// 52px 技能格 (设计稿: 暗底 1px 边 4px 圆角, 键位数字左上角)
fn spawn_skill_slot(
    parent: &mut ChildBuilder,
    skin: &Skin,
    idx: Option<usize>,
    key: &str,
    icon: Option<Handle<Image>>,
) {
    let active = idx.is_some();
    parent
        .spawn((
            Node {
                width: Val::Px(52.0),
                height: Val::Px(52.0),
                border: UiRect::all(Val::Px(1.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(if active { SLOT_BG } else { SLOT_BG_WEAK }),
            BorderColor(if active { EDGE_GOLD } else { EDGE_DARK }),
            BorderRadius::all(Val::Px(4.0)),
        ))
        .with_children(|s| {
            // 键位角标
            s.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(2.0),
                    left: Val::Px(5.0),
                    ..default()
                },
                Text::new(key),
                TextFont {
                    font: skin.font.clone(),
                    font_size: 9.0,
                    ..default()
                },
                TextColor(if active { TEXT_DIM } else { DISABLED }),
            ));
            if let Some(i) = idx {
                if let Some(icon) = icon {
                    // MagIcon 图标 (格内 40×40)
                    s.spawn((
                        Node {
                            width: Val::Px(40.0),
                            height: Val::Px(40.0),
                            ..default()
                        },
                        ImageNode::new(icon),
                    ));
                } else {
                    // 技能名 (双字, 无图标时的占位)
                    s.spawn((text(&skin.font, "", 14.0, TEXT_MAIN), SkillNameText(i)));
                }
                // 冷却遮罩: 自顶向下按剩余比例覆盖
                s.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        top: Val::Px(0.0),
                        left: Val::Px(0.0),
                        right: Val::Px(0.0),
                        height: Val::Percent(0.0),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
                    SkillMask(i),
                ));
            }
        });
}

/// HP/MP 行: 标签在外侧, 280×14 圆角条 (烘焙渐变填充), 数值居中叠在条上
fn spawn_stat_bar(
    parent: &mut ChildBuilder,
    skin: &Skin,
    label: &str,
    fill: Handle<Image>,
    fill_marker: impl Component,
    text_marker: impl Component,
    label_first: bool,
) {
    parent
        .spawn(Node {
            align_items: AlignItems::Center,
            column_gap: Val::Px(8.0),
            ..default()
        })
        .with_children(|row| {
            if label_first {
                row.spawn(text(&skin.font, label, 10.0, TEXT_DIM));
            }
            row.spawn((
                Node {
                    width: Val::Px(280.0),
                    height: Val::Px(14.0),
                    border: UiRect::all(Val::Px(1.0)),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(BAR_BG),
                BorderColor(GOLD),
                BorderRadius::all(Val::Px(7.0)),
            ))
            .with_children(|b| {
                b.spawn((
                    Node {
                        height: Val::Percent(100.0),
                        width: Val::Percent(100.0),
                        ..default()
                    },
                    ImageNode::new(fill).with_mode(sliced(4.0)),
                    BorderRadius::all(Val::Px(6.0)),
                    fill_marker,
                ));
                // 数值居中叠在条上
                b.spawn((Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    right: Val::Px(0.0),
                    top: Val::Px(0.0),
                    bottom: Val::Px(0.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },))
                    .with_children(|overlay| {
                        overlay.spawn((text(&skin.font, "", 11.0, TEXT_MAIN), text_marker));
                    });
            });
            if !label_first {
                row.spawn(text(&skin.font, label, 10.0, TEXT_DIM));
            }
        });
}

/// 进入游戏时构建 HUD (全部绝对定位, 对齐设计稿)
/// 技能图标缓存 (与 net.skills 同序), HUD 技能格与技能面板共用
#[derive(Resource, Default)]
pub struct SkillIcons(pub Vec<Option<Handle<Image>>>);

/// 按技能表顺序解码图标帧 → 独立 Image
fn load_skill_icons(
    net: &Net,
    data_dir: Option<&std::path::Path>,
    images: &mut Assets<Image>,
) -> Vec<Option<Handle<Image>>> {
    // Crystal 路线已废除: 技能图标只走 packs/magicon.mfl
    let _ = data_dir;
    let packs = std::env::var("MIRFORGE_PACKS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("packs"));
    let lib = mir_formats::mfl::AnyLib::open(&packs.join("magicon.mfl")).ok();
    net.skills
        .iter()
        .map(|s| {
            let img = lib.as_ref()?.image(s.icon as usize).ok().flatten()?;
            Some(images.add(Image::new(
                bevy::render::render_resource::Extent3d {
                    width: img.width as u32,
                    height: img.height as u32,
                    depth_or_array_layers: 1,
                },
                bevy::render::render_resource::TextureDimension::D2,
                img.rgba,
                bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
                bevy::asset::RenderAssetUsages::RENDER_WORLD,
            )))
        })
        .collect()
}

pub fn setup(
    mut commands: Commands,
    skin: Res<Skin>,
    net: Res<Net>,
    world: Res<crate::World>,
    mut images: ResMut<Assets<Image>>,
) {
    let icons = load_skill_icons(&net, Some(world.data_root.as_path()), &mut images);
    commands.insert_resource(SkillIcons(icons.clone()));
    // ── 底部中央动作条 (透明背景, gap 22) ──
    commands
        .spawn((
            HudRoot,
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(18.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::FlexEnd,
                column_gap: Val::Px(22.0),
                ..default()
            },
        ))
        .with_children(|root| {
            // 左翼: HP 行 + 技能格 1234
            root.spawn((
                crate::panels::UiBlock,
                RelativeCursorPosition::default(),
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(10.0),
                    align_items: AlignItems::FlexEnd,
                    ..default()
                },
            ))
            .with_children(|wing| {
                spawn_stat_bar(wing, &skin, "HP", skin.bar_hp.clone(), HpFill, HpText, true);
                wing.spawn(Node {
                    column_gap: Val::Px(8.0),
                    ..default()
                })
                .with_children(|row| {
                    for (i, key) in ["1", "2", "3", "4", "5"].iter().enumerate() {
                        let idx = (i < net.skills.len().clamp(3, 5)).then_some(i);
                        let icon = icons.get(i).cloned().flatten();
                        spawn_skill_slot(row, &skin, if i < 3 { idx } else { None }, key, icon);
                    }
                });
            });
            // 中央: 92px 圆形头像 + 等级胶囊 (高出两翼 14px)
            root.spawn((
                crate::panels::UiBlock,
                RelativeCursorPosition::default(),
                Node {
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    margin: UiRect::bottom(Val::Px(14.0)),
                    ..default()
                },
            ))
            .with_children(|center| {
                center
                    .spawn((
                        Node {
                            width: Val::Px(92.0),
                            height: Val::Px(92.0),
                            border: UiRect::all(Val::Px(2.0)),
                            justify_content: JustifyContent::Center,
                            align_items: AlignItems::Center,
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.114, 0.122, 0.180)),
                        BorderColor(GOLD),
                        BorderRadius::all(Val::Percent(50.0)),
                    ))
                    .with_children(|av| {
                        // 职业图标素材接入前: 「战」字占位
                        av.spawn(text(&skin.font, "战", 32.0, GOLD_BRIGHT));
                    });
                center
                    .spawn((
                        Node {
                            margin: UiRect::top(Val::Px(-12.0)),
                            padding: UiRect::axes(Val::Px(12.0), Val::Px(1.0)),
                            border: UiRect::all(Val::Px(1.0)),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.141, 0.114, 0.063)), // #241d10
                        BorderColor(GOLD),
                        BorderRadius::all(Val::Px(10.0)),
                    ))
                    .with_children(|b| {
                        b.spawn((text(&skin.font, "1", 11.0, GOLD_BRIGHT), LvText));
                    });
            });
            // 右翼: MP 行 + QWER 空格位
            root.spawn((
                crate::panels::UiBlock,
                RelativeCursorPosition::default(),
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(10.0),
                    align_items: AlignItems::FlexStart,
                    ..default()
                },
            ))
            .with_children(|wing| {
                spawn_stat_bar(
                    wing,
                    &skin,
                    "MP",
                    skin.bar_mp.clone(),
                    MpFill,
                    MpText,
                    false,
                );
                wing.spawn(Node {
                    column_gap: Val::Px(8.0),
                    ..default()
                })
                .with_children(|row| {
                    for key in ["Q", "W", "E", "R", "T"] {
                        spawn_skill_slot(row, &skin, None, key, None);
                    }
                });
            });
        });

    // ── EXP 细条横贯底边 (5px) ──
    commands
        .spawn((
            HudRoot,
            crate::panels::UiBlock,
            RelativeCursorPosition::default(),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                bottom: Val::Px(0.0),
                height: Val::Px(5.0),
                ..default()
            },
            BackgroundColor(BAR_BG),
        ))
        .with_children(|bar| {
            bar.spawn((
                Node {
                    height: Val::Percent(100.0),
                    width: Val::Percent(0.0),
                    ..default()
                },
                BackgroundColor(EXP_GOLD),
                ExpFill,
            ));
        });

    // ── 右上: 208×208 方形小地图 (1px 金边, 区域名/坐标叠图上) ──
    commands
        .spawn((
            HudRoot,
            crate::panels::UiBlock,
            RelativeCursorPosition::default(),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(20.0),
                right: Val::Px(20.0),
                width: Val::Px(208.0),
                height: Val::Px(208.0),
                border: UiRect::all(Val::Px(1.0)),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(Color::srgb(0.071, 0.078, 0.129)),
            BorderColor(GOLD),
        ))
        .with_children(|map| {
            // 原版小地图图层 (mmap.Lib 帧, 以玩家为中心裁剪窗口; 无帧映射时隐藏)
            map.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    top: Val::Px(0.0),
                    width: Val::Px(206.0),
                    height: Val::Px(206.0),
                    ..default()
                },
                ImageNode::default(),
                Visibility::Hidden,
                MiniMapImg,
            ));
            // 玩家点 (金色)
            map.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(101.0),
                    top: Val::Px(101.0),
                    width: Val::Px(6.0),
                    height: Val::Px(6.0),
                    ..default()
                },
                BackgroundColor(GOLD_BRIGHT),
                BorderRadius::all(Val::Percent(50.0)),
                PlayerDot,
            ));
            // 怪物点池
            for i in 0..24 {
                map.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        width: Val::Px(4.0),
                        height: Val::Px(4.0),
                        display: Display::None,
                        ..default()
                    },
                    BackgroundColor(HP_RED),
                    BorderRadius::all(Val::Percent(50.0)),
                    MiniDot(i),
                ));
            }
            map.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(8.0),
                    left: Val::Px(10.0),
                    ..default()
                },
                Text::new(""),
                TextFont {
                    font: skin.font.clone(),
                    font_size: 12.0,
                    ..default()
                },
                TextColor(TEXT_MAIN),
                ZoneNameText,
            ));
            map.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    bottom: Val::Px(6.0),
                    right: Val::Px(10.0),
                    ..default()
                },
                Text::new(""),
                TextFont {
                    font: skin.font.clone(),
                    font_size: 11.0,
                    ..default()
                },
                TextColor(TEXT_DIM),
                CoordText,
            ));
        });

    // ── 小地图下: 任务追踪 ──
    commands
        .spawn((
            HudRoot,
            crate::panels::UiBlock,
            RelativeCursorPosition::default(),
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(246.0),
                right: Val::Px(20.0),
                width: Val::Px(208.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                ..default()
            },
        ))
        .with_children(|track| {
            track
                .spawn(Node {
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(8.0),
                    ..default()
                })
                .with_children(|title| {
                    title.spawn(text(&skin.font, "任 务 追 踪", 12.0, GOLD));
                    title.spawn((
                        Node {
                            flex_grow: 1.0,
                            height: Val::Px(1.0),
                            ..default()
                        },
                        BackgroundColor(EDGE_GOLD),
                    ));
                });
            track.spawn((
                TrackerBody,
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(10.0),
                    ..default()
                },
            ));
        });

    // ── 左下: 玻璃聊天框 (内容 + 输入行占位) ──
    commands
        .spawn((
            HudRoot,
            crate::panels::UiBlock,
            RelativeCursorPosition::default(),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(20.0),
                bottom: Val::Px(18.0), // 与中央动作条同底, 一起贴齐窗口底部
                width: Val::Px(400.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                ..default()
            },
        ))
        .with_children(|chat| {
            chat.spawn((
                ChatBody,
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(5.0),
                    padding: UiRect::axes(Val::Px(14.0), Val::Px(12.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    min_height: Val::Px(96.0),
                    ..default()
                },
                BackgroundColor(GLASS_BG),
                BorderColor(Color::srgba(0.420, 0.353, 0.196, 0.35)),
                BorderRadius::all(Val::Px(4.0)),
            ));
            chat.spawn((
                Node {
                    height: Val::Px(38.0),
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(8.0),
                    padding: UiRect::horizontal(Val::Px(12.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(GLASS_BG),
                BorderColor(Color::srgba(0.420, 0.353, 0.196, 0.35)),
                BorderRadius::all(Val::Px(4.0)),
            ))
            .with_children(|input| {
                input.spawn(text(&skin.font, "世界", 12.0, GOLD));
                input.spawn((
                    text(&skin.font, "按 Enter 聊天", 13.0, DISABLED),
                    ChatInputText,
                ));
            });
        });

    // ── 右下: 功能按钮组 (B C K L F G P) ──
    commands
        .spawn((
            HudRoot,
            crate::panels::UiBlock,
            RelativeCursorPosition::default(),
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(20.0),
                bottom: Val::Px(18.0), // 与中央动作条同底, 一起贴齐窗口底部
                column_gap: Val::Px(8.0),
                ..default()
            },
        ))
        .with_children(|menu| {
            let buttons: [(&str, &str, Option<PanelKind>); 7] = [
                ("包", "B", Some(PanelKind::Bag)),
                ("角", "C", Some(PanelKind::Character)),
                ("技", "K", Some(PanelKind::Skill)),
                ("务", "L", Some(PanelKind::Quest)),
                ("友", "F", None),
                ("会", "G", None),
                ("队", "P", None),
            ];
            for (icon, key, kind) in buttons {
                let enabled = kind.is_some();
                menu.spawn((
                    Button,
                    MenuBtn(kind),
                    Node {
                        width: Val::Px(44.0),
                        height: Val::Px(44.0),
                        flex_direction: FlexDirection::Column,
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        row_gap: Val::Px(2.0),
                        border: UiRect::all(Val::Px(1.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.051, 0.059, 0.094, 0.6)),
                    BorderColor(EDGE_DARK),
                    BorderRadius::all(Val::Px(4.0)),
                ))
                .with_children(|b| {
                    b.spawn(text(
                        &skin.font,
                        icon,
                        15.0,
                        if enabled { GOLD } else { DISABLED },
                    ));
                    b.spawn(text(
                        &skin.font,
                        key,
                        9.0,
                        if enabled { TEXT_DIM } else { DISABLED },
                    ));
                });
            }
        });

    // ── 通知堆栈 (小地图左侧) ──
    commands
        .spawn((
            HudRoot,
            crate::panels::UiBlock,
            RelativeCursorPosition::default(),
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(248.0),
                top: Val::Px(24.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                row_gap: Val::Px(4.0),
                ..default()
            },
        ))
        .with_children(|col| {
            for i in 0..6 {
                col.spawn((text(&skin.font, "", 15.0, TEXT_MAIN), NoticeLine(i)));
            }
        });

    // ── 右上: 攻击模式标记 (小地图下方; Ctrl+H 切换) ──
    commands
        .spawn((
            HudRoot,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(234.0),
                right: Val::Px(20.0),
                width: Val::Px(208.0),
                justify_content: JustifyContent::Center,
                ..default()
            },
        ))
        .with_children(|root| {
            root.spawn((text(&skin.font, "和平模式 (Ctrl+H)", 11.0, TEXT_DIM), PkModeText));
        });

    // ── 顶部中央: 锁定目标栏 (异形: 左圆形头像位, 右上等级+名字, 右下血条) ──
    commands
        .spawn((
            HudRoot,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(12.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                justify_content: JustifyContent::Center,
                ..default()
            },
        ))
        .with_children(|root| {
            root.spawn((
                TargetFrame,
                // 挡住穿透: 点交易按钮不该同时当作点地面 (清锁定/走路)
                crate::panels::UiBlock,
                RelativeCursorPosition::default(),
                Node {
                    height: Val::Px(56.0),
                    align_items: AlignItems::Center,
                    display: Display::None,
                    ..default()
                },
            ))
            .with_children(|f| {
                // 右侧信息块: 左端缩进给圆形头像叠位, 右端收成半圆
                f.spawn((
                    Node {
                        margin: UiRect::left(Val::Px(28.0)),
                        padding: UiRect::new(
                            Val::Px(38.0),
                            Val::Px(22.0),
                            Val::Px(7.0),
                            Val::Px(7.0),
                        ),
                        flex_direction: FlexDirection::Column,
                        justify_content: JustifyContent::Center,
                        row_gap: Val::Px(4.0),
                        ..default()
                    },
                    BackgroundColor(GLASS_BG),
                    BorderRadius {
                        top_left: Val::Px(6.0),
                        bottom_left: Val::Px(6.0),
                        top_right: Val::Px(21.0),
                        bottom_right: Val::Px(21.0),
                    },
                ))
                .with_children(|info| {
                    // 上: 等级徽标 + 名字
                    info.spawn(Node {
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(7.0),
                        ..default()
                    })
                    .with_children(|row| {
                        row.spawn((text(&skin.font, "", 11.0, GOLD_BRIGHT), TargetLevel));
                        row.spawn((text(&skin.font, "", 14.0, TEXT_MAIN), TargetName));
                    });
                    // 下: 血条 (无外框, 数值居中叠条上)
                    info.spawn((
                        Node {
                            width: Val::Px(190.0),
                            height: Val::Px(12.0),
                            overflow: Overflow::clip(),
                            ..default()
                        },
                        BackgroundColor(BAR_BG),
                        BorderRadius::all(Val::Px(6.0)),
                    ))
                    .with_children(|b| {
                        b.spawn((
                            Node {
                                height: Val::Percent(100.0),
                                width: Val::Percent(100.0),
                                ..default()
                            },
                            ImageNode::new(skin.bar_hp.clone()).with_mode(sliced(4.0)),
                            BorderRadius::all(Val::Px(6.0)),
                            TargetHpFill,
                        ));
                        b.spawn((Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(0.0),
                            right: Val::Px(0.0),
                            top: Val::Px(0.0),
                            bottom: Val::Px(0.0),
                            justify_content: JustifyContent::Center,
                            align_items: AlignItems::Center,
                            ..default()
                        },))
                            .with_children(|overlay| {
                                overlay.spawn((
                                    text(&skin.font, "", 10.0, TEXT_MAIN),
                                    TargetHpText,
                                ));
                            });
                    });
                    // 交易/组队按钮行: 仅锁定玩家时显示
                    info.spawn((
                        TargetTradeBtn, // 行整体显隐用交易标记 (历史沿用)
                        Node {
                            display: Display::None,
                            align_self: AlignSelf::Center,
                            column_gap: Val::Px(8.0),
                            ..default()
                        },
                    ))
                    .with_children(|row| {
                        for (label, party) in [("交易", false), ("组队", true)] {
                            let mut b = row.spawn((
                                Button,
                                Node {
                                    padding: UiRect::axes(Val::Px(10.0), Val::Px(1.0)),
                                    border: UiRect::all(Val::Px(1.0)),
                                    ..default()
                                },
                                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
                                BorderColor(EDGE_GOLD),
                                BorderRadius::all(Val::Px(8.0)),
                            ));
                            if party {
                                b.insert(TargetPartyBtn);
                            } else {
                                b.insert(TargetTradeClick);
                            }
                            b.with_children(|t| {
                                t.spawn(text(&skin.font, label, 10.0, GOLD));
                            });
                        }
                    });
                });
                // 左: 圆形头像位 (叠在信息块左端; 头像素材接入前留空)
                f.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(0.0),
                        width: Val::Px(56.0),
                        height: Val::Px(56.0),
                        border: UiRect::all(Val::Px(2.0)),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.114, 0.122, 0.180)),
                    BorderColor(GOLD),
                    BorderRadius::all(Val::Percent(50.0)),
                    TargetAvatar,
                ));
            });
        });
}

pub fn teardown(mut commands: Commands, q: Query<Entity, With<HudRoot>>) {
    for e in &q {
        commands.entity(e).despawn_recursive();
    }
}

/// 功能键点击 → 开关对应面板
/// 聊天输入: Enter 开启/发送, Esc 取消, 字符/退格编辑, IME 中文提交
#[allow(clippy::type_complexity)]
pub fn chat_input(
    mut chat: ResMut<ChatState>,
    net: Res<Net>,
    mut keys: EventReader<KeyboardInput>,
    mut ime: EventReader<Ime>,
    mut windows: Query<&mut Window>,
    mut q_line: Query<(&mut Text, &mut TextColor), With<ChatInputText>>,
) {
    for ev in keys.read() {
        if !ev.state.is_pressed() {
            continue;
        }
        match &ev.logical_key {
            Key::Enter => {
                if chat.active {
                    let content = chat.buffer.trim().to_string();
                    if !content.is_empty() {
                        net.send(ClientMessage::Chat {
                            channel: ChatChannel::World,
                            content,
                            target_name: None,
                        });
                    }
                    chat.active = false;
                    chat.buffer.clear();
                } else {
                    chat.active = true;
                    chat.buffer.clear();
                }
            }
            Key::Escape if chat.active => {
                chat.active = false;
                chat.buffer.clear();
            }
            Key::Backspace if chat.active => {
                chat.buffer.pop();
            }
            Key::Space if chat.active && chat.buffer.chars().count() < 120 => {
                chat.buffer.push(' ');
            }
            Key::Character(s) if chat.active => {
                for c in s.chars().filter(|c| !c.is_control()) {
                    if chat.buffer.chars().count() < 120 {
                        chat.buffer.push(c);
                    }
                }
            }
            _ => {}
        }
    }
    // 中文输入法整段提交
    for ev in ime.read() {
        if let Ime::Commit { value, .. } = ev {
            if chat.active {
                for c in value.chars() {
                    if chat.buffer.chars().count() < 120 {
                        chat.buffer.push(c);
                    }
                }
            }
        }
    }
    if let Ok(mut w) = windows.get_single_mut() {
        if w.ime_enabled != chat.active {
            w.ime_enabled = chat.active;
        }
    }
    for (mut t, mut color) in q_line.iter_mut() {
        if chat.active {
            **t = format!("{}_", chat.buffer);
            color.0 = TEXT_MAIN;
        } else {
            **t = "按 Enter 聊天".into();
            color.0 = DISABLED;
        }
    }
}

pub fn menu_clicks(
    q_btn: Query<(&Interaction, &MenuBtn), Changed<Interaction>>,
    mut q_panel: Query<(&Panel, &mut Node)>,
) {
    for (it, btn) in &q_btn {
        if *it != Interaction::Pressed {
            continue;
        }
        let Some(kind) = btn.0 else { continue };
        for (p, mut node) in q_panel.iter_mut() {
            if p.0 == kind {
                node.display = if node.display == Display::None {
                    Display::Flex
                } else {
                    Display::None
                };
            }
        }
    }
}

/// 数据 → HUD (每帧: 条/文本/冷却/小地图; 任务追踪与聊天按 rev 重建)
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn update(
    mut commands: Commands,
    time: Res<Time>,
    mut net: ResMut<Net>,
    skin: Res<Skin>,
    q_player: Query<&crate::Player>,
    remotes: Res<crate::Remotes>,
    world: Res<crate::World>,
    minimap: Res<crate::MiniMap>,
    mut q_mmimg: Query<(&mut ImageNode, &mut Visibility), With<MiniMapImg>>,
    mut q_fill: Query<
        (
            &mut Node,
            Option<&HpFill>,
            Option<&MpFill>,
            Option<&ExpFill>,
            Option<&SkillMask>,
            Option<&MiniDot>,
            Option<&PlayerDot>,
        ),
        Or<(
            With<HpFill>,
            With<MpFill>,
            With<ExpFill>,
            With<SkillMask>,
            With<MiniDot>,
            With<PlayerDot>,
        )>,
    >,
    mut q_texts: Query<(
        &mut Text,
        &mut TextColor,
        Option<&HpText>,
        Option<&MpText>,
        Option<&LvText>,
        Option<&SkillNameText>,
        Option<&NoticeLine>,
        Option<&ZoneNameText>,
        Option<&CoordText>,
    )>,
    q_tracker: Query<Entity, With<TrackerBody>>,
    q_chat: Query<Entity, With<ChatBody>>,
    mut last_rev: Local<(u32, u32)>,
) {
    let now = time.elapsed_secs_f64();
    net.notices.retain(|(_, _, born)| now - born < 4.0);
    let stat = net.stat;
    let player_pos = q_player.get_single().map(|p| p.pos).ok();

    // 小地图: 有原版图帧 → 以玩家为中心裁剪窗口, 实体点用图像素坐标系
    // (Crystal 语义: 像素位 = 格坐标 × 图尺寸/地图格数); 无图帧 → 2px/格相对点阵
    const PANE: f32 = 206.0;
    let mut player_dot = (101.0, 101.0);
    let mut dots: Vec<(f32, f32)> = Vec::new();
    let mut window = None; // (x0, y0, sx, sy) 有图时的裁剪窗口与比例
    if let (Some((handle, size)), Some(pp)) = (&minimap.image, player_pos) {
        let sx = size.x / world.map.width.max(1) as f32;
        let sy = size.y / world.map.height.max(1) as f32;
        let (px, py) = (pp.x as f32 * sx, pp.y as f32 * sy);
        let x0 = (px - PANE / 2.0).clamp(0.0, (size.x - PANE).max(0.0));
        let y0 = (py - PANE / 2.0).clamp(0.0, (size.y - PANE).max(0.0));
        for (mut img, mut vis) in q_mmimg.iter_mut() {
            img.image = handle.clone();
            img.rect = Some(Rect::new(
                x0,
                y0,
                (x0 + PANE).min(size.x),
                (y0 + PANE).min(size.y),
            ));
            *vis = Visibility::Inherited;
        }
        player_dot = (px - x0 - 3.0, py - y0 - 3.0);
        window = Some((x0, y0, sx, sy));
    } else {
        for (_, mut vis) in q_mmimg.iter_mut() {
            *vis = Visibility::Hidden;
        }
    }
    if let Some(pp) = player_pos {
        for r in remotes.0.values() {
            if r.image.is_none() || r.anim == 4 {
                continue;
            }
            match window {
                Some((x0, y0, sx, sy)) => {
                    let (mx, my) = (r.pos.x as f32 * sx - x0, r.pos.y as f32 * sy - y0);
                    if (0.0..PANE).contains(&mx) && (0.0..PANE).contains(&my) {
                        dots.push((mx - 2.0, my - 2.0));
                    }
                }
                None => {
                    let (dx, dy) = ((r.pos.x - pp.x) * 2.0, (r.pos.y - pp.y) * 2.0);
                    if dx.abs() < 100.0 && dy.abs() < 100.0 {
                        dots.push((104.0 + dx as f32, 104.0 + dy as f32));
                    }
                }
            }
        }
    }

    for (mut node, hp, mp, exp, mask, dot, pdot) in q_fill.iter_mut() {
        if pdot.is_some() {
            node.left = Val::Px(player_dot.0);
            node.top = Val::Px(player_dot.1);
            continue;
        }
        if hp.is_some() {
            if let Some(s) = stat {
                node.width =
                    Val::Percent(100.0 * (s.hp.max(0) as f32 / s.max_hp.max(1) as f32).min(1.0));
            }
        } else if mp.is_some() {
            if let Some(s) = stat {
                node.width =
                    Val::Percent(100.0 * (s.mp.max(0) as f32 / s.max_mp.max(1) as f32).min(1.0));
            }
        } else if exp.is_some() {
            if let Some(s) = stat {
                node.width = Val::Percent(100.0 * (s.exp as f32 / s.req.max(1) as f32).min(1.0));
            }
        } else if let Some(SkillMask(i)) = mask {
            let frac = net
                .skills
                .get(*i)
                .and_then(|s| net.cds.get(&s.id).map(|&t| (t - now, s.cooldown_ms)))
                .map(|(remain, total)| (remain / (total as f64 / 1000.0)).clamp(0.0, 1.0))
                .unwrap_or(0.0);
            node.height = Val::Percent(100.0 * frac as f32);
        } else if let Some(MiniDot(i)) = dot {
            match dots.get(*i) {
                Some((x, y)) => {
                    node.display = Display::Flex;
                    node.left = Val::Px(*x);
                    node.top = Val::Px(*y);
                }
                None => node.display = Display::None,
            }
        }
    }

    for (mut t, mut color, hp, mp, lv, sname, notice, zone, coord) in q_texts.iter_mut() {
        if hp.is_some() {
            if let Some(s) = stat {
                **t = format!("{}/{}", s.hp, s.max_hp);
            }
        } else if mp.is_some() {
            if let Some(s) = stat {
                **t = format!("{}/{}", s.mp, s.max_mp);
            }
        } else if lv.is_some() {
            if let Some(s) = stat {
                **t = format!("{}", s.level);
            }
        } else if let Some(SkillNameText(i)) = sname {
            **t = net
                .skills
                .get(*i)
                .map(|s| s.name.chars().take(2).collect())
                .unwrap_or_default();
        } else if zone.is_some() {
            **t = net.zone_name.clone();
        } else if coord.is_some() {
            if let Some(pp) = player_pos {
                **t = format!("{:.0} , {:.0}", pp.x, pp.y);
            }
        } else if let Some(NoticeLine(i)) = notice {
            match net.notices.get(*i) {
                Some((msg, kind, born)) => {
                    let alpha = (1.0 - ((now - born) as f32 - 3.0).max(0.0)).clamp(0.0, 1.0);
                    let mut c = match kind.as_str() {
                        "exp" | "quest" => EXP_GOLD,
                        "loot" => LOOT_GREEN,
                        "levelup" => GOLD_BRIGHT,
                        "warn" | "death" => HP_RED,
                        _ => TEXT_MAIN,
                    };
                    c.set_alpha(alpha);
                    **t = msg.clone();
                    color.0 = c;
                }
                None => **t = String::new(),
            }
        }
    }

    // 任务追踪 + 聊天 (系统消息) 按 rev 重建
    let rev = (net.quest_rev, net.notice_rev);
    if *last_rev != rev {
        *last_rev = rev;
        for e in &q_tracker {
            let mut ec = commands.entity(e);
            ec.despawn_descendants();
            ec.with_children(|body| {
                for q in net.quests.iter().filter(|q| q.state == "active").take(3) {
                    body.spawn((
                        Node {
                            flex_direction: FlexDirection::Column,
                            row_gap: Val::Px(4.0),
                            padding: UiRect::axes(Val::Px(12.0), Val::Px(10.0)),
                            border: UiRect::left(Val::Px(2.0)),
                            ..default()
                        },
                        BackgroundColor(TRACK_BG),
                        BorderColor(GOLD),
                    ))
                    .with_children(|item| {
                        item.spawn(text(&skin.font, q.name.clone(), 12.0, TEXT_MAIN));
                        for o in &q.objectives {
                            let done = o.current >= o.required;
                            item.spawn(text(
                                &skin.font,
                                format!("已击杀 {} / {}", o.current.min(o.required), o.required),
                                12.0,
                                if done { EXP_GOLD } else { TEXT_DIM },
                            ));
                        }
                    });
                }
            });
        }
        for e in &q_chat {
            let mut ec = commands.entity(e);
            ec.despawn_descendants();
            ec.with_children(|body| {
                let recent: Vec<_> = net.chatlog.iter().rev().take(5).rev().cloned().collect();
                if recent.is_empty() {
                    body.spawn(text(&skin.font, "[系统] 欢迎来到玛法大陆", 13.0, TEXT_DIM));
                }
                for (tag, line) in recent {
                    let tag_color = if tag == "系统" { EXP_GOLD } else { GOLD };
                    body.spawn(Node::default()).with_children(|row| {
                        row.spawn(text(&skin.font, format!("[{tag}] "), 13.0, tag_color));
                        row.spawn(text(&skin.font, line, 13.0, TEXT_SUB));
                    });
                }
            });
        }
    }
}

/// 顶部中央目标栏: 锁定目标的名字 + 血条; 目标死亡/离场时自动清锁定。
/// 玩家血量不广播 (无 PvP 需求), 锁定玩家时只显示名字、血条置空。
pub fn target_frame(
    mut net: ResMut<Net>,
    mut q_pk: Query<
        (&mut Text, &mut TextColor),
        (
            With<PkModeText>,
            Without<TargetName>,
            Without<TargetHpText>,
            Without<TargetLevel>,
        ),
    >,
    remotes: Res<crate::Remotes>,
    mut q_frame: Query<&mut Node, With<TargetFrame>>,
    mut q_name: Query<(&mut Text, &mut TextColor), With<TargetName>>,
    mut q_fill: Query<&mut Node, (With<TargetHpFill>, Without<TargetFrame>)>,
    mut q_hp: Query<&mut Text, (With<TargetHpText>, Without<TargetName>)>,
    mut q_lv: Query<&mut Text, (With<TargetLevel>, Without<TargetName>, Without<TargetHpText>)>,
    mut q_trade: Query<
        &mut Node,
        (With<TargetTradeBtn>, Without<TargetFrame>, Without<TargetHpFill>),
    >,
) {
    if let Ok((mut t, mut c)) = q_pk.get_single_mut() {
        let (label, color) = if net.pk_mode == "all" {
            ("全体模式 (Ctrl+H)", Color::srgb(0.95, 0.35, 0.3))
        } else {
            ("和平模式 (Ctrl+H)", TEXT_DIM)
        };
        if **t != label {
            **t = label.to_string();
            c.0 = color;
        }
    }
    let info = net
        .target
        .as_ref()
        .and_then(|id| {
            let r = remotes.0.get(id)?;
            (r.anim != 4).then_some(r)
        })
        .map(|r| {
            let color = if r.owner.is_some() {
                Color::srgb(0.45, 0.95, 0.5) // 宠物 绿
            } else if r.image.is_some() {
                TEXT_MAIN // 怪物
            } else {
                // 玩家按善恶名色
                match r.pk.as_str() {
                    "grey" => Color::srgb(0.62, 0.62, 0.62),
                    "red" => Color::srgb(0.95, 0.28, 0.25),
                    _ => GOLD_BRIGHT,
                }
            };
            // 宠物名自带「 LvN」后缀, 等级徽标已单列, 去重
            let name = match r.level {
                Some(lv) => r
                    .name
                    .strip_suffix(&format!(" Lv{lv}"))
                    .unwrap_or(&r.name)
                    .to_string(),
                None => r.name.clone(),
            };
            let name = if name.is_empty() {
                "???".to_string()
            } else {
                name
            };
            // 玩家目标 (非怪非宠) 才给交易按钮
            let is_player = r.image.is_none() && r.owner.is_none();
            (name, color, r.hp, r.level, is_player)
        });
    if info.is_none() && net.target.is_some() {
        net.target = None;
    }
    let Ok(mut frame) = q_frame.get_single_mut() else {
        return;
    };
    match info {
        Some((name, color, hp, level, is_player)) => {
            frame.display = Display::Flex;
            if let Ok(mut n) = q_trade.get_single_mut() {
                let want = if is_player {
                    Display::Flex
                } else {
                    Display::None
                };
                if n.display != want {
                    n.display = want;
                }
            }
            if let Ok((mut t, mut c)) = q_name.get_single_mut() {
                if **t != name {
                    **t = name;
                }
                c.0 = color;
            }
            if let Ok(mut t) = q_lv.get_single_mut() {
                let label = match level {
                    Some(lv) => format!("Lv{lv}"),
                    None => String::new(),
                };
                if **t != label {
                    **t = label;
                }
            }
            let (frac, label) = match hp {
                Some((cur, max)) => (
                    (cur.max(0) as f32 / max.max(1) as f32).min(1.0),
                    format!("{}/{}", cur.max(0), max),
                ),
                None => (0.0, String::new()),
            };
            if let Ok(mut fill) = q_fill.get_single_mut() {
                fill.width = Val::Percent(100.0 * frac);
            }
            if let Ok(mut t) = q_hp.get_single_mut() {
                if **t != label {
                    **t = label;
                }
            }
        }
        None => {
            if frame.display != Display::None {
                frame.display = Display::None;
            }
        }
    }
}

/// 目标栏「交易」按钮: 对锁定的玩家发起交易请求
pub fn target_trade_click(
    q: Query<&Interaction, (With<TargetTradeClick>, Changed<Interaction>)>,
    q_party: Query<&Interaction, (With<TargetPartyBtn>, Changed<Interaction>)>,
    net: Res<Net>,
) {
    if let Some(t) = &net.target {
        if q.iter().any(|it| *it == Interaction::Pressed) {
            net.send(ClientMessage::TradeRequest {
                target_player_id: t.clone(),
            });
        }
        if q_party.iter().any(|it| *it == Interaction::Pressed) {
            net.send(ClientMessage::PartyInvite {
                target_player_id: t.clone(),
            });
        }
    }
}
