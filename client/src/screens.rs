//! 登录 / 选角 正式版 (M4.5) —— 逐参数对齐 design/Login.dc.html 与
//! CharSelect.dc.html。Bevy UI + 自实现文本输入框；egui 过渡版退役。
//! 尺寸用设计稿 1920×1080 基准原值 (Px)，竖向锚点用 Vh 百分比适配窗口高。

use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;

use crate::hud::{
    Skin, DISABLED, EDGE_DARK, EDGE_GOLD, GOLD, GOLD_BRIGHT, HP_RED, TEXT_DIM, TEXT_MAIN, TEXT_SUB,
};
use crate::{ClientMessage, Net};

const CARD_BG: Color = Color::srgba(0.051, 0.059, 0.094, 0.88); // rgba(13,15,24,.88)
const INPUT_BG: Color = Color::srgb(0.055, 0.063, 0.090); // #0e1017
const BTN_GOLD_BG: Color = Color::srgb(0.216, 0.176, 0.098); // #372d19 (渐变中值)
/// rgba(201,165,92,.07) 的 sRGB 预合成值 (Bevy 在 linear 空间混合会偏亮)
const SEL_BG: Color = Color::srgb(0.103, 0.100, 0.113);
const LIST_BG: Color = Color::srgba(0.051, 0.059, 0.094, 0.70);

/// 分辨率适配: 覆盖窗口 scale factor = 物理高/1080, 使逻辑高恒为设计稿
/// 基准 1080 —— 所有 UI 直接用设计稿 px 值即与设计稿 1:1, 任意分辨率铺满。
pub fn adapt_scale(mut windows: Query<&mut Window>) {
    let Ok(mut w) = windows.get_single_mut() else {
        return;
    };
    let s = (w.resolution.physical_height() as f32 / 1080.0).max(0.5);
    if w.resolution
        .scale_factor_override()
        .is_none_or(|cur| (cur - s).abs() > 0.005)
    {
        w.resolution.set_scale_factor_override(Some(s));
    }
}

// ─────────── 文本输入框 (Bevy 无内置, 自实现) ───────────

#[derive(Component)]
pub struct TextInput {
    pub value: String,
    pub password: bool,
    pub placeholder: String,
    pub focused: bool,
    /// Tab 焦点顺序
    pub order: u8,
}

#[derive(Component)]
pub struct TextInputDisplay;

/// 点击聚焦 + 键入 (字符/退格/Tab 切换); Enter 由各屏系统处理
pub fn text_input(
    mut q_input: Query<(&mut TextInput, &Interaction, &mut BorderColor, &Children)>,
    mut q_display: Query<(&mut Text, &mut TextColor), With<TextInputDisplay>>,
    mut keys: EventReader<KeyboardInput>,
    mouse: Res<ButtonInput<MouseButton>>,
) {
    // 点击聚焦 (点到谁聚焦谁; 点别处不清焦, 保持简单)
    if mouse.just_pressed(MouseButton::Left) {
        let clicked: Vec<u8> = q_input
            .iter()
            .filter(|(_, it, ..)| **it == Interaction::Pressed || **it == Interaction::Hovered)
            .filter(|(_, it, ..)| **it == Interaction::Pressed)
            .map(|(inp, ..)| inp.order)
            .collect();
        if let Some(&target) = clicked.first() {
            for (mut inp, ..) in q_input.iter_mut() {
                inp.focused = inp.order == target;
            }
        }
    }
    // 键入
    let mut tab = false;
    for ev in keys.read() {
        if !ev.state.is_pressed() {
            continue;
        }
        match &ev.logical_key {
            Key::Character(s) => {
                for (mut inp, ..) in q_input.iter_mut() {
                    if inp.focused && inp.value.chars().count() < 24 {
                        // 账号密码限 ASCII 可见字符
                        for c in s.chars().filter(|c| c.is_ascii_graphic()) {
                            inp.value.push(c);
                        }
                    }
                }
            }
            Key::Backspace => {
                for (mut inp, ..) in q_input.iter_mut() {
                    if inp.focused {
                        inp.value.pop();
                    }
                }
            }
            Key::Tab => tab = true,
            _ => {}
        }
    }
    if tab {
        let cur = q_input
            .iter()
            .find(|(i, ..)| i.focused)
            .map(|(i, ..)| i.order);
        let count = q_input.iter().count() as u8;
        if count > 0 {
            let next = cur.map(|c| (c + 1) % count).unwrap_or(0);
            for (mut inp, ..) in q_input.iter_mut() {
                inp.focused = inp.order == next;
            }
        }
    }
    // 渲染: 值/占位/焦点金边
    for (inp, _, mut border, children) in q_input.iter_mut() {
        border.0 = if inp.focused { EDGE_GOLD } else { EDGE_DARK };
        for child in children.iter() {
            if let Ok((mut t, mut color)) = q_display.get_mut(*child) {
                if inp.value.is_empty() {
                    **t = inp.placeholder.clone();
                    color.0 = DISABLED;
                } else if inp.password {
                    **t = "*".repeat(inp.value.chars().count());
                    color.0 = TEXT_MAIN;
                } else {
                    **t = inp.value.clone();
                    color.0 = TEXT_MAIN;
                }
            }
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

/// 输入框 (46px 高, #0e1017 底, 1px #2a2d3c 边, 3px 圆角)
fn spawn_input(
    parent: &mut ChildBuilder,
    skin: &Skin,
    placeholder: &str,
    password: bool,
    order: u8,
) {
    parent
        .spawn((
            Button,
            TextInput {
                value: String::new(),
                password,
                placeholder: placeholder.into(),
                focused: order == 0,
                order,
            },
            Node {
                height: Val::Px(46.0),
                border: UiRect::all(Val::Px(1.0)),
                align_items: AlignItems::Center,
                padding: UiRect::horizontal(Val::Px(14.0)),
                ..default()
            },
            BackgroundColor(INPUT_BG),
            BorderColor(EDGE_DARK),
            BorderRadius::all(Val::Px(3.0)),
        ))
        .with_children(|input| {
            input.spawn((
                text(&skin.font, placeholder, 15.0, DISABLED),
                TextInputDisplay,
            ));
        });
}

/// 金色主按钮 (#4a3c22→#241d10 渐变取中值, 1px 金边, 字距 8)
fn spawn_gold_btn(
    parent: &mut ChildBuilder,
    skin: &Skin,
    label: &str,
    height: f32,
    width: Option<f32>,
    marker: impl Component,
) {
    let mut node = Node {
        height: Val::Px(height),
        border: UiRect::all(Val::Px(1.0)),
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        ..default()
    };
    if let Some(w) = width {
        node.width = Val::Px(w);
    }
    parent
        .spawn((
            Button,
            marker,
            node,
            BackgroundColor(BTN_GOLD_BG),
            BorderColor(EDGE_GOLD),
            BorderRadius::all(Val::Px(3.0)),
        ))
        .with_children(|b| {
            b.spawn(text(&skin.font, label, 16.0, GOLD_BRIGHT));
        });
}

/// 幽灵按钮 (透明底 1px 暗边)
fn spawn_ghost_btn(
    parent: &mut ChildBuilder,
    skin: &Skin,
    label: &str,
    height: f32,
    width: Option<f32>,
    marker: impl Component,
) {
    let mut node = Node {
        height: Val::Px(height),
        border: UiRect::all(Val::Px(1.0)),
        justify_content: JustifyContent::Center,
        align_items: AlignItems::Center,
        ..default()
    };
    if let Some(w) = width {
        node.width = Val::Px(w);
    }
    parent
        .spawn((
            Button,
            marker,
            node,
            BackgroundColor(Color::NONE),
            BorderColor(EDGE_DARK),
            BorderRadius::all(Val::Px(3.0)),
        ))
        .with_children(|b| {
            b.spawn(text(&skin.font, label, 14.0, TEXT_DIM));
        });
}

// ─────────── 登录页 ───────────

#[derive(Component)]
pub struct LoginRoot;
#[derive(Component)]
pub struct LoginBtn;
#[derive(Component)]
pub struct RegisterBtn;
#[derive(Component)]
pub struct StatusText;

pub fn login_setup(mut commands: Commands, skin: Res<Skin>) {
    commands
        .spawn((
            LoginRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgb(0.043, 0.047, 0.086)), // 氛围底色 (渐变近似)
        ))
        .with_children(|root| {
            // 标题 (top 148, 88px 字距 26)
            root.spawn(Node {
                margin: UiRect::top(Val::Vh(13.7)),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(14.0),
                ..default()
            })
            .with_children(|t| {
                t.spawn(text(&skin.font, "传  奇", 88.0, TEXT_MAIN));
                t.spawn(Node {
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(16.0),
                    ..default()
                })
                .with_children(|sub| {
                    sub.spawn((
                        Node {
                            width: Val::Px(120.0),
                            height: Val::Px(1.0),
                            ..default()
                        },
                        BackgroundColor(GOLD),
                    ));
                    sub.spawn(text(&skin.font, "M I R F O R G E", 15.0, GOLD));
                    sub.spawn((
                        Node {
                            width: Val::Px(120.0),
                            height: Val::Px(1.0),
                            ..default()
                        },
                        BackgroundColor(GOLD),
                    ));
                });
            });
            // 登录卡 (440 宽 @top 39.8%, padding 40/44/36, gap 22)
            root.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Vh(39.8),
                    left: Val::Px(0.0),
                    right: Val::Px(0.0),
                    margin: UiRect::horizontal(Val::Auto),
                    width: Val::Px(440.0),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(22.0),
                    padding: UiRect::new(
                        Val::Px(44.0),
                        Val::Px(44.0),
                        Val::Px(40.0),
                        Val::Px(36.0),
                    ),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(CARD_BG),
                BorderColor(EDGE_GOLD),
            ))
            .with_children(|card| {
                crate::panels::ornate_corners(card);
                for (label, ph, pw, order) in [
                    ("账 号", "输入账号", false, 0u8),
                    ("密 码", "输入密码", true, 1u8),
                ] {
                    card.spawn(Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(8.0),
                        ..default()
                    })
                    .with_children(|field| {
                        field.spawn(text(&skin.font, label, 12.0, TEXT_DIM));
                        spawn_input(field, &skin, ph, pw, order);
                    });
                }
                card.spawn((text(&skin.font, "", 12.0, TEXT_DIM), StatusText));
                card.spawn(Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(12.0),
                    margin: UiRect::top(Val::Px(6.0)),
                    ..default()
                })
                .with_children(|btns| {
                    spawn_gold_btn(btns, &skin, "进 入 游 戏", 52.0, None, LoginBtn);
                    spawn_ghost_btn(btns, &skin, "注 册 账 号", 46.0, None, RegisterBtn);
                });
            });
            // 底部信息
            root.spawn((Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(26.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                justify_content: JustifyContent::Center,
                column_gap: Val::Px(28.0),
                ..default()
            },))
                .with_children(|f| {
                    f.spawn(text(&skin.font, "MirForge v0.1 · 开源引擎", 12.0, DISABLED));
                    f.spawn(text(
                        &skin.font,
                        "本引擎不含游戏资源 · 资源版权归其权利方所有",
                        12.0,
                        DISABLED,
                    ));
                });
        });
}

pub fn login_teardown(mut commands: Commands, q: Query<Entity, With<LoginRoot>>) {
    for e in &q {
        commands.entity(e).despawn_recursive();
    }
}

/// 登录页交互: 按钮点击 / Enter 提交 / 状态行
#[allow(clippy::type_complexity)]
pub fn login_update(
    net: Res<Net>,
    q_login: Query<&Interaction, (With<LoginBtn>, Changed<Interaction>)>,
    q_reg: Query<&Interaction, (With<RegisterBtn>, Changed<Interaction>)>,
    q_input: Query<&TextInput>,
    mut q_status: Query<(&mut Text, &mut TextColor), With<StatusText>>,
    mut keys: EventReader<KeyboardInput>,
) {
    let (mut user, mut pass) = (String::new(), String::new());
    for inp in &q_input {
        if inp.order == 0 {
            user = inp.value.clone();
        } else {
            pass = inp.value.clone();
        }
    }
    let ready = net.connected && !user.is_empty() && !pass.is_empty();
    let enter = keys
        .read()
        .any(|e| e.state.is_pressed() && e.logical_key == Key::Enter);
    let login = enter || q_login.iter().any(|it| *it == Interaction::Pressed);
    let register = q_reg.iter().any(|it| *it == Interaction::Pressed);
    if ready && login {
        net.send(ClientMessage::Login {
            username: user,
            password: pass,
        });
    } else if ready && register {
        net.send(ClientMessage::Register {
            username: user,
            password: pass,
        });
    }
    for (mut t, mut color) in q_status.iter_mut() {
        **t = net.status.clone();
        color.0 = if net.status.contains("失败") || net.status.contains("错误") {
            HP_RED
        } else {
            TEXT_DIM
        };
    }
}

// ─────────── 选角页 ───────────

#[derive(Component)]
pub struct CharSelectRoot;
#[derive(Component)]
pub struct CharListBody;
#[derive(Component)]
pub struct CharCard(pub usize);
#[derive(Component)]
pub struct CreateCard;
#[derive(Component)]
pub struct EnterGameBtn;
#[derive(Component)]
pub struct ShowcaseName;
#[derive(Component)]
pub struct ShowcaseInfo;
#[derive(Component)]
pub struct ClassOption(pub usize);
#[derive(Component)]
pub struct CreatePanel;
#[derive(Component)]
pub struct ConfirmCreateBtn;
#[derive(Component)]
pub struct CancelCreateBtn;
#[derive(Component)]
pub struct CsStatusText;

/// 选角页状态
#[derive(Resource, Default)]
pub struct CharSelectState {
    pub selected: usize,
    pub creating: bool,
    pub create_class: usize,
    /// 列表内容版本 (characters 变化时重建)
    seen_rev: u64,
}

const CLASSES: [(&str, protocol::CharacterClass); 3] = [
    ("战士", protocol::CharacterClass::Warrior),
    ("法师", protocol::CharacterClass::Mage),
    ("道士", protocol::CharacterClass::Taoist),
];

fn class_name(c: protocol::CharacterClass) -> &'static str {
    CLASSES
        .iter()
        .find(|(_, k)| *k == c)
        .map(|(n, _)| *n)
        .unwrap_or("?")
}

pub fn charselect_setup(
    mut commands: Commands,
    skin: Res<Skin>,
    mut state: ResMut<CharSelectState>,
) {
    state.creating = false;
    state.selected = 0;
    state.seen_rev = 0;
    commands
        .spawn((
            CharSelectRoot,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                ..default()
            },
            BackgroundColor(Color::srgb(0.047, 0.051, 0.094)),
        ))
        .with_children(|root| {
            // 顶部
            root.spawn(Node {
                position_type: PositionType::Absolute,
                top: Val::Vh(3.7),
                left: Val::Px(64.0),
                right: Val::Px(64.0),
                justify_content: JustifyContent::SpaceBetween,
                ..default()
            })
            .with_children(|top| {
                top.spawn(text(&skin.font, "选 择 角 色", 24.0, TEXT_MAIN));
                top.spawn(text(&skin.font, "玛法一区", 13.0, TEXT_DIM));
            });
            // 左: 角色列表容器
            root.spawn((
                CharListBody,
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(64.0),
                    top: Val::Vh(11.1),
                    width: Val::Px(380.0),
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(16.0),
                    ..default()
                },
            ));
            // 中: 展示台 (名字 + 信息; 立绘接入待角色渲染到 UI 支持)
            root.spawn(Node {
                position_type: PositionType::Absolute,
                left: Val::Px(444.0),
                right: Val::Px(0.0),
                bottom: Val::Vh(13.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                row_gap: Val::Px(6.0),
                ..default()
            })
            .with_children(|show| {
                show.spawn((text(&skin.font, "", 30.0, TEXT_MAIN), ShowcaseName));
                show.spawn((text(&skin.font, "", 13.0, TEXT_DIM), ShowcaseInfo));
            });
            // 底部操作
            root.spawn(Node {
                position_type: PositionType::Absolute,
                left: Val::Px(444.0),
                right: Val::Px(0.0),
                bottom: Val::Vh(5.2),
                justify_content: JustifyContent::Center,
                column_gap: Val::Px(20.0),
                ..default()
            })
            .with_children(|ops| {
                spawn_gold_btn(ops, &skin, "进 入 游 戏", 54.0, Some(260.0), EnterGameBtn);
            });
            // 状态行
            root.spawn((Node {
                position_type: PositionType::Absolute,
                left: Val::Px(444.0),
                right: Val::Px(0.0),
                bottom: Val::Vh(11.1),
                justify_content: JustifyContent::Center,
                ..default()
            },))
                .with_children(|s| {
                    s.spawn((text(&skin.font, "", 13.0, TEXT_DIM), CsStatusText));
                });
        });
}

pub fn charselect_teardown(mut commands: Commands, q: Query<Entity, With<CharSelectRoot>>) {
    for e in &q {
        commands.entity(e).despawn_recursive();
    }
}

/// 列表重建 + 展示台 + 交互
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn charselect_update(
    mut commands: Commands,
    skin: Res<Skin>,
    mut net: ResMut<Net>,
    mut state: ResMut<CharSelectState>,
    q_list: Query<Entity, With<CharListBody>>,
    q_cards: Query<(&Interaction, &CharCard), Changed<Interaction>>,
    q_create: Query<&Interaction, (With<CreateCard>, Changed<Interaction>)>,
    q_enter: Query<&Interaction, (With<EnterGameBtn>, Changed<Interaction>)>,
    mut q_show: Query<
        (
            &mut Text,
            Option<&ShowcaseName>,
            Option<&ShowcaseInfo>,
            Option<&CsStatusText>,
        ),
        Or<(With<ShowcaseName>, With<ShowcaseInfo>, With<CsStatusText>)>,
    >,
    mut keys: EventReader<KeyboardInput>,
) {
    // 点选角色卡
    let mut dirty = false;
    for (it, card) in &q_cards {
        if *it == Interaction::Pressed && state.selected != card.0 {
            state.selected = card.0;
            dirty = true;
        }
    }
    // 创建新角色卡 → 弹创建层
    if q_create.iter().any(|it| *it == Interaction::Pressed) && !state.creating {
        state.creating = true;
        spawn_create_panel(&mut commands, &skin);
    }
    // 进入游戏 (按钮或 Enter)
    let enter_key = keys
        .read()
        .any(|e| e.state.is_pressed() && e.logical_key == Key::Enter);
    if q_enter.iter().any(|it| *it == Interaction::Pressed) || (enter_key && !state.creating) {
        if let Some(id) = net.characters.get(state.selected).map(|c| c.id.clone()) {
            net.character_id = Some(id.clone());
            net.send(ClientMessage::SelectCharacter { character_id: id });
            net.status = "进入游戏...".into();
        }
    }
    // 列表重建 (角色数据或选中变化)
    let rev = net.characters.len() as u64 * 1000 + state.selected as u64;
    if rev != state.seen_rev || dirty {
        state.seen_rev = rev;
        for e in &q_list {
            let mut ec = commands.entity(e);
            ec.despawn_descendants();
            ec.with_children(|list| {
                for (i, c) in net.characters.iter().enumerate() {
                    let selected = i == state.selected;
                    list.spawn((
                        Button,
                        CharCard(i),
                        Node {
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(16.0),
                            padding: UiRect::all(Val::Px(18.0)),
                            border: UiRect::all(Val::Px(1.0)),
                            ..default()
                        },
                        BackgroundColor(if selected { SEL_BG } else { LIST_BG }),
                        BorderColor(if selected { GOLD } else { EDGE_DARK }),
                        BorderRadius::all(Val::Px(4.0)),
                    ))
                    .with_children(|card| {
                        // 头像圆
                        card.spawn((
                            Node {
                                width: Val::Px(64.0),
                                height: Val::Px(64.0),
                                border: UiRect::all(Val::Px(2.0)),
                                justify_content: JustifyContent::Center,
                                align_items: AlignItems::Center,
                                ..default()
                            },
                            BackgroundColor(Color::srgb(0.114, 0.122, 0.180)),
                            BorderColor(if selected { GOLD } else { EDGE_DARK }),
                            BorderRadius::all(Val::Percent(50.0)),
                        ))
                        .with_children(|av| {
                            av.spawn(text(
                                &skin.font,
                                class_name(c.class).chars().next().unwrap().to_string(),
                                24.0,
                                if selected { GOLD_BRIGHT } else { TEXT_DIM },
                            ));
                        });
                        card.spawn(Node {
                            flex_direction: FlexDirection::Column,
                            row_gap: Val::Px(4.0),
                            ..default()
                        })
                        .with_children(|info| {
                            info.spawn(Node {
                                align_items: AlignItems::Baseline,
                                column_gap: Val::Px(10.0),
                                ..default()
                            })
                            .with_children(|nameline| {
                                nameline.spawn(text(
                                    &skin.font,
                                    c.name.clone(),
                                    18.0,
                                    if selected { TEXT_MAIN } else { TEXT_SUB },
                                ));
                                nameline
                                    .spawn((
                                        Node {
                                            border: UiRect::all(Val::Px(1.0)),
                                            padding: UiRect::axes(Val::Px(8.0), Val::Px(1.0)),
                                            ..default()
                                        },
                                        BorderColor(if selected { EDGE_GOLD } else { EDGE_DARK }),
                                        BorderRadius::all(Val::Px(9.0)),
                                    ))
                                    .with_children(|lv| {
                                        lv.spawn(text(
                                            &skin.font,
                                            format!("Lv {}", c.level),
                                            12.0,
                                            if selected { GOLD_BRIGHT } else { TEXT_DIM },
                                        ));
                                    });
                            });
                            info.spawn(text(
                                &skin.font,
                                format!(
                                    "{} · {}",
                                    class_name(c.class),
                                    if c.gender == "female" { "女" } else { "男" }
                                ),
                                12.0,
                                TEXT_DIM,
                            ));
                        });
                    });
                }
                // 创建新角色 (虚线样式近似: 暗边)
                list.spawn((
                    Button,
                    CreateCard,
                    Node {
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        padding: UiRect::all(Val::Px(24.0)),
                        border: UiRect::all(Val::Px(1.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.051, 0.059, 0.094, 0.4)),
                    BorderColor(Color::srgb(0.227, 0.239, 0.314)), // #3a3d50
                    BorderRadius::all(Val::Px(4.0)),
                ))
                .with_children(|c| {
                    c.spawn(text(&skin.font, "+  创建新角色", 14.0, TEXT_DIM));
                });
            });
        }
    }
    // 展示台 + 状态行
    let sel = net.characters.get(state.selected);
    for (mut t, name, info, status) in q_show.iter_mut() {
        if name.is_some() {
            **t = sel.map(|c| c.name.clone()).unwrap_or_default();
        } else if info.is_some() {
            **t = sel
                .map(|c| format!("{} · Lv {}", class_name(c.class), c.level))
                .unwrap_or_else(|| "创建一个角色开始冒险".into());
        } else if status.is_some() {
            **t = net.status.clone();
        }
    }
}

/// 创建角色弹层 (居中卡: 名字 + 三职业 + 确认/取消)
fn spawn_create_panel(commands: &mut Commands, skin: &Skin) {
    commands
        .spawn((
            CreatePanel,
            CharSelectRoot, // 随选角页一并回收
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(0.0),
                top: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.55)),
            GlobalZIndex(20),
        ))
        .with_children(|overlay| {
            overlay
                .spawn((
                    Node {
                        width: Val::Px(440.0),
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(20.0),
                        padding: UiRect::all(Val::Px(40.0)),
                        border: UiRect::all(Val::Px(1.0)),
                        ..default()
                    },
                    BackgroundColor(CARD_BG),
                    BorderColor(EDGE_GOLD),
                ))
                .with_children(|card| {
                    crate::panels::ornate_corners(card);
                    card.spawn(text(&skin.font, "创 建 角 色", 20.0, TEXT_MAIN));
                    card.spawn(Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(8.0),
                        ..default()
                    })
                    .with_children(|f| {
                        f.spawn(text(&skin.font, "角 色 名", 12.0, TEXT_DIM));
                        spawn_input(f, skin, "输入角色名", false, 10);
                    });
                    card.spawn(Node {
                        column_gap: Val::Px(12.0),
                        ..default()
                    })
                    .with_children(|classes| {
                        for (i, (name, _)) in CLASSES.iter().enumerate() {
                            classes
                                .spawn((
                                    Button,
                                    ClassOption(i),
                                    Node {
                                        flex_grow: 1.0,
                                        height: Val::Px(48.0),
                                        justify_content: JustifyContent::Center,
                                        align_items: AlignItems::Center,
                                        border: UiRect::all(Val::Px(1.0)),
                                        ..default()
                                    },
                                    BackgroundColor(INPUT_BG),
                                    BorderColor(if i == 0 { EDGE_GOLD } else { EDGE_DARK }),
                                    BorderRadius::all(Val::Px(3.0)),
                                ))
                                .with_children(|b| {
                                    b.spawn(text(
                                        &skin.font,
                                        *name,
                                        15.0,
                                        if i == 0 { GOLD_BRIGHT } else { TEXT_DIM },
                                    ));
                                });
                        }
                    });
                    card.spawn(Node {
                        column_gap: Val::Px(12.0),
                        margin: UiRect::top(Val::Px(6.0)),
                        ..default()
                    })
                    .with_children(|btns| {
                        btns.spawn(Node {
                            flex_grow: 1.0,
                            ..default()
                        })
                        .with_children(|w| {
                            spawn_gold_btn(w, skin, "创 建", 48.0, None, ConfirmCreateBtn);
                        });
                        btns.spawn(Node {
                            flex_grow: 1.0,
                            ..default()
                        })
                        .with_children(|w| {
                            spawn_ghost_btn(w, skin, "取 消", 48.0, None, CancelCreateBtn);
                        });
                    });
                });
        });
}

/// 创建弹层交互
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn create_panel_update(
    mut commands: Commands,
    net: Res<Net>,
    mut state: ResMut<CharSelectState>,
    mut q_class: Query<(&Interaction, &ClassOption, &mut BorderColor, &Children)>,
    mut q_class_text: Query<&mut TextColor>,
    q_confirm: Query<&Interaction, (With<ConfirmCreateBtn>, Changed<Interaction>)>,
    q_cancel: Query<&Interaction, (With<CancelCreateBtn>, Changed<Interaction>)>,
    q_input: Query<&TextInput>,
    q_panel: Query<Entity, With<CreatePanel>>,
) {
    if !state.creating {
        return;
    }
    // 职业选择
    for (it, opt, ..) in q_class.iter() {
        if *it == Interaction::Pressed {
            state.create_class = opt.0;
        }
    }
    for (_, opt, mut border, children) in q_class.iter_mut() {
        let sel = opt.0 == state.create_class;
        border.0 = if sel { EDGE_GOLD } else { EDGE_DARK };
        for child in children.iter() {
            if let Ok(mut color) = q_class_text.get_mut(*child) {
                color.0 = if sel { GOLD_BRIGHT } else { TEXT_DIM };
            }
        }
    }
    // 确认 / 取消
    let name = q_input
        .iter()
        .find(|i| i.order == 10)
        .map(|i| i.value.clone())
        .unwrap_or_default();
    let confirm = q_confirm.iter().any(|it| *it == Interaction::Pressed);
    let cancel = q_cancel.iter().any(|it| *it == Interaction::Pressed);
    if confirm && !name.is_empty() {
        net.send(ClientMessage::CreateCharacter {
            name,
            class: CLASSES[state.create_class].1,
            gender: "male".into(),
        });
        state.creating = false;
        for e in &q_panel {
            commands.entity(e).despawn_recursive();
        }
    } else if cancel {
        state.creating = false;
        for e in &q_panel {
            commands.entity(e).despawn_recursive();
        }
    }
}
