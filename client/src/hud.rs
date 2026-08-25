//! 方向 A「鎏金经典」HUD —— Bevy UI + 9-slice 皮肤 (client/assets/ui/skin)。
//! 结构: 中央头像金环+等级徽章, 左翼 HP / 右翼 MP, 技能格, EXP 细条贯底,
//! 右上通知堆栈。数据全部来自 Net (PlayerStatus/SkillList/Notification)。

use bevy::prelude::*;
use bevy::ui::widget::NodeImageMode;

use crate::Net;

// ── 设计令牌 (skin.json colors) ──
pub const GOLD_BRIGHT: Color = Color::srgb(1.0, 0.847, 0.463); // #ffd876
pub const TEXT_MAIN: Color = Color::srgb(0.910, 0.886, 0.816); // #e8e2d0
pub const TEXT_DIM: Color = Color::srgb(0.604, 0.569, 0.486); // #9a917c
pub const DISABLED: Color = Color::srgb(0.357, 0.337, 0.282); // #5b5648
pub const HP_RED: Color = Color::srgb(0.878, 0.251, 0.251); // #e04040
pub const EXP_GOLD: Color = Color::srgb(0.933, 0.804, 0.322); // #eecd52
pub const LOOT_GREEN: Color = Color::srgb(0.482, 0.847, 0.561); // #7bd88f

/// 皮肤句柄 + 中文字体 (系统字体运行时加载, 不入库)
#[derive(Resource)]
pub struct Skin {
    pub font: Handle<Font>,
    panel_glass: Handle<Image>,
    bar_frame: Handle<Image>,
    bar_hp: Handle<Image>,
    bar_mp: Handle<Image>,
    bar_exp: Handle<Image>,
    avatar_ring: Handle<Image>,
    badge: Handle<Image>,
    slot: Handle<Image>,
}

fn sliced(border: f32) -> NodeImageMode {
    NodeImageMode::Sliced(TextureSlicer {
        border: BorderRect::square(border),
        ..default()
    })
}

/// 载入皮肤与中文字体
pub fn load_skin(
    mut commands: Commands,
    assets: Res<AssetServer>,
    mut fonts: ResMut<Assets<Font>>,
) {
    // 系统中文字体 (simhei 是普通 ttf, Bevy 可直载; msyh.ttc 不支持)
    let font = [
        "C:/Windows/Fonts/simhei.ttf",
        "C:/Windows/Fonts/msyhl.ttc",
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
        panel_glass: assets.load("ui/skin/panel_glass.png"),
        bar_frame: assets.load("ui/skin/bar_frame.png"),
        bar_hp: assets.load("ui/skin/bar_fill_hp.png"),
        bar_mp: assets.load("ui/skin/bar_fill_mp.png"),
        bar_exp: assets.load("ui/skin/bar_fill_exp.png"),
        avatar_ring: assets.load("ui/skin/avatar_ring.png"),
        badge: assets.load("ui/skin/badge.png"),
        slot: assets.load("ui/skin/slot.png"),
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
pub struct SkillName(usize);
#[derive(Component)]
pub struct SkillKey(usize);
#[derive(Component)]
pub struct NoticeLine(usize);

/// 血/蓝条: bar_frame 9-slice 外框 + 内部填充 + 居中数字
fn spawn_bar(
    parent: &mut ChildBuilder,
    skin: &Skin,
    fill_img: Handle<Image>,
    fill_marker: impl Component,
    text_marker: impl Component,
    width: f32,
) {
    parent
        .spawn((
            Node {
                width: Val::Px(width),
                height: Val::Px(20.0),
                ..default()
            },
            ImageNode::new(skin.bar_frame.clone()).with_mode(sliced(6.0)),
        ))
        .with_children(|bar| {
            bar.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(3.0),
                    top: Val::Px(3.0),
                    bottom: Val::Px(3.0),
                    width: Val::Percent(96.0),
                    ..default()
                },
                ImageNode::new(fill_img).with_mode(sliced(5.0)),
                fill_marker,
            ));
            bar.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(0.0),
                    right: Val::Px(0.0),
                    top: Val::Px(2.0),
                    justify_content: JustifyContent::Center,
                    ..default()
                },
                Text::new(""),
                TextFont {
                    font: skin.font.clone(),
                    font_size: 12.0,
                    ..default()
                },
                TextColor(TEXT_MAIN),
                text_marker,
            ));
        });
}

/// 进入游戏时构建 HUD
pub fn setup(mut commands: Commands, skin: Res<Skin>) {
    // 根容器: 底部居中列 (EXP 细条 + 动作条)
    commands
        .spawn((
            HudRoot,
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(0.0),
                left: Val::Px(0.0),
                right: Val::Px(0.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                ..default()
            },
        ))
        .with_children(|root| {
            // 动作条主体 (玻璃面板)
            root.spawn((
                Node {
                    flex_direction: FlexDirection::Row,
                    align_items: AlignItems::Center,
                    column_gap: Val::Px(14.0),
                    padding: UiRect::axes(Val::Px(18.0), Val::Px(8.0)),
                    ..default()
                },
                ImageNode::new(skin.panel_glass.clone()).with_mode(sliced(8.0)),
            ))
            .with_children(|bar| {
                // 左翼: HP
                spawn_bar(bar, &skin, skin.bar_hp.clone(), HpFill, HpText, 220.0);
                // 中央: 头像金环 + 等级徽章
                bar.spawn(Node {
                    width: Val::Px(76.0),
                    height: Val::Px(76.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::FlexEnd,
                    ..default()
                })
                .with_children(|av| {
                    av.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(0.0),
                            top: Val::Px(0.0),
                            width: Val::Px(76.0),
                            height: Val::Px(76.0),
                            ..default()
                        },
                        ImageNode::new(skin.avatar_ring.clone()),
                    ));
                    av.spawn((
                        Node {
                            padding: UiRect::axes(Val::Px(10.0), Val::Px(2.0)),
                            margin: UiRect::bottom(Val::Px(-4.0)),
                            ..default()
                        },
                        ImageNode::new(skin.badge.clone()).with_mode(sliced(9.0)),
                    ))
                    .with_children(|b| {
                        b.spawn((
                            Text::new("Lv 1"),
                            TextFont {
                                font: skin.font.clone(),
                                font_size: 13.0,
                                ..default()
                            },
                            TextColor(GOLD_BRIGHT),
                            LvText,
                        ));
                    });
                });
                // 右翼: MP
                spawn_bar(bar, &skin, skin.bar_mp.clone(), MpFill, MpText, 220.0);
                // 技能格 ×3
                for i in 0..3 {
                    bar.spawn((
                        Node {
                            width: Val::Px(52.0),
                            height: Val::Px(52.0),
                            flex_direction: FlexDirection::Column,
                            justify_content: JustifyContent::SpaceBetween,
                            align_items: AlignItems::Center,
                            padding: UiRect::all(Val::Px(4.0)),
                            ..default()
                        },
                        ImageNode::new(skin.slot.clone()).with_mode(sliced(6.0)),
                    ))
                    .with_children(|slot| {
                        slot.spawn((
                            Text::new(""),
                            TextFont {
                                font: skin.font.clone(),
                                font_size: 12.0,
                                ..default()
                            },
                            TextColor(TEXT_MAIN),
                            SkillName(i),
                        ));
                        slot.spawn((
                            Text::new(format!("{}", i + 1)),
                            TextFont {
                                font: skin.font.clone(),
                                font_size: 11.0,
                                ..default()
                            },
                            TextColor(TEXT_DIM),
                            SkillKey(i),
                        ));
                    });
                }
            });
            // EXP 细条贯底
            root.spawn((
                Node {
                    width: Val::Percent(62.0),
                    height: Val::Px(8.0),
                    margin: UiRect::top(Val::Px(2.0)),
                    ..default()
                },
                ImageNode::new(skin.bar_frame.clone()).with_mode(sliced(6.0)),
            ))
            .with_children(|bar| {
                bar.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(2.0),
                        top: Val::Px(2.0),
                        bottom: Val::Px(2.0),
                        width: Val::Percent(0.0),
                        ..default()
                    },
                    ImageNode::new(skin.bar_exp.clone()).with_mode(sliced(2.0)),
                    ExpFill,
                ));
            });
        });
    // 右上通知堆栈 (固定 6 行)
    commands
        .spawn((
            HudRoot,
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(18.0),
                top: Val::Px(64.0),
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::FlexEnd,
                row_gap: Val::Px(4.0),
                ..default()
            },
        ))
        .with_children(|col| {
            for i in 0..6 {
                col.spawn((
                    Text::new(""),
                    TextFont {
                        font: skin.font.clone(),
                        font_size: 16.0,
                        ..default()
                    },
                    TextColor(TEXT_MAIN),
                    NoticeLine(i),
                ));
            }
        });
}

pub fn teardown(mut commands: Commands, q: Query<Entity, With<HudRoot>>) {
    for e in &q {
        commands.entity(e).despawn_recursive();
    }
}

/// 数据 → HUD (每帧)
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub fn update(
    time: Res<Time>,
    mut net: ResMut<Net>,
    mut q_hp_fill: Query<&mut Node, (With<HpFill>, Without<MpFill>, Without<ExpFill>)>,
    mut q_mp_fill: Query<&mut Node, (With<MpFill>, Without<HpFill>, Without<ExpFill>)>,
    mut q_exp_fill: Query<&mut Node, (With<ExpFill>, Without<HpFill>, Without<MpFill>)>,
    mut q_texts: Query<(
        &mut Text,
        &mut TextColor,
        Option<&HpText>,
        Option<&MpText>,
        Option<&LvText>,
        Option<&SkillName>,
        Option<&SkillKey>,
        Option<&NoticeLine>,
    )>,
) {
    let now = time.elapsed_secs_f64();
    net.notices.retain(|(_, _, born)| now - born < 4.0);
    let stat = net.stat;
    if let Some(s) = stat {
        let hp_frac = (s.hp.max(0) as f32 / s.max_hp.max(1) as f32).clamp(0.0, 1.0);
        let mp_frac = (s.mp.max(0) as f32 / s.max_mp.max(1) as f32).clamp(0.0, 1.0);
        let exp_frac = (s.exp as f32 / s.req.max(1) as f32).clamp(0.0, 1.0);
        for mut n in q_hp_fill.iter_mut() {
            n.width = Val::Percent(96.0 * hp_frac);
        }
        for mut n in q_mp_fill.iter_mut() {
            n.width = Val::Percent(96.0 * mp_frac);
        }
        for mut n in q_exp_fill.iter_mut() {
            n.width = Val::Percent(99.0 * exp_frac);
        }
    }
    for (mut text, mut color, hp, mp, lv, sname, skey, notice) in q_texts.iter_mut() {
        if hp.is_some() {
            if let Some(s) = stat {
                **text = format!("{}/{}", s.hp, s.max_hp);
            }
        } else if mp.is_some() {
            if let Some(s) = stat {
                **text = format!("{}/{}", s.mp, s.max_mp);
            }
        } else if lv.is_some() {
            if let Some(s) = stat {
                **text = format!("Lv {}", s.level);
            }
        } else if let Some(SkillName(i)) = sname {
            **text = net
                .skills
                .get(*i)
                .map(|s| s.name.clone())
                .unwrap_or_default();
        } else if let Some(SkillKey(i)) = skey {
            // 冷却剩余显示秒数, 否则显示键位
            let remain = net
                .skills
                .get(*i)
                .and_then(|s| net.cds.get(&s.id))
                .map(|&t| t - now)
                .unwrap_or(0.0);
            if remain > 0.0 {
                **text = format!("{remain:.1}");
                color.0 = DISABLED;
            } else {
                **text = format!("{}", i + 1);
                color.0 = TEXT_DIM;
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
                    **text = msg.clone();
                    color.0 = c;
                }
                None => {
                    **text = String::new();
                }
            }
        }
    }
}
