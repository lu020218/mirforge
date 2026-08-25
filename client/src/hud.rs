//! 方向 A「鎏金经典」HUD —— 逐参数对齐 design/HUD.dc.html 定稿。
//! 布局: 底部中央动作条 (92px 圆形头像+等级胶囊居中高出两翼, 左翼 HP 条+技能格
//! 1234, 右翼 MP 条+QWER), EXP 细条横贯屏幕底边, 右上 208 方形小地图 + 其下
//! 任务追踪, 左下玻璃聊天框, 右下七键功能组, 通知堆栈。

use bevy::prelude::*;
use bevy::ui::widget::NodeImageMode;

use crate::panels::{Panel, PanelKind};
use crate::Net;

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
    pub panel_ornate: Handle<Image>,
    pub titlebar: Handle<Image>,
    pub btn_gold: Handle<Image>,
    pub btn_gold_hover: Handle<Image>,
    pub btn_gold_pressed: Handle<Image>,
    pub bar_hp: Handle<Image>,
    pub bar_mp: Handle<Image>,
    pub slot: Handle<Image>,
    pub slot_gold: Handle<Image>,
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
        panel_ornate: assets.load("ui/skin/panel_ornate.png"),
        titlebar: assets.load("ui/skin/titlebar.png"),
        btn_gold: assets.load("ui/skin/btn_gold.png"),
        btn_gold_hover: assets.load("ui/skin/btn_gold_hover.png"),
        btn_gold_pressed: assets.load("ui/skin/btn_gold_pressed.png"),
        bar_hp: assets.load("ui/skin/bar_fill_hp.png"),
        bar_mp: assets.load("ui/skin/bar_fill_mp.png"),
        slot: assets.load("ui/skin/slot.png"),
        slot_gold: assets.load("ui/skin/slot_gold.png"),
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
pub struct MiniDot(usize);
#[derive(Component)]
pub struct TrackerBody;
#[derive(Component)]
pub struct ChatBody;
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
fn spawn_skill_slot(parent: &mut ChildBuilder, skin: &Skin, idx: Option<usize>, key: &str) {
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
                // 技能名 (双字, 图标素材接入前的占位)
                s.spawn((text(&skin.font, "", 14.0, TEXT_MAIN), SkillNameText(i)));
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
                BorderColor(EDGE_GOLD),
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
pub fn setup(mut commands: Commands, skin: Res<Skin>) {
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
            root.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                align_items: AlignItems::FlexEnd,
                ..default()
            })
            .with_children(|wing| {
                spawn_stat_bar(wing, &skin, "HP", skin.bar_hp.clone(), HpFill, HpText, true);
                wing.spawn(Node {
                    column_gap: Val::Px(8.0),
                    ..default()
                })
                .with_children(|row| {
                    spawn_skill_slot(row, &skin, Some(0), "1");
                    spawn_skill_slot(row, &skin, Some(1), "2");
                    spawn_skill_slot(row, &skin, Some(2), "3");
                    spawn_skill_slot(row, &skin, None, "4");
                });
            });
            // 中央: 92px 圆形头像 + 等级胶囊 (高出两翼 14px)
            root.spawn(Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                margin: UiRect::bottom(Val::Px(14.0)),
                ..default()
            })
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
            root.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                align_items: AlignItems::FlexStart,
                ..default()
            })
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
                    for key in ["Q", "W", "E", "R"] {
                        spawn_skill_slot(row, &skin, None, key);
                    }
                });
            });
        });

    // ── EXP 细条横贯底边 (5px) ──
    commands
        .spawn((
            HudRoot,
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
            BorderColor(EDGE_GOLD),
        ))
        .with_children(|map| {
            // 玩家点 (中心, 金色)
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
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(20.0),
                bottom: Val::Px(128.0),
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
                input.spawn(text(&skin.font, "系统", 12.0, GOLD));
                input.spawn(text(&skin.font, "聊天功能开发中…", 13.0, DISABLED));
            });
        });

    // ── 右下: 功能按钮组 (B C K L F G P) ──
    commands
        .spawn((
            HudRoot,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(20.0),
                bottom: Val::Px(128.0),
                column_gap: Val::Px(8.0),
                ..default()
            },
        ))
        .with_children(|menu| {
            let buttons: [(&str, &str, Option<PanelKind>); 7] = [
                ("包", "B", Some(PanelKind::Bag)),
                ("角", "C", Some(PanelKind::Character)),
                ("技", "K", None),
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
}

pub fn teardown(mut commands: Commands, q: Query<Entity, With<HudRoot>>) {
    for e in &q {
        commands.entity(e).despawn_recursive();
    }
}

/// 功能键点击 → 开关对应面板
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
    mut q_fill: Query<
        (
            &mut Node,
            Option<&HpFill>,
            Option<&MpFill>,
            Option<&ExpFill>,
            Option<&SkillMask>,
            Option<&MiniDot>,
        ),
        Or<(
            With<HpFill>,
            With<MpFill>,
            With<ExpFill>,
            With<SkillMask>,
            With<MiniDot>,
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

    // 怪物小地图点位: ±50 格视野 → 208px (2px/格)
    let mut dots: Vec<(f32, f32)> = Vec::new();
    if let Some(pp) = player_pos {
        for r in remotes.0.values() {
            if r.image.is_none() || r.anim == 4 {
                continue;
            }
            let (dx, dy) = ((r.pos.x - pp.x) * 2.0, (r.pos.y - pp.y) * 2.0);
            if dx.abs() < 100.0 && dy.abs() < 100.0 {
                dots.push((104.0 + dx as f32, 104.0 + dy as f32));
            }
        }
    }

    for (mut node, hp, mp, exp, mask, dot) in q_fill.iter_mut() {
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
                        BorderColor(EDGE_GOLD),
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
                for line in recent {
                    body.spawn(Node::default()).with_children(|row| {
                        row.spawn(text(&skin.font, "[系统] ", 13.0, EXP_GOLD));
                        row.spawn(text(&skin.font, line, 13.0, TEXT_SUB));
                    });
                }
            });
        }
    }
}
