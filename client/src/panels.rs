//! 方向 A 面板族 (M4.4)：背包 (B) / 角色 (C) / 任务 (L)。
//! Bevy UI + panel_ornate 鎏金四角饰边皮肤，titlebar 可拖拽，btn_gold 三态按钮。
//! 内容由 Net (inventory/equipment/quests/stat) 驱动，rev 计数变化时重建。

use bevy::prelude::*;
use bevy::ui::RelativeCursorPosition;

use crate::hud::{sliced, Skin, DISABLED, EXP_GOLD, GOLD_BRIGHT, TEXT_DIM, TEXT_MAIN};
use crate::Net;
use protocol::ClientMessage;

/// 品质·白 (物品品质字段接入前统一用)
const QUALITY_COMMON: Color = Color::srgb(0.812, 0.784, 0.706); // #cfc8b4

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
/// 金色按钮三态换图
#[derive(Component)]
pub struct GoldBtn;

/// 拖拽状态 (光标相对面板左上角的偏移)
#[derive(Resource, Default)]
pub struct Drag(Option<(Entity, Vec2)>);

/// 进入游戏时预建三面板 (默认隐藏)
pub fn setup(mut commands: Commands, skin: Res<Skin>) {
    spawn_panel(
        &mut commands,
        &skin,
        PanelKind::Bag,
        "背 包",
        (60.0, 120.0),
        340.0,
    );
    spawn_panel(
        &mut commands,
        &skin,
        PanelKind::Character,
        "角 色",
        (440.0, 120.0),
        320.0,
    );
    spawn_panel(
        &mut commands,
        &skin,
        PanelKind::Quest,
        "任 务",
        (790.0, 120.0),
        380.0,
    );
}

pub fn teardown(mut commands: Commands, q: Query<Entity, With<Panel>>) {
    for e in &q {
        commands.entity(e).despawn_recursive();
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
                padding: UiRect::all(Val::Px(10.0)),
                ..default()
            },
            ImageNode::new(skin.panel_ornate.clone()).with_mode(sliced(32.0)),
            GlobalZIndex(10),
        ))
        .with_children(|panel| {
            // 标题栏 (拖拽区)
            panel
                .spawn((
                    DragBar,
                    Button,
                    RelativeCursorPosition::default(),
                    Node {
                        height: Val::Px(30.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        margin: UiRect::bottom(Val::Px(8.0)),
                        ..default()
                    },
                    ImageNode::new(skin.titlebar.clone()).with_mode(sliced(12.0)),
                ))
                .with_children(|bar| {
                    bar.spawn((
                        Text::new(title),
                        TextFont {
                            font: skin.font.clone(),
                            font_size: 16.0,
                            ..default()
                        },
                        TextColor(GOLD_BRIGHT),
                    ));
                });
            // 内容容器
            panel.spawn((
                PanelBody(kind),
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(6.0),
                    min_height: Val::Px(80.0),
                    ..default()
                },
            ));
        });
}

/// B/C/L 开关面板
pub fn toggle(keys: Res<ButtonInput<KeyCode>>, mut q: Query<(&Panel, &mut Node)>) {
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

/// 金色按钮三态
#[allow(clippy::type_complexity)]
pub fn button_skin(
    skin: Res<Skin>,
    mut q: Query<(&Interaction, &mut ImageNode), (With<GoldBtn>, Changed<Interaction>)>,
) {
    for (it, mut img) in q.iter_mut() {
        img.image = match it {
            Interaction::Pressed => skin.btn_gold_pressed.clone(),
            Interaction::Hovered => skin.btn_gold_hover.clone(),
            Interaction::None => skin.btn_gold.clone(),
        };
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
    mut last_rev: Local<(u32, u32, u32)>,
    q_body: Query<(Entity, &PanelBody)>,
) {
    let rev = (net.inv_rev, net.quest_rev, net.stat_rev);
    if *last_rev == rev {
        return;
    }
    *last_rev = rev;
    for (entity, body) in &q_body {
        let mut e = commands.entity(entity);
        e.despawn_descendants();
        match body.0 {
            PanelKind::Bag => build_bag(&mut e, &net, &skin),
            PanelKind::Character => build_character(&mut e, &net, &skin),
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

/// 背包: 6×4 网格, 有物品的格显示名字, 点击穿戴
fn build_bag(e: &mut bevy::ecs::system::EntityCommands, net: &Net, skin: &Skin) {
    let items = net.inventory.clone();
    let font = skin.font.clone();
    let slot_img = skin.slot.clone();
    let e_ref = e.with_children(|body| {
        body.spawn(Node {
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Wrap,
            column_gap: Val::Px(4.0),
            row_gap: Val::Px(4.0),
            ..default()
        })
        .with_children(|grid| {
            for i in 0..24 {
                let item = items.get(i);
                let mut slot = grid.spawn((
                    Node {
                        width: Val::Px(48.0),
                        height: Val::Px(48.0),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    ImageNode::new(slot_img.clone()).with_mode(sliced(6.0)),
                ));
                if let Some(it) = item {
                    slot.insert((Button, EquipItem(it.id.clone())));
                    let name: String = it.name.chars().take(2).collect();
                    slot.with_children(|s| {
                        s.spawn(text(&font, name, 14.0, QUALITY_COMMON));
                    });
                }
            }
        });
        if let Some(it) = items.first() {
            body.spawn(text(
                &font,
                format!("{} {}  (点击格子穿戴)", it.name, stats_line(it)),
                12.0,
                TEXT_DIM,
            ));
        } else {
            body.spawn(text(&font, "空空如也", 12.0, TEXT_DIM));
        }
    });
    let _ = e_ref;
}

/// 角色: 左装备槽列 + 右属性
fn build_character(e: &mut bevy::ecs::system::EntityCommands, net: &Net, skin: &Skin) {
    const SLOTS: [(&str, &str); 5] = [
        ("weapon", "武器"),
        ("armor", "衣服"),
        ("helmet", "头盔"),
        ("necklace", "项链"),
        ("ring", "戒指"),
    ];
    let font = skin.font.clone();
    let equipment = net.equipment.clone();
    let stat = net.stat;
    let (atk, def): (i32, i32) = (
        equipment.values().map(|i| i.attack).sum(),
        equipment.values().map(|i| i.defense).sum(),
    );
    let slot_img = skin.slot_gold.clone();
    e.with_children(|body| {
        body.spawn(Node {
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(14.0),
            ..default()
        })
        .with_children(|row| {
            // 左: 装备槽
            row.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(5.0),
                ..default()
            })
            .with_children(|col| {
                for (slot, label) in SLOTS {
                    let item = equipment.get(slot);
                    let mut n = col.spawn((
                        Node {
                            width: Val::Px(150.0),
                            height: Val::Px(30.0),
                            align_items: AlignItems::Center,
                            padding: UiRect::horizontal(Val::Px(8.0)),
                            column_gap: Val::Px(6.0),
                            ..default()
                        },
                        ImageNode::new(slot_img.clone()).with_mode(sliced(6.0)),
                    ));
                    if item.is_some() {
                        n.insert((Button, UnequipSlot(slot)));
                    }
                    n.with_children(|s| {
                        s.spawn(text(&font, label, 12.0, TEXT_DIM));
                        match item {
                            Some(it) => {
                                s.spawn(text(&font, it.name.clone(), 13.0, QUALITY_COMMON));
                            }
                            None => {
                                s.spawn(text(&font, "-", 13.0, DISABLED));
                            }
                        }
                    });
                }
                col.spawn(text(&font, "(点击槽位卸下)", 11.0, TEXT_DIM));
            });
            // 右: 属性
            row.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(6.0),
                ..default()
            })
            .with_children(|col| {
                if let Some(s) = stat {
                    col.spawn(text(&font, format!("等级  {}", s.level), 13.0, GOLD_BRIGHT));
                    col.spawn(text(
                        &font,
                        format!("经验  {}/{}", s.exp, s.req),
                        13.0,
                        EXP_GOLD,
                    ));
                    col.spawn(text(
                        &font,
                        format!("生命  {}/{}", s.hp, s.max_hp),
                        13.0,
                        TEXT_MAIN,
                    ));
                    col.spawn(text(
                        &font,
                        format!("魔法  {}/{}", s.mp, s.max_mp),
                        13.0,
                        TEXT_MAIN,
                    ));
                }
                col.spawn(text(&font, format!("装备攻击 +{atk}"), 13.0, TEXT_MAIN));
                col.spawn(text(&font, format!("装备防御 +{def}"), 13.0, TEXT_MAIN));
            });
        });
    });
}

/// 任务: 列表 + 操作按钮
fn build_quest(e: &mut bevy::ecs::system::EntityCommands, net: &Net, skin: &Skin) {
    let font = skin.font.clone();
    let quests = net.quests.clone();
    let btn = skin.btn_gold.clone();
    fn mob_name(t: &str) -> &str {
        match t {
            "chicken" => "鸡",
            "deer" => "鹿",
            "scarecrow" => "稻草人",
            other => other,
        }
    }
    e.with_children(|body| {
        if quests.is_empty() {
            body.spawn(text(&font, "暂无可接任务", 12.0, TEXT_DIM));
        }
        for q in &quests {
            let (tag, tag_color) = match q.state.as_str() {
                "active" => ("进行中", EXP_GOLD),
                "completed" => ("已完成", DISABLED),
                _ => ("可接取", TEXT_MAIN),
            };
            body.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(3.0),
                margin: UiRect::bottom(Val::Px(4.0)),
                ..default()
            })
            .with_children(|item| {
                item.spawn(Node {
                    column_gap: Val::Px(8.0),
                    align_items: AlignItems::Center,
                    ..default()
                })
                .with_children(|row| {
                    row.spawn(text(&font, format!("[{tag}]"), 12.0, tag_color));
                    row.spawn(text(&font, q.name.clone(), 14.0, TEXT_MAIN));
                    row.spawn(text(&font, format!("经验{}", q.exp_reward), 11.0, EXP_GOLD));
                });
                if q.state == "active" {
                    for o in &q.objectives {
                        row_objective(item, &font, mob_name(&o.target_id), o.current, o.required);
                    }
                }
                // 操作按钮
                let actions: Vec<(&str, &str)> = match q.state.as_str() {
                    "available" => vec![("accept", "接取")],
                    "active" => {
                        let done = q.objectives.iter().all(|o| o.current >= o.required);
                        if done {
                            vec![("complete", "交付"), ("abandon", "放弃")]
                        } else {
                            vec![("abandon", "放弃")]
                        }
                    }
                    _ => vec![],
                };
                if !actions.is_empty() {
                    item.spawn(Node {
                        column_gap: Val::Px(8.0),
                        ..default()
                    })
                    .with_children(|row| {
                        for (act, label) in actions {
                            row.spawn((
                                Button,
                                GoldBtn,
                                QuestAction {
                                    quest: q.id.clone(),
                                    act,
                                },
                                Node {
                                    padding: UiRect::axes(Val::Px(14.0), Val::Px(4.0)),
                                    ..default()
                                },
                                ImageNode::new(btn.clone()).with_mode(sliced(12.0)),
                            ))
                            .with_children(|b| {
                                b.spawn(text(&font, label, 13.0, Color::srgb(0.10, 0.09, 0.05)));
                            });
                        }
                    });
                }
            });
        }
    });
}

fn row_objective(
    parent: &mut ChildBuilder,
    font: &Handle<Font>,
    name: &str,
    current: u32,
    required: u32,
) {
    let color = if current >= required {
        EXP_GOLD
    } else {
        TEXT_DIM
    };
    parent.spawn(text(
        font,
        format!("  击杀{name}  {current}/{required}"),
        12.0,
        color,
    ));
}
