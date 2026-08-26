//! 方向 A 面板族：背包 (B) / 角色 (C) / 任务 (L)。
//! M4.5 重做: 逐参数对齐 design/Panels.dc.html —— 金角饰线面板框、
//! 48px 标题栏 (可拖拽/关闭)、8 列背包格、经典 F10 装备环绕布局、
//! 两列属性; 任务面板沿同风格自延 (设计稿未含)。

use bevy::prelude::*;
use bevy::ui::RelativeCursorPosition;

use crate::hud::{
    Skin, DISABLED, EDGE_DARK, EDGE_GOLD, EXP_GOLD, GOLD, GOLD_BRIGHT, TEXT_DIM, TEXT_MAIN,
};
use crate::Net;
use protocol::ClientMessage;

/// 品质·白 (物品品质字段接入前统一用)
const QUALITY_COMMON: Color = Color::srgb(0.812, 0.784, 0.706); // #cfc8b4
const PANEL_BG: Color = Color::srgba(0.051, 0.059, 0.094, 0.94); // rgba(13,15,24,.94)
const SLOT_BG: Color = Color::srgb(0.055, 0.063, 0.090); // #0e1017
/// 标题栏微金渐变的 sRGB 预合成近似 (Bevy 无渐变)
const TITLE_BG: Color = Color::srgb(0.110, 0.106, 0.114);
const BTN_GOLD_BG: Color = Color::srgb(0.216, 0.176, 0.098); // #372d19

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelKind {
    Bag,
    Character,
    Quest,
}

#[derive(Component)]
pub struct Panel(pub PanelKind);
#[derive(Component)]
pub struct DragBar;
#[derive(Component)]
pub struct CloseBtn(pub PanelKind);
/// 面板内容容器 (rev 变化时清空重建)
#[derive(Component)]
pub struct PanelBody(pub PanelKind);

// ── 交互组件 ──
#[derive(Component)]
pub struct EquipItem(String);
#[derive(Component)]
pub struct UnequipSlot(&'static str);
#[derive(Component)]
pub struct QuestAction {
    quest: String,
    /// accept / complete / abandon
    act: &'static str,
}

/// 拖拽状态 (光标相对面板左上角的偏移)
#[derive(Resource, Default)]
pub struct Drag(Option<(Entity, Vec2)>);

/// 进入游戏时预建三面板 (默认隐藏), 位置为设计稿坐标
pub fn setup(mut commands: Commands, skin: Res<Skin>) {
    spawn_panel(
        &mut commands,
        &skin,
        PanelKind::Bag,
        "背 包",
        (468.0, 220.0),
        430.0,
    );
    spawn_panel(
        &mut commands,
        &skin,
        PanelKind::Character,
        "角 色",
        (1010.0, 180.0),
        440.0,
    );
    spawn_panel(
        &mut commands,
        &skin,
        PanelKind::Quest,
        "任 务",
        (60.0, 180.0),
        400.0,
    );
}

pub fn teardown(mut commands: Commands, q: Query<Entity, With<Panel>>) {
    for e in &q {
        commands.entity(e).despawn_recursive();
    }
}

/// 四角金饰线: 左上/右下 22px, 右上/左下 26px, 2px #c9a55c (设计稿 ornate)
pub fn ornate_corners(panel: &mut ChildBuilder) {
    let corners: [(f32, [bool; 4]); 4] = [
        // (尺寸, [top, right, bottom, left] 哪两边描线)
        (22.0, [true, false, false, true]), // 左上
        (22.0, [false, true, true, false]), // 右下
        (26.0, [true, true, false, false]), // 右上
        (26.0, [false, false, true, true]), // 左下
    ];
    for (i, (size, [t, r, b, l])) in corners.into_iter().enumerate() {
        let mut node = Node {
            position_type: PositionType::Absolute,
            width: Val::Px(size),
            height: Val::Px(size),
            border: UiRect {
                top: Val::Px(if t { 2.0 } else { 0.0 }),
                right: Val::Px(if r { 2.0 } else { 0.0 }),
                bottom: Val::Px(if b { 2.0 } else { 0.0 }),
                left: Val::Px(if l { 2.0 } else { 0.0 }),
            },
            ..default()
        };
        match i {
            0 => {
                node.top = Val::Px(-2.0);
                node.left = Val::Px(-2.0);
            }
            1 => {
                node.bottom = Val::Px(-2.0);
                node.right = Val::Px(-2.0);
            }
            2 => {
                node.top = Val::Px(-2.0);
                node.right = Val::Px(-2.0);
            }
            _ => {
                node.bottom = Val::Px(-2.0);
                node.left = Val::Px(-2.0);
            }
        }
        panel.spawn((node, BorderColor(GOLD)));
    }
}

fn spawn_panel(
    commands: &mut Commands,
    skin: &Skin,
    kind: PanelKind,
    title: &str,
    pos: (f32, f32),
    width: f32,
) {
    commands
        .spawn((
            Panel(kind),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(pos.0),
                top: Val::Px(pos.1),
                width: Val::Px(width),
                flex_direction: FlexDirection::Column,
                display: Display::None,
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            BorderColor(EDGE_GOLD),
            GlobalZIndex(10),
        ))
        .with_children(|panel| {
            ornate_corners(panel);
            // 标题栏 48px (拖拽区): 微金渐变近似底 + 底分隔线
            panel
                .spawn((
                    DragBar,
                    Button,
                    RelativeCursorPosition::default(),
                    Node {
                        height: Val::Px(48.0),
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::SpaceBetween,
                        padding: UiRect::horizontal(Val::Px(16.0)),
                        border: UiRect::bottom(Val::Px(1.0)),
                        ..default()
                    },
                    BackgroundColor(TITLE_BG),
                    BorderColor(EDGE_DARK),
                ))
                .with_children(|bar| {
                    bar.spawn(text(&skin.font, title, 15.0, TEXT_MAIN));
                    bar.spawn((Button, CloseBtn(kind), Node::default()))
                        .with_children(|x| {
                            x.spawn(text(&skin.font, "×", 18.0, TEXT_DIM));
                        });
                });
            // 内容容器
            panel.spawn((
                PanelBody(kind),
                Node {
                    flex_direction: FlexDirection::Column,
                    min_height: Val::Px(80.0),
                    ..default()
                },
            ));
        });
}

/// B/C/L 开关面板 (聊天输入时跳过)
pub fn toggle(
    keys: Res<ButtonInput<KeyCode>>,
    chat: Res<crate::hud::ChatState>,
    mut q: Query<(&Panel, &mut Node)>,
) {
    if chat.active {
        return;
    }
    let flip = |kind: PanelKind, q: &mut Query<(&Panel, &mut Node)>| {
        for (p, mut node) in q.iter_mut() {
            if p.0 == kind {
                node.display = if node.display == Display::None {
                    Display::Flex
                } else {
                    Display::None
                };
            }
        }
    };
    if keys.just_pressed(KeyCode::KeyB) {
        flip(PanelKind::Bag, &mut q);
    }
    if keys.just_pressed(KeyCode::KeyC) {
        flip(PanelKind::Character, &mut q);
    }
    if keys.just_pressed(KeyCode::KeyL) {
        flip(PanelKind::Quest, &mut q);
    }
}

/// 标题栏关闭按钮
pub fn close(
    q_btn: Query<(&Interaction, &CloseBtn), Changed<Interaction>>,
    mut q_panel: Query<(&Panel, &mut Node)>,
) {
    for (it, close) in &q_btn {
        if *it != Interaction::Pressed {
            continue;
        }
        for (p, mut node) in q_panel.iter_mut() {
            if p.0 == close.0 {
                node.display = Display::None;
            }
        }
    }
}

/// 标题栏拖拽: 按下记偏移, 按住跟随光标
pub fn drag(
    mut drag: ResMut<Drag>,
    windows: Query<&Window>,
    buttons: Res<ButtonInput<MouseButton>>,
    q_bar: Query<(&Interaction, &Parent), With<DragBar>>,
    mut q_panel: Query<&mut Node, With<Panel>>,
) {
    let Ok(win) = windows.get_single() else {
        return;
    };
    let Some(cursor) = win.cursor_position() else {
        return;
    };
    if buttons.just_pressed(MouseButton::Left) {
        for (it, parent) in &q_bar {
            if *it == Interaction::Pressed {
                if let Ok(node) = q_panel.get(parent.get()) {
                    let (Val::Px(l), Val::Px(t)) = (node.left, node.top) else {
                        continue;
                    };
                    drag.0 = Some((parent.get(), cursor - Vec2::new(l, t)));
                }
            }
        }
    }
    if buttons.pressed(MouseButton::Left) {
        if let Some((panel, off)) = drag.0 {
            if let Ok(mut node) = q_panel.get_mut(panel) {
                node.left = Val::Px((cursor.x - off.x).max(0.0));
                node.top = Val::Px((cursor.y - off.y).max(0.0));
            }
        }
    } else {
        drag.0 = None;
    }
}

/// 点击交互: 装备/卸下/任务操作
#[allow(clippy::type_complexity)]
pub fn clicks(
    net: Res<Net>,
    mut q: Query<
        (
            &Interaction,
            Option<&EquipItem>,
            Option<&UnequipSlot>,
            Option<&QuestAction>,
        ),
        Changed<Interaction>,
    >,
) {
    for (it, equip, unequip, quest) in q.iter_mut() {
        if *it != Interaction::Pressed {
            continue;
        }
        if let Some(EquipItem(id)) = equip {
            net.send(ClientMessage::Equip {
                item_id: id.clone(),
                slot: String::new(),
            });
        } else if let Some(UnequipSlot(slot)) = unequip {
            net.send(ClientMessage::Unequip {
                slot: slot.to_string(),
            });
        } else if let Some(qa) = quest {
            let msg = match qa.act {
                "accept" => ClientMessage::AcceptQuest {
                    quest_id: qa.quest.clone(),
                },
                "complete" => ClientMessage::CompleteQuest {
                    quest_id: qa.quest.clone(),
                },
                _ => ClientMessage::AbandonQuest {
                    quest_id: qa.quest.clone(),
                },
            };
            net.send(msg);
        }
    }
}

fn stats_line(i: &protocol::ItemInfo) -> String {
    let mut s = Vec::new();
    if i.attack > 0 {
        s.push(format!("攻+{}", i.attack));
    }
    if i.defense > 0 {
        s.push(format!("防+{}", i.defense));
    }
    if i.hp > 0 {
        s.push(format!("血+{}", i.hp));
    }
    s.join(" ")
}

/// 内容重建: inventory/equipment/quests/stat 变化时刷新对应面板
#[allow(clippy::too_many_arguments)]
pub fn refresh(
    mut commands: Commands,
    net: Res<Net>,
    skin: Res<Skin>,
    portrait: Option<Res<crate::Portrait>>,
    world: Res<crate::World>,
    mut icons: ResMut<crate::ItemIcons>,
    mut images: ResMut<Assets<Image>>,
    mut last_rev: Local<(u32, u32, u32)>,
    q_body: Query<(Entity, &PanelBody)>,
) {
    let rev = (net.inv_rev, net.quest_rev, net.stat_rev);
    if *last_rev == rev {
        return;
    }
    *last_rev = rev;
    let portrait = portrait.as_ref().and_then(|p| p.0.clone());
    // 本次重建涉及的物品图标预取
    let mut icon_of = |img: u16| icons.get(img, &world.data_root, &mut images);
    let inv_icons: Vec<Option<Handle<Image>>> =
        net.inventory.iter().map(|i| icon_of(i.image)).collect();
    let equip_icons: std::collections::HashMap<String, Option<Handle<Image>>> = net
        .equipment
        .iter()
        .map(|(k, i)| (k.clone(), icon_of(i.image)))
        .collect();
    for (entity, body) in &q_body {
        let mut e = commands.entity(entity);
        e.despawn_descendants();
        match body.0 {
            PanelKind::Bag => build_bag(&mut e, &net, &skin, &inv_icons),
            PanelKind::Character => {
                build_character(&mut e, &net, &skin, portrait.clone(), &equip_icons)
            }
            PanelKind::Quest => build_quest(&mut e, &net, &skin),
        }
    }
}

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

/// 背包: 8×4 网格 44px 格, 物品格白边+名字, 点击穿戴; 底栏统计
fn build_bag(
    e: &mut bevy::ecs::system::EntityCommands,
    net: &Net,
    skin: &Skin,
    icons: &[Option<Handle<Image>>],
) {
    let items = net.inventory.clone();
    let font = skin.font.clone();
    let used = items.len();
    e.with_children(|body| {
        // 格区: padding 16, 8 列 gap 6 → (430-2-32-42)/8 ≈ 44px 方格
        body.spawn(Node {
            padding: UiRect::all(Val::Px(16.0)),
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Wrap,
            column_gap: Val::Px(6.0),
            row_gap: Val::Px(6.0),
            ..default()
        })
        .with_children(|grid| {
            for i in 0..32 {
                let item = items.get(i);
                let mut slot = grid.spawn((
                    Node {
                        width: Val::Px(44.0),
                        height: Val::Px(44.0),
                        border: UiRect::all(Val::Px(1.0)),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    BackgroundColor(SLOT_BG),
                    BorderColor(if item.is_some() {
                        QUALITY_COMMON
                    } else {
                        EDGE_DARK
                    }),
                    BorderRadius::all(Val::Px(3.0)),
                ));
                if let Some(it) = item {
                    slot.insert((Button, EquipItem(it.id.clone())));
                    let icon = icons.get(i).cloned().flatten();
                    let name: String = it.name.chars().take(2).collect();
                    slot.with_children(|s| match icon {
                        Some(h) => {
                            s.spawn((
                                Node {
                                    width: Val::Px(38.0),
                                    height: Val::Px(38.0),
                                    ..default()
                                },
                                ImageNode::new(h),
                            ));
                        }
                        None => {
                            s.spawn(text(&font, name, 13.0, QUALITY_COMMON));
                        }
                    });
                }
            }
        });
        // 选中说明行 (背包底部之上)
        if let Some(it) = items.first() {
            body.spawn(Node {
                padding: UiRect::horizontal(Val::Px(16.0)),
                ..default()
            })
            .with_children(|row| {
                row.spawn(text(
                    &font,
                    format!("{} {}  (点击格子穿戴)", it.name, stats_line(it)),
                    12.0,
                    TEXT_DIM,
                ));
            });
        }
        // 底栏 42px: 顶分隔线 + 统计
        body.spawn((
            Node {
                height: Val::Px(42.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::horizontal(Val::Px(16.0)),
                border: UiRect::top(Val::Px(1.0)),
                margin: UiRect::top(Val::Px(8.0)),
                ..default()
            },
            BorderColor(EDGE_DARK),
        ))
        .with_children(|bar| {
            bar.spawn(text(&font, format!("32 格 · 已用 {used}"), 12.0, TEXT_DIM));
            bar.spawn(text(&font, "金币 0", 12.0, EXP_GOLD));
        });
    });
}

/// 装备槽 56px (底排 52px): 有装备→金边+名字缩写(点击卸下), 空→暗边+占位名
#[allow(clippy::too_many_arguments)]
fn equip_slot(
    parent: &mut ChildBuilder,
    font: &Handle<Font>,
    equipment: &std::collections::HashMap<String, protocol::ItemInfo>,
    icons: &std::collections::HashMap<String, Option<Handle<Image>>>,
    slot_key: Option<&'static str>,
    label: &str,
    size: f32,
) {
    let item = slot_key.and_then(|k| equipment.get(k));
    let icon = slot_key.and_then(|k| icons.get(k)).cloned().flatten();
    let mut n = parent.spawn((
        Node {
            width: Val::Px(size),
            height: Val::Px(size),
            border: UiRect::all(Val::Px(1.0)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            ..default()
        },
        BackgroundColor(SLOT_BG),
        BorderColor(if item.is_some() { EDGE_GOLD } else { EDGE_DARK }),
        BorderRadius::all(Val::Px(3.0)),
    ));
    if let (Some(key), true) = (slot_key, item.is_some()) {
        n.insert((Button, UnequipSlot(key)));
    }
    match (item, icon) {
        (Some(_), Some(h)) => {
            n.with_children(|s| {
                s.spawn((
                    Node {
                        width: Val::Px(size - 12.0),
                        height: Val::Px(size - 12.0),
                        ..default()
                    },
                    ImageNode::new(h),
                ));
            });
        }
        (Some(it), None) => {
            let name: String = it.name.chars().take(2).collect();
            n.with_children(|s| {
                s.spawn(text(font, name, 13.0, QUALITY_COMMON));
            });
        }
        (None, _) => {
            n.with_children(|s| {
                s.spawn(text(font, label, 10.0, DISABLED));
            });
        }
    }
}

/// 角色: 经典 F10 布局 — 立绘居中, 装备槽左右下环绕 + 两列属性
fn build_character(
    e: &mut bevy::ecs::system::EntityCommands,
    net: &Net,
    skin: &Skin,
    portrait: Option<(Handle<Image>, Vec2)>,
    icons: &std::collections::HashMap<String, Option<Handle<Image>>>,
) {
    let font = skin.font.clone();
    let equipment = net.equipment.clone();
    let stat = net.stat;
    let name = net
        .characters
        .iter()
        .find(|c| Some(&c.id) == net.character_id.as_ref())
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "冒险者".into());
    let (atk, def): (i32, i32) = (
        equipment.values().map(|i| i.attack).sum(),
        equipment.values().map(|i| i.defense).sum(),
    );
    e.with_children(|body| {
        // 主区: 左列 4 槽 | 中央立绘+名字 | 右列 4 槽
        body.spawn(Node {
            padding: UiRect::new(Val::Px(20.0), Val::Px(20.0), Val::Px(18.0), Val::Px(0.0)),
            justify_content: JustifyContent::SpaceBetween,
            column_gap: Val::Px(12.0),
            ..default()
        })
        .with_children(|row| {
            row.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                ..default()
            })
            .with_children(|col| {
                equip_slot(col, &font, &equipment, icons, Some("weapon"), "武器", 56.0);
                equip_slot(col, &font, &equipment, icons, Some("armor"), "衣服", 56.0);
                equip_slot(col, &font, &equipment, icons, None, "护腕", 56.0);
                equip_slot(col, &font, &equipment, icons, Some("ring"), "戒指", 56.0);
            });
            // 中央立绘 (150×240 剪影近似) + 名字/Lv
            row.spawn(Node {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(10.0),
                flex_grow: 1.0,
                ..default()
            })
            .with_children(|mid| {
                mid.spawn((
                    Node {
                        width: Val::Px(150.0),
                        height: Val::Px(240.0),
                        border: UiRect::all(Val::Px(1.0)),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.137, 0.145, 0.204)), // #23263a
                    BorderColor(EDGE_DARK),
                    BorderRadius::all(Val::Px(4.0)),
                ))
                .with_children(|frame| {
                    if let Some((img, size)) = portrait {
                        // 站立帧等比放大到高 ~220 (像素风 nearest)
                        let scale = (220.0 / size.y).min(140.0 / size.x);
                        frame.spawn((
                            Node {
                                width: Val::Px(size.x * scale),
                                height: Val::Px(size.y * scale),
                                ..default()
                            },
                            ImageNode::new(img),
                        ));
                    }
                });
                mid.spawn(Node {
                    align_items: AlignItems::Baseline,
                    column_gap: Val::Px(8.0),
                    ..default()
                })
                .with_children(|line| {
                    line.spawn(text(&font, name, 17.0, TEXT_MAIN));
                    line.spawn((
                        Node {
                            border: UiRect::all(Val::Px(1.0)),
                            padding: UiRect::axes(Val::Px(8.0), Val::Px(0.0)),
                            ..default()
                        },
                        BorderColor(EDGE_GOLD),
                        BorderRadius::all(Val::Px(9.0)),
                    ))
                    .with_children(|lv| {
                        lv.spawn(text(
                            &font,
                            format!("Lv {}", stat.map(|s| s.level).unwrap_or(1)),
                            11.0,
                            GOLD_BRIGHT,
                        ));
                    });
                });
            });
            row.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                ..default()
            })
            .with_children(|col| {
                equip_slot(col, &font, &equipment, icons, Some("helmet"), "头盔", 56.0);
                equip_slot(
                    col,
                    &font,
                    &equipment,
                    icons,
                    Some("necklace"),
                    "项链",
                    56.0,
                );
                equip_slot(col, &font, &equipment, icons, None, "护腕", 56.0);
                equip_slot(col, &font, &equipment, icons, None, "戒指", 56.0);
            });
        });
        // 底排 5 槽 52px 居中
        body.spawn(Node {
            padding: UiRect::new(Val::Px(20.0), Val::Px(20.0), Val::Px(10.0), Val::Px(14.0)),
            justify_content: JustifyContent::Center,
            column_gap: Val::Px(10.0),
            ..default()
        })
        .with_children(|row| {
            for label in ["腰带", "鞋子", "宝石", "生肖", "星座"] {
                equip_slot(row, &font, &equipment, icons, None, label, 52.0);
            }
        });
        // 属性: 两列 grid, 顶分隔线
        let pairs: Vec<(String, String)> = vec![
            ("攻击".into(), format!("+{atk}")),
            ("防御".into(), format!("+{def}")),
            (
                "生命".into(),
                stat.map(|s| format!("{}/{}", s.hp, s.max_hp))
                    .unwrap_or_default(),
            ),
            (
                "魔法".into(),
                stat.map(|s| format!("{}/{}", s.mp, s.max_mp))
                    .unwrap_or_default(),
            ),
            (
                "经验".into(),
                stat.map(|s| format!("{}/{}", s.exp, s.req))
                    .unwrap_or_default(),
            ),
            (
                "等级".into(),
                stat.map(|s| s.level.to_string()).unwrap_or_default(),
            ),
        ];
        body.spawn((
            Node {
                border: UiRect::top(Val::Px(1.0)),
                padding: UiRect::new(Val::Px(20.0), Val::Px(20.0), Val::Px(14.0), Val::Px(18.0)),
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                column_gap: Val::Px(24.0),
                row_gap: Val::Px(8.0),
                ..default()
            },
            BorderColor(EDGE_DARK),
        ))
        .with_children(|grid| {
            for (label, value) in pairs {
                grid.spawn(Node {
                    width: Val::Px(180.0),
                    justify_content: JustifyContent::SpaceBetween,
                    ..default()
                })
                .with_children(|cell| {
                    cell.spawn(text(&font, label, 12.0, TEXT_DIM));
                    cell.spawn(text(&font, value, 12.0, TEXT_MAIN));
                });
            }
        });
    });
}

/// 任务: 列表 + 操作按钮 (面板框同风格)
fn build_quest(e: &mut bevy::ecs::system::EntityCommands, net: &Net, skin: &Skin) {
    let font = skin.font.clone();
    let quests = net.quests.clone();
    fn mob_name(t: &str) -> &str {
        match t {
            "chicken" => "鸡",
            "deer" => "鹿",
            "scarecrow" => "稻草人",
            other => other,
        }
    }
    e.with_children(|body| {
        body.spawn(Node {
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(12.0),
            padding: UiRect::all(Val::Px(16.0)),
            ..default()
        })
        .with_children(|list| {
            if quests.is_empty() {
                list.spawn(text(&font, "暂无可接任务", 12.0, TEXT_DIM));
            }
            for q in &quests {
                let (tag, tag_color) = match q.state.as_str() {
                    "active" => ("进行中", EXP_GOLD),
                    "completed" => ("已完成", DISABLED),
                    _ => ("可接取", TEXT_MAIN),
                };
                list.spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(6.0),
                        padding: UiRect::all(Val::Px(12.0)),
                        border: UiRect::all(Val::Px(1.0)),
                        ..default()
                    },
                    BackgroundColor(SLOT_BG),
                    BorderColor(EDGE_DARK),
                    BorderRadius::all(Val::Px(3.0)),
                ))
                .with_children(|item| {
                    item.spawn(Node {
                        column_gap: Val::Px(8.0),
                        align_items: AlignItems::Center,
                        ..default()
                    })
                    .with_children(|row| {
                        row.spawn(text(&font, q.name.clone(), 14.0, TEXT_MAIN));
                        row.spawn((
                            Node {
                                border: UiRect::all(Val::Px(1.0)),
                                padding: UiRect::axes(Val::Px(8.0), Val::Px(0.0)),
                                ..default()
                            },
                            BorderColor(EDGE_DARK),
                            BorderRadius::all(Val::Px(9.0)),
                        ))
                        .with_children(|t| {
                            t.spawn(text(&font, tag, 11.0, tag_color));
                        });
                        row.spawn(text(
                            &font,
                            format!("经验 {}", q.exp_reward),
                            11.0,
                            EXP_GOLD,
                        ));
                    });
                    if q.state == "active" {
                        for o in &q.objectives {
                            let color = if o.current >= o.required {
                                EXP_GOLD
                            } else {
                                TEXT_DIM
                            };
                            item.spawn(text(
                                &font,
                                format!(
                                    "击杀{}  {}/{}",
                                    mob_name(&o.target_id),
                                    o.current,
                                    o.required
                                ),
                                12.0,
                                color,
                            ));
                        }
                    }
                    // 操作按钮
                    let actions: Vec<(&str, &str)> = match q.state.as_str() {
                        "available" => vec![("accept", "接 取")],
                        "active" => {
                            let done = q.objectives.iter().all(|o| o.current >= o.required);
                            if done {
                                vec![("complete", "交 付"), ("abandon", "放 弃")]
                            } else {
                                vec![("abandon", "放 弃")]
                            }
                        }
                        _ => vec![],
                    };
                    if !actions.is_empty() {
                        item.spawn(Node {
                            column_gap: Val::Px(8.0),
                            margin: UiRect::top(Val::Px(4.0)),
                            ..default()
                        })
                        .with_children(|row| {
                            for (act, label) in actions {
                                let primary = act != "abandon";
                                row.spawn((
                                    Button,
                                    QuestAction {
                                        quest: q.id.clone(),
                                        act,
                                    },
                                    Node {
                                        padding: UiRect::axes(Val::Px(16.0), Val::Px(5.0)),
                                        border: UiRect::all(Val::Px(1.0)),
                                        ..default()
                                    },
                                    BackgroundColor(if primary {
                                        BTN_GOLD_BG
                                    } else {
                                        Color::NONE
                                    }),
                                    BorderColor(if primary { EDGE_GOLD } else { EDGE_DARK }),
                                    BorderRadius::all(Val::Px(3.0)),
                                ))
                                .with_children(|b| {
                                    b.spawn(text(
                                        &font,
                                        label,
                                        12.0,
                                        if primary { GOLD_BRIGHT } else { TEXT_DIM },
                                    ));
                                });
                            }
                        });
                    }
                });
            }
        });
    });
}
