//! 场景 NPC 渲染与点击判定(自 main.rs 机械拆出)。
use crate::*;

/// NPC 精灵的世界坐标包围盒 (由 npc_step 每帧写入, 供点击判定用)
///
/// 之前用固定的格子框判定, 但 NPC 精灵带帧偏移且向上延伸近百像素 ——
/// 点头/点身子都会落空, 只有脚下那一小块能点中。
#[derive(Resource, Default)]
pub struct NpcHits(pub(crate) HashMap<String, Rect>);

/// 当前打开的 NPC 商店
#[derive(Clone)]
pub struct NpcShop {
    pub npc_id: String,
    pub name: String,
    pub items: Vec<protocol::ShopItemInfo>,
    /// 回收价比例 (卖价 = 物品基准价 × 它)
    #[allow(dead_code)] // 回收价展示预留 (当前卖出计价在服务端)
    pub sell_rate: f64,
}

/// 当前显示的 NPC 对话页
#[derive(Clone)]
pub struct NpcDialog {
    pub npc_id: String,
    pub name: String,
    pub page: u32,
    pub text: String,
    pub options: Vec<protocol::NpcDialogOption>,
}

/// 场景 NPC 精灵 (随区域/热重载重建)
#[derive(Component)]
pub(crate) struct NpcSprite {
    id: String,
    /// NPC 形象编号 → Data/NPC/{image:02}.Lib
    image: u16,
    /// 格坐标 (服务器配置)
    x: f64,
    y: f64,
}

/// NPC 渲染: 站立 4 帧循环 (Crystal FrameSet.NPC Standing = 0..4 @ 450ms), 头顶名字
#[allow(clippy::too_many_arguments)]
pub(crate) fn npc_step(
    mut commands: Commands,
    time: Res<Time>,
    net: Res<Net>,
    mut world: ResMut<World>,
    mut images: ResMut<Assets<Image>>,
    skin: Res<hud::Skin>,
    mut hits: ResMut<NpcHits>,
    mut seen_rev: Local<u32>,
    q: Query<(Entity, &NpcSprite)>,
) {
    // 区域变更 / 配置热重载 → 全部重建
    if *seen_rev != net.npc_rev {
        *seen_rev = net.npc_rev;
        for (e, _) in q.iter() {
            commands.entity(e).despawn_recursive();
        }
        hits.0.clear();
        for n in &net.npcs {
            commands
                .spawn((
                    NpcSprite {
                        id: n.id.clone(),
                        image: n.image,
                        x: n.x,
                        y: n.y,
                    },
                    Transform::default(),
                    Visibility::default(),
                ))
                .with_children(|p| {
                    p.spawn((
                        Text2d::new(n.name.clone()),
                        TextFont {
                            font: skin.font.clone(),
                            font_size: 13.0,
                            ..default()
                        },
                        TextColor(Color::srgb(0.98, 0.85, 0.47)),
                        Transform::from_xyz(CELL_W / 2.0, 14.0, 0.01),
                    ));
                });
        }
        return; // 本帧只建实体, 下帧起取帧 (贴图晚一帧无碍)
    }
    let idx = ((time.elapsed_secs_f64() / 0.45) as i32) % 4;
    for (e, npc) in q.iter() {
        let Some(f) = world.frame(Layer::Npc(npc.image), 0, idx) else {
            continue;
        };
        world.ensure_pages(&mut images);
        let px = npc.x as f32 * CELL_W - CELL_W / 2.0 + f.off.x;
        let py = npc.y as f32 * CELL_H - CELL_H / 2.0 + f.off.y;
        commands.entity(e).insert((
            Sprite {
                image: world.pages[f.page].clone(),
                rect: Some(f.rect),
                anchor: Anchor::TopLeft,
                ..default()
            },
            // 与远程实体同一套排序 (按格 y), 略低于同格玩家
            Transform::from_xyz(px, -py, 10.0 + npc.y as f32 * 0.01 + 0.002),
        ));
        // Anchor::TopLeft: 精灵自 (px,-py) 向右向下铺开
        let size = f.rect.size();
        hits.0.insert(
            npc.id.clone(),
            Rect::new(px, -py - size.y, px + size.x, -py),
        );
    }
}
