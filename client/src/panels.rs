//! 方向 A 面板族：背包 (B) / 角色 (C) / 任务 (L)。
//! M4.5 重做: 逐参数对齐 design/Panels.dc.html —— 金角饰线面板框、
//! 48px 标题栏 (可拖拽/关闭)、8 列背包格、经典 F10 装备环绕布局、
//! 两列属性; 任务面板沿同风格自延 (设计稿未含)。

use bevy::math::DVec2;
use bevy::prelude::*;
use bevy::ui::widget::NodeImageMode;
use bevy::ui::RelativeCursorPosition;

use crate::hud::HP_RED;
use crate::hud::{
    Skin, DISABLED, EDGE_DARK, EDGE_GOLD, EXP_GOLD, GOLD, GOLD_BRIGHT, TEXT_DIM, TEXT_MAIN,
};
use crate::Net;
use protocol::ClientMessage;

/// 程序生成的 UI 皮肤贴图 (启动时一次, 各窗口共用)
#[derive(Resource)]
pub struct WoodTex {
    /// 平铺木纹
    pub wood: Handle<Image>,
    /// 金边 (9-slice, 1px 金线 + 透明中心)
    pub frame: Handle<Image>,
}

/// 生成 128×128 可平铺竖向拉丝纹: 一维噪声竖条 + 低幅云雾 + 细噪声
///
/// 没有外部素材可用 (仓库不收商业资源), 只能程序造。主纹是只随 x 变化、
/// 整列同值的细密竖条, 像拉丝硬木; 再叠一层低幅分形噪声打破死板。
/// 噪声格点按 tile 尺寸取周期, 平铺无缝。
pub fn make_wood(mut commands: Commands, mut images: ResMut<Assets<bevy::image::Image>>) {
    const W: u32 = 128;
    const H: u32 = 128;
    fn hash(x: u32, y: u32, s: u32) -> f32 {
        let mut h = x.wrapping_mul(374_761_393)
            ^ y.wrapping_mul(668_265_263)
            ^ s.wrapping_mul(2_246_822_519);
        h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
        ((h ^ (h >> 16)) & 0xffff) as f32 / 65535.0
    }
    // 周期格点值噪声: u/v ∈ [0,1), 格点取模所以平铺无缝
    fn vnoise(u: f32, v: f32, cells: u32, s: u32) -> f32 {
        let (gx, gy) = (u * cells as f32, v * cells as f32);
        let (x0, y0) = (gx.floor() as u32, gy.floor() as u32);
        let (tx, ty) = (gx - gx.floor(), gy - gy.floor());
        let (tx, ty) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
        let c = |dx: u32, dy: u32| hash((x0 + dx) % cells, (y0 + dy) % cells, s);
        let a = c(0, 0) * (1.0 - tx) + c(1, 0) * tx;
        let b = c(0, 1) * (1.0 - tx) + c(1, 1) * tx;
        a * (1.0 - ty) + b * ty
    }
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        let v = y as f32 / H as f32;
        for x in 0..W {
            let u = x as f32 / W as f32;
            // 竖条主纹: 一维噪声, 同列同值
            let streak = vnoise(u, 0.0, 48, 66) - 0.5;
            // 低幅云雾: 3 层八度, 防止竖条过于机械
            let (mut n, mut amp, mut tot) = (0.0f32, 1.0f32, 0.0f32);
            for o in 0..3u32 {
                n += amp * vnoise(u, v, 4 << o, 70 + o);
                tot += amp;
                amp *= 0.5;
            }
            let grain =
                streak * 0.14 + (n / tot - 0.5) * 0.05 + (hash(x, y, 99) - 0.5) * 0.05;
            let k = 0.92 + grain;
            for base in [0.165f32, 0.105, 0.075] {
                rgba.push(((base * k).clamp(0.0, 1.0) * 255.0) as u8);
            }
            rgba.push(255);
        }
    }
    let mk = |w: u32, h: u32, rgba: Vec<u8>, images: &mut Assets<bevy::image::Image>| {
        images.add(bevy::image::Image::new(
            bevy::render::render_resource::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            bevy::render::render_resource::TextureDimension::D2,
            rgba,
            bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
            bevy::asset::RenderAssetUsages::RENDER_WORLD,
        ))
    };
    let wood = mk(W, H, rgba, &mut images);

    // 金属边框: 12×12, 外圈 1px 单色金线, 中心透明。
    // 9-slice 由 GPU 采样缩放, 四边共用同一套几何 —— 逐节点画边在分数缩放下
    // 四边各自取整, 左/上与右/下会差 1-2px, 这里从机制上避开。
    const F: u32 = 12;
    let ring = [
        (201, 165, 92u8), // 单色金线
    ];
    let mut fr = Vec::with_capacity((F * F * 4) as usize);
    for y in 0..F {
        for x in 0..F {
            let d = x.min(y).min(F - 1 - x).min(F - 1 - y);
            if (d as usize) < ring.len() {
                let (r, g, b) = ring[d as usize];
                fr.extend_from_slice(&[r, g, b, 255]);
            } else {
                fr.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    let frame = mk(F, F, fr, &mut images);
    commands.insert_resource(WoodTex { wood, frame });
}

/// 木板底: 铺满窗口内区。必须在其它子节点之前生成 (先画者在下层)
///
/// inset 必须是 0, 不能向外出血: 绝对子节点的原点在父边框内侧, 负 inset 会
/// 延伸到父边框之上——子节点画在父边框上层, 曾把 2px 金色主边整个盖成木纹
/// (看起来像"金线外多了一圈深色", 而那条细金线其实只是内圈高光)。
/// 防透底靠双保险: 本节点自带不透明底色 + root 自身的不透明 PANEL_BG,
/// 即便某个缩放下子矩形取整偏短, 露出的也是深木色而非场景。
pub fn wood_bg(parent: &mut ChildBuilder, wood: &WoodTex) {
    parent.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            top: Val::Px(0.0),
            bottom: Val::Px(0.0),
            ..default()
        },
        ImageNode {
            image: wood.wood.clone(),
            image_mode: NodeImageMode::Tiled {
                tile_x: true,
                tile_y: true,
                stretch_value: 1.0,
            },
            ..default()
        },
        BackgroundColor(PANEL_BG),
    ));
}

/// 人物面板: 装备槽边长 (侧列与底排统一)
const CHAR_SLOT: f32 = 52.0;
/// 槽间距
const CHAR_GAP: f32 = 12.0;
/// 内容区横向内边距
const CHAR_PAD: f32 = 14.0;
/// 内容区宽 = 底排 5 槽 + 4 间隙; 侧列与底排都以它对齐
const CHAR_INNER: f32 = CHAR_SLOT * 5.0 + CHAR_GAP * 4.0;
/// 面板宽 = 内容区 + 内边距 + 1px 金线×2
const CHAR_PANEL_W: f32 = CHAR_INNER + CHAR_PAD * 2.0 + 2.0;

/// 品质·白 (物品品质字段接入前统一用)
const QUALITY_COMMON: Color = Color::srgb(0.812, 0.784, 0.706); // #cfc8b4
const PANEL_BG: Color = Color::srgb(0.152, 0.097, 0.069); // 深栗色, 不透明, 与皮革贴图同调
const SLOT_BG: Color = Color::srgb(0.055, 0.063, 0.090); // #0e1017
/// 标题栏: 半透明压暗层叠在木纹上, 分出题区又不盖掉纹理
const TITLE_BG: Color = Color::srgba(0.0, 0.0, 0.0, 0.38);
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
/// 背包中的物品格 (格内物品 id)
#[derive(Component)]
pub struct EquipItem(String);
/// 人物面板上的装备栏 (对应服务端 slot 键); 空栏也带, 用作放置目标
#[derive(Component)]
pub struct EquipSlotTarget(&'static str);
/// 挡住世界点击的 UI 区域 (面板本体 / HUD 各块)
#[derive(Component)]
pub struct UiBlock;
#[derive(Component)]
pub struct QuestAction {
    quest: String,
    /// accept / complete / abandon
    act: &'static str,
}

/// 拖拽状态 (光标相对面板左上角的偏移)
#[derive(Resource, Default)]
pub struct Drag(Option<(Entity, Vec2)>);

/// 物品从哪里被提起 — 决定放下时要发什么消息
#[derive(Clone, PartialEq)]
pub enum GrabFrom {
    Bag,
    Equip(&'static str),
}

/// 提在光标上的物品 (经典传奇: 左键提起 → 移到目标 → 左键放下)
///
/// 提起纯粹是客户端状态, 服务端背包不动; 只有真正放到装备栏/地上时才发消息。
#[derive(Resource, Default)]
pub struct Grab {
    pub item: Option<protocol::ItemInfo>,
    pub from: Option<GrabFrom>,
    /// 版本号 — 变化时重建面板 (源格子要显示为空)
    pub rev: u32,
    /// 本帧刚放下: 抑制这一次点击触发走路
    pub released: bool,
}

impl Grab {
    fn take(&mut self) {
        self.item = None;
        self.from = None;
        self.rev = self.rev.wrapping_add(1);
        self.released = true;
    }
}

/// 光标是否停在 UI 上 (面板/HUD) — 世界点击与丢弃判定都看它
#[derive(Resource, Default)]
pub struct UiHover(pub bool);

/// 每帧汇总: 光标是否落在任一 UiBlock 区域内
pub fn ui_hover(mut hover: ResMut<UiHover>, q: Query<&RelativeCursorPosition, With<UiBlock>>) {
    hover.0 = q.iter().any(|r| r.mouse_over());
}

/// 进入游戏时预建三面板 (默认隐藏), 位置为设计稿坐标
pub fn setup(mut commands: Commands, skin: Res<Skin>, wood: Res<WoodTex>) {
    spawn_panel(
        &mut commands,
        &skin,
        &wood,
        PanelKind::Bag,
        "背 包",
        (468.0, 220.0),
        BAG_PANEL_W,
    );
    spawn_panel(
        &mut commands,
        &skin,
        &wood,
        PanelKind::Character,
        "角 色",
        (1010.0, 180.0),
        CHAR_PANEL_W,
    );
    spawn_panel(
        &mut commands,
        &skin,
        &wood,
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

/// 金边: 一张 9-slice 边框贴图铺满窗口 (1px 单色金线, 中心透明)。
///
/// 之前做过 4/3/2px 的渐变金属环, 按反馈逐步减到单色细线。仍走贴图而非
/// 原生 border, 是为了保住四边等厚 (原生边框在分数缩放下左右会差 1-2px)。
///
/// 深金→金→亮金的渐变烙在贴图里, 内亮外沉的浮雕感由像素承载;
/// GPU 采样保证四边等厚 (逐节点画边在分数缩放下会左右不对称)。
pub fn metal_frame(panel: &mut ChildBuilder, wood: &WoodTex) {
    panel.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            top: Val::Px(0.0),
            bottom: Val::Px(0.0),
            ..default()
        },
        ImageNode {
            image: wood.frame.clone(),
            image_mode: crate::hud::sliced(1.0),
            ..default()
        },
    ));
}

fn spawn_panel(
    commands: &mut Commands,
    skin: &Skin,
    wood: &WoodTex,
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
                padding: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            GlobalZIndex(10),
            UiBlock,
            RelativeCursorPosition::default(),
        ))
        .with_children(|panel| {
            wood_bg(panel, wood);
            metal_frame(panel, wood);
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

/// 开发钩子: MIRFORGE_PANELS=bclm — 进图后自动展开对应面板/大地图 (配合 MIRFORGE_SHOT 做视觉自查)
pub fn dev_open(
    mut q: Query<(&Panel, &mut Node)>,
    mut q_map: Query<&mut Node, (With<BigMapRoot>, Without<Panel>)>,
    mut done: Local<bool>,
) {
    // 面板在进图时才建, 所以要等查询非空才算生效
    if *done || q.is_empty() {
        return;
    }
    let Ok(spec) = std::env::var("MIRFORGE_PANELS") else {
        *done = true;
        return;
    };
    *done = true;
    let spec = spec.to_lowercase();
    for (p, mut node) in q.iter_mut() {
        let key = match p.0 {
            PanelKind::Bag => 'b',
            PanelKind::Character => 'c',
            PanelKind::Quest => 'l',
        };
        if spec.contains(key) {
            node.display = Display::Flex;
        }
    }
    if spec.contains('m') {
        for mut node in q_map.iter_mut() {
            node.display = Display::Flex;
        }
    }
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
    ui_scale: Res<UiScale>,
    buttons: Res<ButtonInput<MouseButton>>,
    q_bar: Query<(&Interaction, &Parent), With<DragBar>>,
    mut q_panel: Query<&mut Node, With<Panel>>,
) {
    // 必须换算成逻辑像素: node.left/top 是逻辑单位, 直接用物理光标坐标会让
    // 面板以 1/UiScale 的倍率跑得比鼠标快 (900p 下约 1.2 倍)
    let Some((cx, cy, _, _)) = cursor_logical(&windows, ui_scale.0) else {
        return;
    };
    let cursor = Vec2::new(cx, cy);
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

/// 经典传奇物品操作: 左键提起 → 跟随光标 → 左键放到目标
///
/// 目标判定:
/// - 对应的装备栏 → 穿戴 (槽位不符不接收)
/// - 背包面板内 → 放回背包 (从装备栏提起的即为脱下)
/// - 其他 UI 上 → 取消, 物品回原位
/// - UI 之外的地面 → 丢弃
///
/// 右键随时取消。
#[allow(clippy::too_many_arguments)]
pub fn item_drag(
    net: Res<Net>,
    buttons: Res<ButtonInput<MouseButton>>,
    hover: Res<UiHover>,
    mut grab: ResMut<Grab>,
    q_bag: Query<(&Interaction, &EquipItem)>,
    q_slot: Query<(&Interaction, &EquipSlotTarget)>,
    q_panel: Query<(&Panel, &RelativeCursorPosition)>,
    q_shop: Query<&RelativeCursorPosition, With<ShopRoot>>,
) {
    grab.released = false;
    // 右键取消: 提起本就没动服务端状态, 清掉即可
    if buttons.just_pressed(MouseButton::Right) && grab.item.is_some() {
        grab.take();
        return;
    }
    if !buttons.just_pressed(MouseButton::Left) {
        return;
    }
    let hovered = |it: &Interaction| matches!(it, Interaction::Hovered | Interaction::Pressed);

    // ── 手上没东西: 提起 ──
    if grab.item.is_none() {
        if let Some((_, item)) = q_bag.iter().find(|(it, _)| hovered(it)) {
            if let Some(info) = net.inventory.iter().find(|i| i.id == item.0) {
                grab.item = Some(info.clone());
                grab.from = Some(GrabFrom::Bag);
                grab.rev = grab.rev.wrapping_add(1);
            }
        } else if let Some((_, slot)) = q_slot.iter().find(|(it, _)| hovered(it)) {
            if let Some(info) = net.equipment.get(slot.0) {
                grab.item = Some(info.clone());
                grab.from = Some(GrabFrom::Equip(slot.0));
                grab.rev = grab.rev.wrapping_add(1);
            }
        }
        return;
    }

    // ── 手上有东西: 放下 ──
    let item = grab.item.clone().unwrap();
    let from = grab.from.clone().unwrap_or(GrabFrom::Bag);
    // 1) 落在装备栏上 — 槽位要对得上
    if let Some((_, slot)) = q_slot.iter().find(|(it, _)| hovered(it)) {
        if slot.0 == item.slot && from == GrabFrom::Bag {
            net.send(ClientMessage::Equip {
                item_id: item.id.clone(),
                slot: item.slot.clone(),
            });
        }
        grab.take();
        return;
    }
    // 2) 落在商店窗上 — 卖出 (装备栏提起的先脱下再卖)
    if q_shop.iter().any(|r| r.mouse_over()) {
        if let Some(sh) = &net.shop {
            if let GrabFrom::Equip(slot) = from {
                net.send(ClientMessage::Unequip {
                    slot: slot.to_string(),
                });
            }
            net.send(ClientMessage::SellItem {
                npc_id: sh.npc_id.clone(),
                item_id: item.id.clone(),
            });
        }
        grab.take();
        return;
    }
    // 3) 落在背包面板内 — 从装备栏提起的即为脱下, 从背包提起的原样放回
    let in_bag = q_panel
        .iter()
        .any(|(p, r)| p.0 == PanelKind::Bag && r.mouse_over());
    if in_bag {
        if let GrabFrom::Equip(slot) = from {
            net.send(ClientMessage::Unequip {
                slot: slot.to_string(),
            });
        }
        grab.take();
        return;
    }
    // 4) 落在其他 UI 上 — 取消
    if hover.0 {
        grab.take();
        return;
    }
    // 5) 落在地面 — 丢弃 (装备上的先脱下再丢, 消息按序处理)
    if let GrabFrom::Equip(slot) = from {
        net.send(ClientMessage::Unequip {
            slot: slot.to_string(),
        });
    }
    net.send(ClientMessage::DropItem {
        item_id: item.id.clone(),
    });
    grab.take();
}

// ─────────── NPC 对话框 ───────────

/// 对话框根节点 (随对话版本重建)
#[derive(Component)]
pub struct DialogRoot;

/// 对话选项按钮 (回传给服务端的下标)
#[derive(Component)]
pub struct DialogOption(u32);

/// 对话框: 服务端下发一页就重建一次; 关闭由服务端的 NpcDialogEnd 驱动
pub fn dialog(
    mut commands: Commands,
    net: Res<Net>,
    skin: Res<Skin>,
    wood: Res<WoodTex>,
    mut seen_rev: Local<u32>,
    q_old: Query<Entity, With<DialogRoot>>,
) {
    if *seen_rev == net.dialog_rev {
        return;
    }
    *seen_rev = net.dialog_rev;
    for e in &q_old {
        commands.entity(e).despawn_recursive();
    }
    let Some(d) = &net.dialog else {
        return;
    };
    let font = skin.font.clone();
    commands
        .spawn((
            DialogRoot,
            UiBlock,
            RelativeCursorPosition::default(),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                top: Val::Px(180.0),
                margin: UiRect::left(Val::Px(-210.0)), // 宽 420 的一半, 居中
                width: Val::Px(420.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            BorderRadius::all(Val::Px(4.0)),
            GlobalZIndex(20),
        ))
        .with_children(|root| {
            wood_bg(root, &wood);
            metal_frame(root, &wood);
            // 标题栏: NPC 名 + 关闭
            root.spawn((
                Node {
                    height: Val::Px(44.0),
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
                bar.spawn(text(&font, d.name.clone(), 15.0, GOLD_BRIGHT));
                // 关闭 = 一个 idx 越界的选项, 服务端一律回 NpcDialogEnd
                bar.spawn((Button, DialogOption(u32::MAX), Node::default()))
                    .with_children(|x| {
                        x.spawn(text(&font, "×", 18.0, TEXT_DIM));
                    });
            });
            // 正文
            root.spawn(Node {
                padding: UiRect::axes(Val::Px(18.0), Val::Px(16.0)),
                ..default()
            })
            .with_children(|body| {
                body.spawn((
                    text(&font, d.text.clone(), 14.0, TEXT_MAIN),
                    TextLayout::new_with_justify(JustifyText::Left),
                ));
            });
            // 选项
            root.spawn(Node {
                flex_direction: FlexDirection::Column,
                padding: UiRect::new(Val::Px(18.0), Val::Px(18.0), Val::Px(0.0), Val::Px(16.0)),
                row_gap: Val::Px(6.0),
                ..default()
            })
            .with_children(|list| {
                for o in &d.options {
                    list.spawn((
                        Button,
                        DialogOption(o.idx),
                        Node {
                            padding: UiRect::axes(Val::Px(12.0), Val::Px(7.0)),
                            border: UiRect::all(Val::Px(1.0)),
                            ..default()
                        },
                        BackgroundColor(SLOT_BG),
                        BorderColor(EDGE_DARK),
                        BorderRadius::all(Val::Px(3.0)),
                    ))
                    .with_children(|b| {
                        b.spawn(text(&font, format!("· {}", o.label), 13.0, QUALITY_COMMON));
                    });
                }
            });
        });
}

/// 选项点击 → 回传服务端; 悬停变金边
pub fn dialog_clicks(
    net: Res<Net>,
    mut q: Query<(&Interaction, &DialogOption, &mut BorderColor), Changed<Interaction>>,
) {
    for (it, opt, mut border) in q.iter_mut() {
        match it {
            Interaction::Pressed => {
                let Some(d) = &net.dialog else { continue };
                net.send(ClientMessage::NpcOption {
                    npc_id: d.npc_id.clone(),
                    page: d.page,
                    idx: opt.0,
                });
            }
            Interaction::Hovered => border.0 = EDGE_GOLD,
            Interaction::None => border.0 = EDGE_DARK,
        }
    }
}

// ─────────── 大地图 (M) ───────────

/// 大地图窗口四周留给屏幕的余量 (逻辑 px)
const BIGMAP_INSET: f32 = 40.0;
/// 标题栏 + 底栏高度 (逻辑 px), 算可用空间时要扣掉
const BIGMAP_CHROME: f32 = 46.0 + 38.0;
/// 没有小地图帧时的占位尺寸
const BIGMAP_FALLBACK: f32 = 420.0;
/// 图与窗口边框之间的内边距 (逻辑 px)
///
/// 顺带盖掉一个观感问题: mmap 帧右侧常带一两列全透明像素, 图若直接贴边,
/// 那几列会露出面板底色, 看着像右边框变黑了。
const BIGMAP_PAD: f32 = 2.0;
/// 点阵容量: 怪物与 NPC 各自的上限, 超出不画
const BIGMAP_DOTS: usize = 64;

#[derive(Component)]
pub struct BigMapRoot;
#[derive(Component)]
pub struct BigMapImg;
/// 地图显示区 (高度随图的实际比例收紧, 免得留一大片黑边)
#[derive(Component)]
pub struct BigMapArea;
#[derive(Component)]
pub struct BigMapPlayerDot;
/// 寻路终点标记
#[derive(Component)]
pub struct BigMapGoalDot;
/// 大地图上的实体点: kind 0=怪物 1=NPC
#[derive(Component)]
pub struct BigMapDot(u8, usize);
#[derive(Component)]
pub struct BigMapTitle;
#[derive(Component)]
pub struct BigMapFoot;

/// 进图时预建 (默认隐藏); 与面板族同样是常驻节点, 靠 display 开关
pub fn setup_bigmap(mut commands: Commands, skin: Res<Skin>, wood: Res<WoodTex>) {
    let font = skin.font.clone();
    commands
        .spawn((
            BigMapRoot,
            UiBlock,
            RelativeCursorPosition::default(),
            Node {
                display: Display::None,
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                top: Val::Percent(50.0),
                width: Val::Px(BIGMAP_FALLBACK),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            BorderRadius::all(Val::Px(4.0)),
            GlobalZIndex(30),
        ))
        .with_children(|root| {
            wood_bg(root, &wood);
            metal_frame(root, &wood);
            root.spawn((
                Node {
                    height: Val::Px(46.0),
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
                bar.spawn((text(&font, "地 图", 15.0, GOLD_BRIGHT), BigMapTitle));
                bar.spawn(text(&font, "M / Esc 关闭", 11.0, TEXT_DIM));
            });
            // 地图区: 图与所有点都绝对定位在这里
            root.spawn((
                BigMapArea,
                Node {
                    // 尺寸每帧按图算; 不留内外边距, 图直接贴住窗口内沿
                    width: Val::Px(BIGMAP_FALLBACK),
                    height: Val::Px(BIGMAP_FALLBACK),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(SLOT_BG),
            ))
            .with_children(|area| {
                area.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        ..default()
                    },
                    ImageNode::default(),
                    Visibility::Hidden,
                    BigMapImg,
                    // 点图寻路: Button 让它拿得到 Interaction, 相对位置换算成格坐标
                    Button,
                    RelativeCursorPosition::default(),
                ));
                for i in 0..BIGMAP_DOTS {
                    area.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            width: Val::Px(4.0),
                            height: Val::Px(4.0),
                            display: Display::None,
                            ..default()
                        },
                        BackgroundColor(HP_RED),
                        BorderRadius::all(Val::Percent(50.0)),
                        BigMapDot(0, i),
                    ));
                }
                for i in 0..BIGMAP_DOTS {
                    area.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            width: Val::Px(5.0),
                            height: Val::Px(5.0),
                            display: Display::None,
                            ..default()
                        },
                        BackgroundColor(Color::srgb(0.60, 0.49, 0.88)),
                        BorderRadius::all(Val::Percent(50.0)),
                        BigMapDot(1, i),
                    ));
                }
                // 寻路终点标记 (青色, 在玩家点之下)
                area.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        width: Val::Px(7.0),
                        height: Val::Px(7.0),
                        display: Display::None,
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.35, 0.86, 0.86)),
                    BorderRadius::all(Val::Percent(50.0)),
                    BigMapGoalDot,
                ));
                // 玩家点最后建, 盖在其它点之上
                area.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        width: Val::Px(7.0),
                        height: Val::Px(7.0),
                        display: Display::None,
                        ..default()
                    },
                    BackgroundColor(GOLD_BRIGHT),
                    BorderRadius::all(Val::Percent(50.0)),
                    BigMapPlayerDot,
                ));
            });
            root.spawn((
                Node {
                    height: Val::Px(38.0),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    padding: UiRect::horizontal(Val::Px(17.0)),
                    border: UiRect::top(Val::Px(1.0)),
                    ..default()
                },
                BorderColor(EDGE_DARK),
            ))
            .with_children(|bar| {
                bar.spawn((text(&font, "", 12.0, TEXT_DIM), BigMapFoot));
                bar.spawn(text(&font, "金=自己 · 红=怪物 · 紫=NPC", 11.0, TEXT_DIM));
            });
        });
}

/// M 开关大地图; Esc 关闭
pub fn toggle_bigmap(
    keys: Res<ButtonInput<KeyCode>>,
    chat: Res<crate::hud::ChatState>,
    mut q: Query<&mut Node, With<BigMapRoot>>,
) {
    if chat.active {
        return;
    }
    let open = keys.just_pressed(KeyCode::KeyM);
    let close = keys.just_pressed(KeyCode::Escape);
    if !open && !close {
        return;
    }
    for mut node in q.iter_mut() {
        node.display = if close || node.display != Display::None {
            Display::None
        } else {
            Display::Flex
        };
    }
}

/// 大地图内容: 整图等比塞进方框, 点按「像素位 = 格坐标 × 图尺寸/地图格数」换算
///
/// 几个 Query 都要写 &mut Node, 靠成串的 Without 向 Bevy 证明互不相交,
/// 类型因此偏长 —— 与本文件其它系统同样用 allow 压掉。
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub fn bigmap(
    net: Res<Net>,
    world: Res<crate::World>,
    minimap: Res<crate::MiniMap>,
    remotes: Res<crate::Remotes>,
    auto: Res<crate::AutoPath>,
    windows: Query<&Window>,
    ui_scale: Res<UiScale>,
    mut q_root: Query<&mut Node, With<BigMapRoot>>,
    q_player: Query<&crate::Player>,
    mut q_img: Query<
        (&mut ImageNode, &mut Node, &mut Visibility),
        (With<BigMapImg>, Without<BigMapRoot>, Without<BigMapArea>),
    >,
    mut q_area: Query<&mut Node, (With<BigMapArea>, Without<BigMapRoot>)>,
    mut q_dot: Query<
        (&BigMapDot, &mut Node),
        (
            Without<BigMapImg>,
            Without<BigMapRoot>,
            Without<BigMapArea>,
            Without<BigMapPlayerDot>,
            Without<BigMapGoalDot>,
        ),
    >,
    mut q_pdot: Query<
        &mut Node,
        (
            With<BigMapPlayerDot>,
            Without<BigMapImg>,
            Without<BigMapRoot>,
            Without<BigMapArea>,
            Without<BigMapGoalDot>,
        ),
    >,
    mut q_gdot: Query<
        &mut Node,
        (
            With<BigMapGoalDot>,
            Without<BigMapImg>,
            Without<BigMapRoot>,
            Without<BigMapArea>,
            Without<BigMapPlayerDot>,
        ),
    >,
    mut q_title: Query<&mut Text, (With<BigMapTitle>, Without<BigMapFoot>)>,
    mut q_foot: Query<&mut Text, With<BigMapFoot>>,
) {
    // 关着就不算 —— 每帧重排点位不便宜
    if q_root.iter().all(|n| n.display == Display::None) {
        return;
    }
    // 可用空间 = 窗口逻辑尺寸 减去四周余量与上下栏
    let s = ui_scale.0.max(0.01);
    let (max_w, max_h) = match windows.get_single() {
        Ok(w) => (
            (w.width() / s - BIGMAP_INSET * 2.0 - BIGMAP_PAD * 2.0 - 2.0).max(200.0),
            (w.height() / s - BIGMAP_INSET * 2.0 - BIGMAP_CHROME - BIGMAP_PAD * 2.0 - 2.0)
                .max(160.0),
        ),
        Err(_) => (BIGMAP_FALLBACK, BIGMAP_FALLBACK),
    };
    let player = q_player.get_single().ok();
    for mut t in q_title.iter_mut() {
        t.0 = if net.zone_name.is_empty() {
            "地 图".into()
        } else {
            net.zone_name.clone()
        };
    }
    for mut t in q_foot.iter_mut() {
        t.0 = match player {
            Some(p) => format!("坐标 {:.0}, {:.0}", p.pos.x, p.pos.y),
            None => String::new(),
        };
    }
    // 图: 等比缩放塞进方框; 没有小地图帧时整个区域留空
    let fit = match &minimap.image {
        Some((handle, _)) => {
            // 只取非透明区域: 边缘空像素会露底色, 显得右/下边框发黑
            let trim = minimap.trim;
            let size = Vec2::new(trim.width().max(1.0), trim.height().max(1.0));
            // 能放多大放多大, 但放大只取整数倍 —— 小地图是像素图, 非整数
            // 倍率放大会发糊 (与背包图标同一条规矩)。缩小则只能按需取小数。
            let raw = (max_w / size.x.max(1.0)).min(max_h / size.y.max(1.0));
            let k = if raw >= 1.0 { raw.floor() } else { raw };
            let (dw, dh) = (size.x * k, size.y * k);
            // 显示区 = 图 + 四周内边距
            let (aw, ah) = (dw + BIGMAP_PAD * 2.0, dh + BIGMAP_PAD * 2.0);
            for mut node in q_area.iter_mut() {
                node.width = Val::Px(aw);
                node.height = Val::Px(ah);
            }
            // 窗口宽度随图走 (+2 是左右金线), 并重算居中偏移
            let root_w = aw + 2.0;
            for mut node in q_root.iter_mut() {
                node.width = Val::Px(root_w);
                node.margin = UiRect::new(
                    Val::Px(-root_w / 2.0),
                    Val::Px(0.0),
                    Val::Px(-(ah + BIGMAP_CHROME + 2.0) / 2.0),
                    Val::Px(0.0),
                );
            }
            for (mut img, mut node, mut vis) in q_img.iter_mut() {
                img.image = handle.clone();
                img.rect = Some(trim);
                node.width = Val::Px(dw);
                node.height = Val::Px(dh);
                node.left = Val::Px(BIGMAP_PAD);
                node.top = Val::Px(BIGMAP_PAD);
                *vis = Visibility::Inherited;
            }
            // 格坐标 → 区域内像素 (点位同样要带上内边距)
            let sx = size.x / world.map.width.max(1) as f32 * k;
            let sy = size.y / world.map.height.max(1) as f32 * k;
            Some((sx, sy, BIGMAP_PAD, BIGMAP_PAD))
        }
        None => {
            // 该区没配小地图帧: 收回占位尺寸, 免得沿用上一张图的宽度
            for (_, _, mut vis) in q_img.iter_mut() {
                *vis = Visibility::Hidden;
            }
            for mut node in q_area.iter_mut() {
                node.width = Val::Px(BIGMAP_FALLBACK);
                node.height = Val::Px(BIGMAP_FALLBACK / 2.0);
            }
            for mut node in q_root.iter_mut() {
                node.width = Val::Px(BIGMAP_FALLBACK);
                node.margin = UiRect::new(
                    Val::Px(-BIGMAP_FALLBACK / 2.0),
                    Val::Px(0.0),
                    Val::Px(-(BIGMAP_FALLBACK / 2.0 + BIGMAP_CHROME) / 2.0),
                    Val::Px(0.0),
                );
            }
            None
        }
    };
    let Some((sx, sy, ox, oy)) = fit else {
        for (_, mut node) in q_dot.iter_mut() {
            node.display = Display::None;
        }
        for mut node in q_pdot.iter_mut() {
            node.display = Display::None;
        }
        return;
    };
    let place = |node: &mut Node, x: f64, y: f64, r: f32| {
        node.left = Val::Px(ox + x as f32 * sx - r);
        node.top = Val::Px(oy + y as f32 * sy - r);
        node.display = Display::Flex;
    };
    // 怪物 (活着的) 与 NPC 各自成组
    let mobs: Vec<DVec2> = remotes
        .0
        .values()
        .filter(|r| r.image.is_some() && r.anim != 4)
        .map(|r| r.pos)
        .collect();
    for (dot, mut node) in q_dot.iter_mut() {
        let pos = match dot.0 {
            0 => mobs.get(dot.1).copied(),
            _ => net.npcs.get(dot.1).map(|n| DVec2::new(n.x, n.y)),
        };
        match pos {
            Some(p) => place(&mut node, p.x, p.y, 2.0),
            None => node.display = Display::None,
        }
    }
    for mut node in q_pdot.iter_mut() {
        match player {
            Some(p) => place(&mut node, p.pos.x, p.pos.y, 3.5),
            None => node.display = Display::None,
        }
    }
    for mut node in q_gdot.iter_mut() {
        match auto.goal {
            Some(g) => place(&mut node, g.x, g.y, 3.5),
            None => node.display = Display::None,
        }
    }
}

// ─────────── NPC 商店 ───────────

/// 商店窗根节点 (随商店版本重建)
#[derive(Component)]
pub struct ShopRoot;

/// 货架上的一行 (点击买入)
#[derive(Component)]
pub struct ShopBuy(String);

/// 关闭商店
#[derive(Component)]
pub struct ShopClose;

/// 商店窗: 货架逐行 图标/名称/属性/价格, 点行买入; 背包物品提到窗上松手即卖出
#[allow(clippy::too_many_arguments)]
pub fn shop(
    mut commands: Commands,
    net: Res<Net>,
    skin: Res<Skin>,
    wood: Res<WoodTex>,
    world: Res<crate::World>,
    mut icons: ResMut<crate::ItemIcons>,
    mut images: ResMut<Assets<Image>>,
    ui_scale: Res<UiScale>,
    mut seen_rev: Local<u32>,
    q_old: Query<Entity, With<ShopRoot>>,
) {
    if *seen_rev == net.shop_rev {
        return;
    }
    *seen_rev = net.shop_rev;
    for e in &q_old {
        commands.entity(e).despawn_recursive();
    }
    let Some(sh) = &net.shop else {
        return;
    };
    let font = skin.font.clone();
    let rows: Vec<(protocol::ShopItemInfo, Icon)> = sh
        .items
        .iter()
        .map(|it| {
            (
                it.clone(),
                icons.get(it.image, &world.data_root, &mut images),
            )
        })
        .collect();
    let gold = net.gold;
    let title = sh.name.clone();
    commands
        .spawn((
            ShopRoot,
            UiBlock,
            RelativeCursorPosition::default(),
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                top: Val::Px(150.0),
                margin: UiRect::left(Val::Px(-235.0)),
                width: Val::Px(470.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            BorderRadius::all(Val::Px(4.0)),
            GlobalZIndex(20),
        ))
        .with_children(|root| {
            wood_bg(root, &wood);
            metal_frame(root, &wood);
            root.spawn((
                Node {
                    height: Val::Px(44.0),
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
                bar.spawn(text(&font, title, 15.0, GOLD_BRIGHT));
                bar.spawn((Button, ShopClose, Node::default()))
                    .with_children(|x| {
                        x.spawn(text(&font, "×", 18.0, TEXT_DIM));
                    });
            });
            root.spawn(Node {
                flex_direction: FlexDirection::Column,
                padding: UiRect::axes(Val::Px(14.0), Val::Px(12.0)),
                row_gap: Val::Px(5.0),
                ..default()
            })
            .with_children(|list| {
                for (it, icon) in rows {
                    let mut st = Vec::new();
                    if it.attack > 0 {
                        st.push(format!("攻+{}", it.attack));
                    }
                    if it.magic > 0 {
                        st.push(format!("魔+{}", it.magic));
                    }
                    if it.spirit > 0 {
                        st.push(format!("道+{}", it.spirit));
                    }
                    if it.defense > 0 {
                        st.push(format!("防+{}", it.defense));
                    }
                    if it.hp > 0 {
                        st.push(format!("血+{}", it.hp));
                    }
                    let stock = if it.stock < 0 {
                        String::new()
                    } else {
                        format!("  余{}", it.stock)
                    };
                    list.spawn((
                        Button,
                        ShopBuy(it.template.clone()),
                        Node {
                            height: Val::Px(BAG_CELL + 6.0),
                            align_items: AlignItems::Center,
                            column_gap: Val::Px(10.0),
                            padding: UiRect::horizontal(Val::Px(8.0)),
                            border: UiRect::all(Val::Px(1.0)),
                            ..default()
                        },
                        BackgroundColor(SLOT_BG),
                        BorderColor(EDGE_DARK),
                        BorderRadius::all(Val::Px(3.0)),
                    ))
                    .with_children(|row| {
                        row.spawn((
                            Node {
                                width: Val::Px(BAG_CELL),
                                height: Val::Px(BAG_CELL),
                                justify_content: JustifyContent::Center,
                                align_items: AlignItems::Center,
                                overflow: Overflow::clip(),
                                ..default()
                            },
                            BackgroundColor(SLOT_BG),
                        ))
                        .with_children(|s| {
                            if let Some((h, natural)) = icon {
                                s.spawn(crisp_icon(h, natural, BAG_CELL, ui_scale.0));
                            }
                        });
                        row.spawn((
                            Node {
                                width: Val::Px(120.0),
                                ..default()
                            },
                            Text::new(it.name.clone()),
                            TextFont {
                                font: font.clone(),
                                font_size: 13.0,
                                ..default()
                            },
                            TextColor(QUALITY_COMMON),
                        ));
                        row.spawn((
                            Node {
                                width: Val::Px(130.0),
                                ..default()
                            },
                            Text::new(st.join(" ")),
                            TextFont {
                                font: font.clone(),
                                font_size: 12.0,
                                ..default()
                            },
                            TextColor(TEXT_DIM),
                        ));
                        row.spawn(text(
                            &font,
                            format!("{} 金{stock}", it.price),
                            13.0,
                            EXP_GOLD,
                        ));
                    });
                }
            });
            root.spawn((
                Node {
                    height: Val::Px(40.0),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    padding: UiRect::horizontal(Val::Px(16.0)),
                    border: UiRect::top(Val::Px(1.0)),
                    ..default()
                },
                BorderColor(EDGE_DARK),
            ))
            .with_children(|bar| {
                bar.spawn(text(
                    &font,
                    "点一行买入 · 从背包提起物品放到本窗即卖出",
                    12.0,
                    TEXT_DIM,
                ));
                bar.spawn(text(&font, format!("金币 {gold}"), 12.0, EXP_GOLD));
            });
        });
}

/// 货架点击买入 / 关闭 (关闭只是本地收起, 服务端不存会话)
pub fn shop_clicks(
    mut commands: Commands,
    net: Res<Net>,
    mut q_buy: Query<(&Interaction, &ShopBuy, &mut BorderColor), Changed<Interaction>>,
    q_close: Query<&Interaction, (Changed<Interaction>, With<ShopClose>)>,
    q_root: Query<Entity, With<ShopRoot>>,
) {
    for (it, buy, mut border) in q_buy.iter_mut() {
        match it {
            Interaction::Pressed => {
                let Some(sh) = &net.shop else { continue };
                net.send(ClientMessage::BuyItem {
                    npc_id: sh.npc_id.clone(),
                    template: buy.0.clone(),
                });
            }
            Interaction::Hovered => border.0 = EDGE_GOLD,
            Interaction::None => border.0 = EDGE_DARK,
        }
    }
    if q_close.iter().any(|i| *i == Interaction::Pressed) {
        for e in &q_root {
            commands.entity(e).despawn_recursive();
        }
    }
}

/// 悬停物品 Tips (原版传奇: 属性不常驻面板, 鼠标停到物品上才浮出)
#[derive(Component)]
pub struct Tooltip;

/// 槽位键 → 中文名
fn slot_label(slot: &str) -> &'static str {
    match slot {
        "weapon" => "武器",
        "armor" => "衣服",
        "helmet" => "头盔",
        "necklace" => "项链",
        "ring" => "戒指",
        "bracelet" => "护腕",
        _ => "物品",
    }
}

/// Tips 内容行: 名称 + 部位 + 逐条属性
fn tooltip_lines(i: &protocol::ItemInfo) -> Vec<(String, Color)> {
    let mut v = vec![
        (i.name.clone(), GOLD_BRIGHT),
        (slot_label(&i.slot).to_string(), TEXT_DIM),
    ];
    if i.attack > 0 {
        v.push((format!("攻击 +{}", i.attack), TEXT_MAIN));
    }
    if i.magic > 0 {
        v.push((format!("魔法 +{}", i.magic), TEXT_MAIN));
    }
    if i.spirit > 0 {
        v.push((format!("道术 +{}", i.spirit), TEXT_MAIN));
    }
    if i.defense > 0 {
        v.push((format!("防御 +{}", i.defense), TEXT_MAIN));
    }
    if i.hp > 0 {
        v.push((format!("生命 +{}", i.hp), TEXT_MAIN));
    }
    if v.len() == 2 {
        v.push(("无附加属性".into(), DISABLED));
    }
    v
}

/// Tips 最大宽度 (逻辑 px) — 实宽由内容决定, 这里只作贴边翻转的保守估计
const TIP_W: f32 = 150.0;
/// Tips 估高 (名称 + 部位 + 最多三条属性), 同样只用于贴边翻转
const TIP_H: f32 = 78.0;

/// 光标 → Tips 左上角 (逻辑 px): 默认右下, 贴到窗口右/下边就翻到另一侧
fn tip_pos(cx: f32, cy: f32, ww: f32, wh: f32) -> (f32, f32) {
    let left = if cx + 18.0 + TIP_W > ww {
        cx - 12.0 - TIP_W
    } else {
        cx + 18.0
    };
    let top = if cy + 18.0 + TIP_H > wh {
        cy - 12.0 - TIP_H
    } else {
        cy + 18.0
    };
    (left.max(0.0), top.max(0.0))
}

/// 光标逻辑坐标 + 窗口逻辑尺寸 (UI 布局用逻辑像素, cursor_position 给的是物理像素)
fn cursor_logical(windows: &Query<&Window>, ui: f32) -> Option<(f32, f32, f32, f32)> {
    let win = windows.get_single().ok()?;
    let c = win.cursor_position()?;
    let s = ui.max(0.01);
    Some((c.x / s, c.y / s, win.width() / s, win.height() / s))
}

/// 悬停背包格/装备栏时浮出 Tips, 跟随光标; 手上提着物品时不显示
#[allow(clippy::too_many_arguments)]
pub fn tooltip(
    mut commands: Commands,
    windows: Query<&Window>,
    ui_scale: Res<UiScale>,
    skin: Res<Skin>,
    net: Res<Net>,
    grab: Res<Grab>,
    q_bag: Query<(&Interaction, &EquipItem)>,
    q_slot: Query<(&Interaction, &EquipSlotTarget)>,
    q_tip: Query<Entity, With<Tooltip>>,
    mut q_node: Query<&mut Node, With<Tooltip>>,
    mut shown: Local<Option<String>>,
) {
    let hovered = |it: &Interaction| matches!(it, Interaction::Hovered | Interaction::Pressed);
    // 提着东西时不弹 Tips (挡住落点判断)
    let item: Option<protocol::ItemInfo> = if grab.item.is_some() {
        None
    } else if let Some((_, e)) = q_bag.iter().find(|(it, _)| hovered(it)) {
        net.inventory.iter().find(|i| i.id == e.0).cloned()
    } else if let Some((_, s)) = q_slot.iter().find(|(it, _)| hovered(it)) {
        net.equipment.get(s.0).cloned()
    } else {
        None
    };

    // 光标位置要在生成前拿到 — 否则新节点这一帧会先画在窗口左上角闪一下
    let Some((cx, cy, ww, wh)) = cursor_logical(&windows, ui_scale.0) else {
        return;
    };
    let (left, top) = tip_pos(cx, cy, ww, wh);

    // 目标变了才重建
    if shown.as_deref() != item.as_ref().map(|i| i.id.as_str()) {
        *shown = item.as_ref().map(|i| i.id.clone());
        for e in &q_tip {
            commands.entity(e).despawn_recursive();
        }
        if let Some(i) = &item {
            let lines = tooltip_lines(i);
            commands
                .spawn((
                    Tooltip,
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(left),
                        top: Val::Px(top),
                        min_width: Val::Px(88.0),
                        max_width: Val::Px(TIP_W),
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(3.0),
                        padding: UiRect::axes(Val::Px(10.0), Val::Px(8.0)),
                        border: UiRect::all(Val::Px(1.0)),
                        ..default()
                    },
                    BackgroundColor(PANEL_BG),
                    BorderColor(GOLD),
                    BorderRadius::all(Val::Px(3.0)),
                    GlobalZIndex(90),
                ))
                .with_children(|t| {
                    for (n, (line, color)) in lines.into_iter().enumerate() {
                        t.spawn(text(
                            &skin.font,
                            line,
                            if n == 0 { 13.0 } else { 12.0 },
                            color,
                        ));
                    }
                });
        }
    }

    // 后续帧跟随光标
    for mut node in q_node.iter_mut() {
        node.left = Val::Px(left);
        node.top = Val::Px(top);
    }
}

/// 提在光标上的物品图标 (跟随光标, 压在所有 UI 之上)
#[derive(Component)]
pub struct GrabIcon;

#[allow(clippy::too_many_arguments)]
pub fn grab_icon(
    mut commands: Commands,
    windows: Query<&Window>,
    grab: Res<Grab>,
    world: Res<crate::World>,
    mut icons: ResMut<crate::ItemIcons>,
    mut images: ResMut<Assets<Image>>,
    ui_scale: Res<UiScale>,
    mut seen_rev: Local<u32>,
    mut q: Query<&mut Node, With<GrabIcon>>,
    q_ent: Query<Entity, With<GrabIcon>>,
) {
    // 光标位置要在生成前拿到 — 否则新节点这一帧会先画在窗口左上角闪一下
    let Some((cx, cy, _, _)) = cursor_logical(&windows, ui_scale.0) else {
        return;
    };
    let (left, top) = (cx - BAG_CELL / 2.0, cy - BAG_CELL / 2.0);
    if *seen_rev != grab.rev {
        *seen_rev = grab.rev;
        for e in &q_ent {
            commands.entity(e).despawn_recursive();
        }
        if let Some(item) = &grab.item {
            if let Some((h, natural)) = icons.get(item.image, &world.data_root, &mut images) {
                commands
                    .spawn((
                        GrabIcon,
                        Node {
                            position_type: PositionType::Absolute,
                            left: Val::Px(left),
                            top: Val::Px(top),
                            ..default()
                        },
                        GlobalZIndex(100),
                    ))
                    .with_children(|p| {
                        p.spawn(crisp_icon(h, natural, BAG_CELL, ui_scale.0));
                    });
            }
        }
    }
    for mut node in q.iter_mut() {
        node.left = Val::Px(left);
        node.top = Val::Px(top);
    }
}

/// 点击交互: 装备/卸下/任务操作
#[allow(clippy::type_complexity)]
pub fn clicks(net: Res<Net>, mut q: Query<(&Interaction, &QuestAction), Changed<Interaction>>) {
    for (it, qa) in q.iter_mut() {
        if *it != Interaction::Pressed {
            continue;
        }
        {
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
    ui_scale: Res<UiScale>,
    grab: Res<Grab>,
    mut last_rev: Local<(u32, u32, u32, u32)>,
    q_body: Query<(Entity, &PanelBody)>,
) {
    // 提起/放下也要重建 — 源格子在物品提在手上时应显示为空
    let rev = (net.inv_rev, net.quest_rev, net.stat_rev, grab.rev);
    if *last_rev == rev {
        return;
    }
    // 面板在进图时才建 — 若数据先到, 不能吞掉这次版本号, 否则面板永远是空的
    if q_body.is_empty() {
        return;
    }
    *last_rev = rev;
    let portrait = portrait.as_ref().and_then(|p| p.0.clone());
    // 本次重建涉及的物品图标预取
    let mut icon_of = |img: u16| icons.get(img, &world.data_root, &mut images);
    let inv_icons: Vec<Icon> = net.inventory.iter().map(|i| icon_of(i.image)).collect();
    let equip_icons: std::collections::HashMap<String, Icon> = net
        .equipment
        .iter()
        .map(|(k, i)| (k.clone(), icon_of(i.image)))
        .collect();
    for (entity, body) in &q_body {
        let mut e = commands.entity(entity);
        e.despawn_descendants();
        match body.0 {
            PanelKind::Bag => build_bag(&mut e, &net, &skin, &inv_icons, ui_scale.0, &grab),
            PanelKind::Character => build_character(
                &mut e,
                &net,
                &skin,
                portrait.clone(),
                &equip_icons,
                ui_scale.0,
                &grab,
            ),
            PanelKind::Quest => build_quest(&mut e, &net, &skin),
        }
    }
}

/// 背包格数 = 列 × 行 (与服务端 MAX_INVENTORY 一致)
const BAG_SLOTS: usize = BAG_COLS * 5;
/// 每行格数 — 430 面板宽下 10×34 + 9×5 + 左右 22.5 内边距刚好排满
const BAG_COLS: usize = 10;
/// 背包格边长 (逻辑 px)
const BAG_CELL: f32 = 34.0;
/// 格间距
const BAG_GAP: f32 = 5.0;
/// 格区四周内边距
const BAG_PAD: f32 = 8.0;
/// 面板宽度 = 格区 + 内边距 + 1px 边框×2 (Bevy UI 按 border-box 量)
/// —— 由格数算出, 改列数不用再手调宽度, 也不会留下一圈空白
/// +2 是金线 (1px×2, 见 metal_frame), 末尾 +2 是抗舍入余量: 缩放后各段取整
/// 可能多出一两像素, 留一点免得最后一列的边框被裁掉 (列数由 grid 钉死)
const BAG_PANEL_W: f32 =
    BAG_COLS as f32 * BAG_CELL + (BAG_COLS as f32 - 1.0) * BAG_GAP + BAG_PAD * 2.0 + 2.0 + 2.0;

/// 图标句柄 + 原始像素尺寸
type Icon = Option<(Handle<Image>, Vec2)>;

/// 图标按整数倍率呈现, 保证纹素与屏幕像素对齐 (像素画放大/缩小非整数倍会发糊)
///
/// `cell` 为格子边长 (逻辑 px), `ui` 为 UiScale — 逻辑尺寸 × UiScale 才是实际
/// 渲染像素, 所以倍率是按渲染像素算的, 再除回 UiScale 写进 Node。
/// 候选倍率 1×/2×/3×… 与 1/2×/1/3×…; 允许 18% 溢出 (格子 overflow:clip 裁掉),
/// 避免因差几个像素就被迫砍半。
/// 允许超出格子的比例 — 差几个像素就砍半不划算, 溢出部分由格子 overflow:clip 裁掉
const ICON_BLEED: f32 = 1.2;

fn icon_scale(long: f32, cell: f32, ui: f32) -> f32 {
    let budget = cell * ui.max(0.01) * ICON_BLEED; // 可占用的渲染像素
    let long = long.max(1.0);
    if long <= budget {
        (budget / long).floor().max(1.0) // 放大: 取整数倍
    } else {
        1.0 / (long / budget).ceil() // 缩小: 取 1/整数
    }
}

fn crisp_icon(h: Handle<Image>, natural: Vec2, avail: f32, ui: f32) -> impl Bundle {
    let ui = ui.max(0.01);
    let k = icon_scale(natural.max_element(), avail, ui);
    (
        Node {
            width: Val::Px(natural.x * k / ui),
            height: Val::Px(natural.y * k / ui),
            ..default()
        },
        ImageNode::new(h),
    )
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

/// 背包: 10×5 网格 34px 格, 物品格白边, 左键穿戴 / 右键丢地上; 底栏统计
fn build_bag(
    e: &mut bevy::ecs::system::EntityCommands,
    net: &Net,
    skin: &Skin,
    icons: &[Icon],
    ui: f32,
    grab: &Grab,
) {
    let items = net.inventory.clone();
    let font = skin.font.clone();
    let used = items.len();
    // 提在光标上的那件, 原格子留空
    let grabbed = match (&grab.item, &grab.from) {
        (Some(it), Some(GrabFrom::Bag)) => Some(it.id.clone()),
        _ => None,
    };
    e.with_children(|body| {
        // 格区用真正的网格而非 flex_wrap: 列数必须钉死。
        // Bevy 会把每个节点按物理像素取整, UiScale ≠ 1 时子项之和可能比容器
        // 多出一两像素, flex_wrap 一旦没有余量就会少排一列, 整个背包错位。
        body.spawn(Node {
            display: Display::Grid,
            grid_template_columns: RepeatedGridTrack::px(BAG_COLS as u16, BAG_CELL),
            padding: UiRect::all(Val::Px(BAG_PAD)),
            justify_content: JustifyContent::Center,
            column_gap: Val::Px(BAG_GAP),
            row_gap: Val::Px(BAG_GAP),
            ..default()
        })
        .with_children(|grid| {
            for i in 0..BAG_SLOTS {
                let item = items
                    .get(i)
                    .filter(|it| grabbed.as_deref() != Some(it.id.as_str()));
                let mut slot = grid.spawn((
                    Node {
                        width: Val::Px(BAG_CELL),
                        height: Val::Px(BAG_CELL),
                        border: UiRect::all(Val::Px(1.0)),
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        overflow: Overflow::clip(),
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
                        // 原分辨率呈现: 不拉伸到格子大小, 只按整数倍率对齐
                        Some((h, natural)) => {
                            s.spawn(crisp_icon(h, natural, BAG_CELL, ui));
                        }
                        None => {
                            s.spawn(text(&font, name, 13.0, QUALITY_COMMON));
                        }
                    });
                }
            }
        });
        // 底栏 42px: 顶分隔线 + 统计
        body.spawn((
            Node {
                height: Val::Px(42.0),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::SpaceBetween,
                padding: UiRect::horizontal(Val::Px(16.0)),
                border: UiRect::top(Val::Px(1.0)),
                ..default()
            },
            BorderColor(EDGE_DARK),
        ))
        .with_children(|bar| {
            bar.spawn(text(
                &font,
                format!("{BAG_SLOTS} 格 · 已用 {used}"),
                12.0,
                if used >= BAG_SLOTS {
                    QUALITY_COMMON
                } else {
                    TEXT_DIM
                },
            ));
            bar.spawn(text(&font, format!("金币 {}", net.gold), 12.0, EXP_GOLD));
        });
    });
}

/// 装备槽 56px (底排 52px): 有装备→金边+名字缩写(点击卸下), 空→暗边+占位名
#[allow(clippy::too_many_arguments)]
fn equip_slot(
    parent: &mut ChildBuilder,
    font: &Handle<Font>,
    equipment: &std::collections::HashMap<String, protocol::ItemInfo>,
    icons: &std::collections::HashMap<String, Icon>,
    slot_key: Option<&'static str>,
    label: &str,
    size: f32,
    ui: f32,
    grab: &Grab,
) {
    // 从这一栏提起的物品已在光标上, 栏内显示为空
    let grabbed_here = matches!(&grab.from, Some(GrabFrom::Equip(s)) if Some(*s) == slot_key);
    let item = slot_key
        .filter(|_| !grabbed_here)
        .and_then(|k| equipment.get(k));
    let icon = slot_key.and_then(|k| icons.get(k)).cloned().flatten();
    let mut n = parent.spawn((
        Node {
            width: Val::Px(size),
            height: Val::Px(size),
            border: UiRect::all(Val::Px(1.0)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            overflow: Overflow::clip(),
            ..default()
        },
        BackgroundColor(SLOT_BG),
        BorderColor(if item.is_some() { EDGE_GOLD } else { EDGE_DARK }),
        BorderRadius::all(Val::Px(3.0)),
    ));
    // 空栏也要能接收放下的物品, 所以只要有 slot 键就挂上目标组件
    if let Some(key) = slot_key {
        n.insert((Button, EquipSlotTarget(key)));
    }
    match (item, icon) {
        (Some(_), Some((h, natural))) => {
            n.with_children(|s| {
                s.spawn(crisp_icon(h, natural, size, ui));
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
    icons: &std::collections::HashMap<String, Icon>,
    ui: f32,
    grab: &Grab,
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
    let (atk, mag, spi, def): (i32, i32, i32, i32) = (
        equipment.values().map(|i| i.attack).sum(),
        equipment.values().map(|i| i.magic).sum(),
        equipment.values().map(|i| i.spirit).sum(),
        equipment.values().map(|i| i.defense).sum(),
    );
    e.with_children(|body| {
        // 主区: 左列 4 槽 | 中央立绘+名字 (flex_grow 撑满) | 右列 4 槽
        // 横向 padding 与底排/属性区一致, 三段的左右边界天然对齐
        body.spawn(Node {
            padding: UiRect::new(
                Val::Px(CHAR_PAD),
                Val::Px(CHAR_PAD),
                Val::Px(16.0),
                Val::Px(0.0),
            ),
            column_gap: Val::Px(CHAR_GAP),
            ..default()
        })
        .with_children(|row| {
            row.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(10.0),
                ..default()
            })
            .with_children(|col| {
                equip_slot(
                    col,
                    &font,
                    &equipment,
                    icons,
                    Some("weapon"),
                    "武器",
                    CHAR_SLOT,
                    ui,
                    grab,
                );
                equip_slot(
                    col,
                    &font,
                    &equipment,
                    icons,
                    Some("armor"),
                    "衣服",
                    CHAR_SLOT,
                    ui,
                    grab,
                );
                equip_slot(
                    col, &font, &equipment, icons, None, "护腕", CHAR_SLOT, ui, grab,
                );
                equip_slot(
                    col,
                    &font,
                    &equipment,
                    icons,
                    Some("ring"),
                    "戒指",
                    CHAR_SLOT,
                    ui,
                    grab,
                );
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
                        width: Val::Px(CHAR_INNER - CHAR_SLOT * 2.0 - CHAR_GAP * 2.0 - 24.0),
                        height: Val::Px(230.0),
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
                equip_slot(
                    col,
                    &font,
                    &equipment,
                    icons,
                    Some("helmet"),
                    "头盔",
                    CHAR_SLOT,
                    ui,
                    grab,
                );
                equip_slot(
                    col,
                    &font,
                    &equipment,
                    icons,
                    Some("necklace"),
                    "项链",
                    CHAR_SLOT,
                    ui,
                    grab,
                );
                equip_slot(
                    col, &font, &equipment, icons, None, "护腕", CHAR_SLOT, ui, grab,
                );
                equip_slot(
                    col, &font, &equipment, icons, None, "戒指", CHAR_SLOT, ui, grab,
                );
            });
        });
        // 底排 5 槽: 与侧列同 padding, SpaceBetween 让首末槽的外缘
        // 与左右两列的外边界精确对齐 (内容区宽度就是按 5 槽推导的)
        body.spawn(Node {
            padding: UiRect::new(
                Val::Px(CHAR_PAD),
                Val::Px(CHAR_PAD),
                Val::Px(CHAR_GAP),
                Val::Px(14.0),
            ),
            justify_content: JustifyContent::SpaceBetween,
            ..default()
        })
        .with_children(|row| {
            for label in ["腰带", "鞋子", "宝石", "生肖", "星座"] {
                equip_slot(
                    row, &font, &equipment, icons, None, label, CHAR_SLOT, ui, grab,
                );
            }
        });
        // 属性: 两列 grid, 顶分隔线
        let pairs: Vec<(String, String)> = vec![
            ("攻击".into(), format!("+{atk}")),
            ("防御".into(), format!("+{def}")),
            ("魔法".into(), format!("+{mag}")),
            ("道术".into(), format!("+{spi}")),
            (
                "生命".into(),
                stat.map(|s| format!("{}/{}", s.hp, s.max_hp))
                    .unwrap_or_default(),
            ),
            (
                "魔力".into(),
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
                padding: UiRect::new(
                    Val::Px(CHAR_PAD),
                    Val::Px(CHAR_PAD),
                    Val::Px(14.0),
                    Val::Px(18.0),
                ),
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                justify_content: JustifyContent::SpaceBetween,
                row_gap: Val::Px(8.0),
                ..default()
            },
            BorderColor(EDGE_DARK),
        ))
        .with_children(|grid| {
            for (label, value) in pairs {
                grid.spawn(Node {
                    // 两列: 各占内容区一半减半个列距
                    width: Val::Px((CHAR_INNER - CHAR_GAP) / 2.0),
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
                        // 奖励摘要: 经验 · 金币 · 物品
                        let mut rw = vec![format!("经验 {}", q.exp_reward)];
                        if q.gold_reward > 0 {
                            rw.push(format!("金币 {}", q.gold_reward));
                        }
                        for r in &q.item_rewards {
                            rw.push(if r.count > 1 {
                                format!("{}×{}", r.name, r.count)
                            } else {
                                r.name.clone()
                            });
                        }
                        row.spawn(text(&font, rw.join(" · "), 11.0, EXP_GOLD));
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

/// 大地图上点一下 → A* 算路 → 交给 AutoPath 跑过去
#[allow(clippy::type_complexity)]
pub fn bigmap_click(
    world: Res<crate::World>,
    remotes: Res<crate::Remotes>,
    net: Res<Net>,
    mut auto: ResMut<crate::AutoPath>,
    q_player: Query<&crate::Player>,
    q_img: Query<(&Interaction, &RelativeCursorPosition), (With<BigMapImg>, Changed<Interaction>)>,
) {
    for (it, rel) in &q_img {
        if *it != Interaction::Pressed {
            continue;
        }
        let Some(n) = rel.normalized else { continue };
        let Ok(p) = q_player.get_single() else {
            continue;
        };
        // 图铺满整张地图, 归一化位置直接换算成格坐标
        let target = DVec2::new(
            (n.x as f64 * world.map.width as f64).clamp(0.0, world.map.width as f64 - 1.0),
            (n.y as f64 * world.map.height as f64).clamp(0.0, world.map.height as f64 - 1.0),
        );
        let blockers = crate::avoid_points(&remotes, &net, p.pos);
        if std::env::var("MIRFORGE_PATHDBG").is_ok() {
            let plain = sim::find_path(
                &world.walk,
                (p.pos.x, p.pos.y),
                (target.x, target.y),
                sim::BODY_RADIUS,
            );
            let avoid = crate::plan_path(&world, p.pos, target, &blockers);
            info!(
                "PATHDBG 不避让 {:?} 拐点 / 避让 {:?} 拐点; 实体 {}",
                plain.as_ref().map(|v| v.len()),
                avoid.as_ref().map(|v| v.len()),
                blockers.len()
            );
        }
        match crate::plan_path(&world, p.pos, target, &blockers) {
            Some(path) if !path.is_empty() => {
                auto.goal = path.back().copied();
                auto.waypoints = path;
                info!(
                    "寻路: ({:.0},{:.0}) → ({:.0},{:.0}), {} 个拐点, 避开 {} 个实体",
                    p.pos.x,
                    p.pos.y,
                    target.x,
                    target.y,
                    auto.waypoints.len(),
                    blockers.len()
                );
            }
            // 已在原地 / 无路可走: 清掉旧路线, 不留半截状态
            _ => auto.clear(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::icon_scale;

    /// 倍率必须是 n 或 1/n — 非整数比例会让像素画发糊
    #[test]
    fn scale_is_texel_aligned() {
        for &(long, avail, ui) in &[
            (32.0, 32.0, 1.0),
            (32.0, 32.0, 0.833),
            (32.0, 32.0, 2.0),
            (96.0, 32.0, 1.0),
            (17.0, 60.0, 1.5),
            (1.0, 4.0, 0.5),
        ] {
            let k = icon_scale(long, avail, ui);
            assert!(k > 0.0, "倍率必须为正: {k}");
            let ok = k >= 1.0 && (k - k.round()).abs() < 1e-6
                || k < 1.0 && ((1.0 / k) - (1.0 / k).round()).abs() < 1e-6;
            assert!(
                ok,
                "倍率 {k} 既不是整数倍也不是 1/整数 (long={long} avail={avail} ui={ui})"
            );
        }
    }

    /// 900p 窗口 (UiScale 0.833) 下 32px 图标应保持 1:1 而不是被砍半
    #[test]
    fn native_icon_survives_sub_unit_ui_scale() {
        assert_eq!(icon_scale(32.0, super::BAG_CELL, 0.833), 1.0);
    }

    /// 4K (UiScale 2) 下应整数放大而不是留白
    #[test]
    fn scales_up_on_hidpi() {
        assert_eq!(icon_scale(32.0, super::BAG_CELL, 2.0), 2.0);
    }
}
