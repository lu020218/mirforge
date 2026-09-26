//! 远程实体渲染: 插值行走/帧表寻址/血条/武器叠层(自 main.rs 机械拆出)。
use crate::*;
use bevy::asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

/// 其他玩家插值行走 + 精灵帧 (与本地玩家同一套 CArmour 帧表)
/// 玩家名牌颜色: 白名淡金 / 灰名灰 / 红名红
fn player_pk_color(pk: &str) -> Color {
    match pk {
        "grey" => Color::srgb(0.62, 0.62, 0.62),
        "red" => Color::srgb(0.95, 0.28, 0.25),
        _ => Color::srgb(0.98, 0.93, 0.62),
    }
}

/// 生成脚下选中光圈贴图: 128x64 金色椭圆环 + 内圈淡光
fn ring_image() -> Image {
    const W: usize = 128;
    const H: usize = 64;
    let mut data = vec![0u8; W * H * 4];
    for y in 0..H {
        for x in 0..W {
            let dx = (x as f32 - 63.5) / 62.0;
            let dy = (y as f32 - 31.5) / 30.5;
            let rr = (dx * dx + dy * dy).sqrt();
            let a = if rr <= 1.0 {
                // 主环 (0.9 附近最亮) + 靠环内侧的淡光晕
                let ring = 1.0 - ((rr - 0.9).abs() / 0.1).min(1.0);
                let glow = ((rr - 0.45) / 0.45).clamp(0.0, 1.0) * 0.22;
                (ring * 0.95 + glow).min(1.0)
            } else {
                0.0
            };
            let i = (y * W + x) * 4;
            data[i] = 255; // 金 #ffd876
            data[i + 1] = 216;
            data[i + 2] = 118;
            data[i + 3] = (a * 255.0) as u8;
        }
    }
    Image::new(
        Extent3d {
            width: W as u32,
            height: H as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    )
}

pub(crate) fn remote_step(
    mut commands: Commands,
    time: Res<Time>,
    mut world: ResMut<World>,
    mut images: ResMut<Assets<Image>>,
    mut remotes: ResMut<Remotes>,
    skin: Res<hud::Skin>,
    net: Res<Net>,
    mut ring: Local<Option<(Entity, Handle<Image>)>>,
) {
    let dt = time.delta_secs_f64();
    let now = time.elapsed_secs_f64();
    let mut gone = Vec::new();
    // 锁定目标的光圈落点 (循环里按该实体当帧包围盒回填)
    let mut ring_at: Option<(f32, f32, f32, f32)> = None; // (cx, cy, z, 宽)
    for (id, r) in remotes.0.iter_mut() {
        // 3s 未出现在广播里视为离场
        if now - r.last_seen > 3.0 {
            if let Some(ent) = r.entity.take() {
                commands.entity(ent).despawn();
            }
            if let Some((a, b)) = r.bar.take() {
                commands.entity(a).despawn();
                commands.entity(b).despawn();
            }
            if let Some(w) = r.wep_entity.take() {
                commands.entity(w).despawn();
            }
            if let Some(l) = r.label.take() {
                commands.entity(l).despawn();
            }
            gone.push(id.clone());
            continue;
        }
        // 朝目标插值: 速度贴近服务器实速 (怪物游荡 0.8/追击 1.8 格/s),
        // 距离积压时按 dist*4 加速收敛 — 快怪自动跟上, 慢怪不会瞬间追完
        let d = r.target - r.pos;
        let dist = d.length();
        let speed = match r.anim {
            2 => RUN_SPEED,
            1 => {
                if r.image.is_some() {
                    0.9
                } else {
                    WALK_SPEED
                }
            }
            _ => 0.0,
        };
        let step = (speed * dt).max(dist * 4.0 * dt);
        if dist > 0.02 && r.anim < 3 {
            // 8 向量化插值: 服务器移动同为 8 向懒转向, target 轨迹即折线;
            // 客户端严格重放 — 沿当前朝向走到投影耗尽才换向 (与服务器同规则)
            // 阈值须小于最慢实体的单包位移 (游荡 0.8 格/s ÷ 20Hz = 0.04),
            // 否则慢速怪永远走瞬移分支 → 身体平移而腿不动
            let v = sim::DIR8[r.dir];
            if d.x * v.0 + d.y * v.1 < 0.01 {
                r.dir = dir8_from(d.x, d.y);
            }
            let v = sim::DIR8[r.dir];
            let along = (d.x * v.0 + d.y * v.1).max(0.0);
            let adv = step.min(along);
            if adv > 0.0 {
                r.pos.x += v.0 * adv;
                r.pos.y += v.1 * adv;
                r.walk_phase += adv;
                r.last_move_t = now;
            }
        } else if r.anim < 3 && dist > 0.0 {
            // 余量 ≤0.02 格 (≤1px): 无感贴齐
            r.pos = r.target;
        }
        r.anim_t += dt;
        // 行走动画去抖: 最近 0.25s 内有实际位移才算在走
        let walking = r.anim < 3 && now - r.last_move_t < 0.25;
        // 脚步帧 = 位移相位 × 6 (走一格一轮), 与地面锁定不受插值快慢影响
        let foot = (r.walk_phase * 6.0) as usize % 6;
        // 帧表: 玩家 = packs 衣甲布局 (站0/走64/跑128, 每向8帧位);
        // 怪物 = 每库自适应 (基址/跨度探测 + 块内实帧数取模, 防踩空帧闪烁)
        let (layer, frame_idx) = if let Some(n) = r.image {
            let layer = Layer::Mon(n);
            let Some(MonMeta { base, stride, end }) = world.mon_meta(n, r.image_base) else {
                continue;
            };
            // 动作序号: 站0 走1 攻2 被击3 死4; (相位, 一次性)
            let (act, phase, oneshot) = match r.anim {
                4 => (4, (r.anim_t / 0.13) as usize, true),
                5 => (3, (r.anim_t / 0.12) as usize, true),
                3 => (2, (r.anim_t / 0.15) as usize, false),
                _ if walking => (1, (r.walk_phase * 6.0) as usize, false),
                _ => (0, (r.anim_t / 0.25) as usize, false),
            };
            let mut block = base + act * stride * 8 + r.dir as i32 * stride;
            // 段尾栅栏: 越过本怪的段就是同库下一只怪的帧, 绝不能取
            let mut k = if block >= end {
                0
            } else {
                world.block_len(layer, block, stride) as usize
            };
            if k == 0 && oneshot {
                // 死亡缺该方向 → 退本怪段内最后一块 (方向不对但动作对,
                // 比僵直站立或变成别的怪好)
                let last = base + ((end - base) / stride - 1).max(0) * stride;
                block = last;
                k = world.block_len(layer, block, stride) as usize;
            }
            if k == 0 {
                // 该动作段缺帧 → 退站立段; 站立也缺 → 退段首块
                block = base + r.dir as i32 * stride;
                k = if block >= end {
                    0
                } else {
                    world.block_len(layer, block, stride) as usize
                };
            }
            if k == 0 {
                block = base;
                k = world.block_len(layer, base, stride).max(1) as usize;
            }
            let idx = block
                + if oneshot {
                    phase.min(k.saturating_sub(1))
                } else {
                    phase % k.max(1)
                } as i32;
            (layer, idx as usize)
        } else {
            let dirb = r.dir * hum::DIR_STRIDE;
            let acting = r.act_start.filter(|t| now - t < 0.54);
            let idx = if r.anim == 4 {
                // 玩家倒地: 4 帧一次性, 停在最后一帧
                hum::DIE + dirb + ((r.anim_t / 0.15) as usize).min(3)
            } else if let Some(t) = acting {
                r.act_base + dirb + (((now - t) / 0.09) as usize).min(5)
            } else if walking && r.anim == 2 {
                hum::RUN + dirb + foot
            } else if walking {
                hum::WALK + dirb + foot
            } else {
                dirb + ((r.anim_t / 0.2) as usize % 4)
            };
            (Layer::Hum(r.armour), idx)
        };
        // 玩家外观缺库退 0 号裸模 (怪物缺库仍跳过 — 没有合理的替身)
        let Some(f) = world.frame(layer, 0, frame_idx as i32).or_else(|| {
            matches!(layer, Layer::Hum(n) if n != 0)
                .then(|| world.frame(Layer::Hum(0), 0, frame_idx as i32))
                .flatten()
        }) else {
            continue;
        };
        world.ensure_pages(&mut images);
        let px = r.pos.x as f32 * CELL_W - CELL_W / 2.0 + f.off.x;
        let py = r.pos.y as f32 * CELL_H - CELL_H / 2.0 + f.off.y;
        let z = 10.0 + r.pos.y as f32 * 0.01 + 0.004; // 略低于本地玩家
        let sprite = Sprite {
            image: world.pages[f.page].clone(),
            rect: Some(f.rect),
            anchor: Anchor::TopLeft,
            color: if r.stunned {
                Color::srgb(1.6, 1.6, 1.6) // 僵直提亮泛白 (受击硬直)
            } else if r.poisoned {
                Color::srgb(0.55, 1.0, 0.55) // 中毒染绿 (经典绿毒表现)
            } else {
                Color::WHITE
            },
            ..default()
        };
        let tf = Transform::from_xyz(px, -py, z);
        match r.entity {
            Some(ent) => {
                commands.entity(ent).insert((sprite, tf));
            }
            None => {
                r.entity = Some(commands.spawn((sprite, tf, Visibility::default())).id());
            }
        }
        // 远程玩家武器叠层 (同帧号, z 微高)
        if r.image.is_none() {
            let wf = r
                .weapon
                .and_then(|s| world.frame(Layer::Weapon(s), 0, frame_idx as i32));
            world.ensure_pages(&mut images);
            match (wf, r.wep_entity) {
                (Some(w), ent) => {
                    let bx = r.pos.x as f32 * CELL_W - CELL_W / 2.0;
                    let by = r.pos.y as f32 * CELL_H - CELL_H / 2.0;
                    let ws = Sprite {
                        image: world.pages[w.page].clone(),
                        rect: Some(w.rect),
                        anchor: Anchor::TopLeft,
                        ..default()
                    };
                    let wz = if matches!(r.dir, 0 | 5 | 6 | 7) {
                        z - 0.0005
                    } else {
                        z + 0.0005
                    };
                    let wt = Transform::from_xyz(bx + w.off.x, -(by + w.off.y), wz);
                    match ent {
                        Some(e) => {
                            commands.entity(e).insert((ws, wt, Visibility::Inherited));
                        }
                        None => {
                            r.wep_entity =
                                Some(commands.spawn((ws, wt, Visibility::default())).id());
                        }
                    }
                }
                (None, Some(e)) => {
                    commands.entity(e).insert(Visibility::Hidden);
                }
                (None, None) => {}
            }
        }
        // 受伤怪头顶血条 (满血/死亡中不显示)
        let show_bar =
            r.image.is_some() && r.anim != 4 && r.hp.is_some_and(|(cur, max)| cur > 0 && cur < max);
        if show_bar {
            let (cur, max) = r.hp.unwrap();
            let frac = cur as f32 / max as f32;
            let cx = r.pos.x as f32 * CELL_W - CELL_W / 2.0 + CELL_W / 2.0;
            let cy = -(r.pos.y as f32 * CELL_H - CELL_H / 2.0 - 52.0);
            let bg = (
                Sprite {
                    color: Color::srgba(0.05, 0.05, 0.08, 0.85),
                    custom_size: Some(Vec2::new(44.0, 6.0)),
                    ..default()
                },
                Transform::from_xyz(cx, cy, 650.0),
            );
            let fg = (
                Sprite {
                    // 宠物血条绿色 (友方一眼可辨)
                    color: if r.owner.is_some() {
                        Color::srgb(0.30, 0.85, 0.35)
                    } else {
                        Color::srgb(0.88, 0.25, 0.25)
                    },
                    custom_size: Some(Vec2::new(42.0 * frac, 4.0)),
                    anchor: Anchor::CenterLeft,
                    ..default()
                },
                Transform::from_xyz(cx - 21.0, cy, 651.0),
            );
            match r.bar {
                Some((b, f)) => {
                    commands.entity(b).insert(bg);
                    commands.entity(f).insert(fg);
                }
                None => {
                    let b = commands.spawn((bg.0, bg.1, Visibility::default())).id();
                    let f = commands.spawn((fg.0, fg.1, Visibility::default())).id();
                    r.bar = Some((b, f));
                }
            }
        } else if let Some((a, b)) = r.bar.take() {
            commands.entity(a).despawn();
            commands.entity(b).despawn();
        }
        // 名牌: 脚下一行 (经典 Mir 名字在人物下方); 死亡中不显示
        let show_label = !r.name.is_empty() && r.anim != 4;
        if show_label {
            // 按精灵实际包围盒定位: 水平居中、垂直贴脚底 — 各实体精灵
            // 尺寸差异大 (人物/骷髅/鸡), 按格中心算会高低不齐
            let size = f.rect.size();
            // 水平取逻辑格中心 (与血条同基准): 帧内容在包围盒里并不居中,
            // 按包围盒算会整体偏右
            let cx = r.pos.x as f32 * CELL_W;
            // 名牌落在精灵包围盒正中 (身体中心); z 高于血条 (650/651),
            // 免得被队友/宠物血条压住
            let cy = -(py + size.y / 2.0 - 8.0);
            let tf = Transform::from_xyz(cx, cy, 660.0);
            match r.label {
                Some(l) => {
                    commands.entity(l).insert(tf);
                    // 文本变化 (如宠物升级) 才重建, 免得每帧重排版
                    if r.label_text != r.name {
                        r.label_text = r.name.clone();
                        commands.entity(l).insert(Text2d::new(r.name.clone()));
                    }
                    // 玩家名色随善恶变化重染 (灰名/红名)
                    if r.image.is_none() && r.owner.is_none() && r.label_pk != r.pk {
                        r.label_pk = r.pk.clone();
                        commands.entity(l).insert(TextColor(player_pk_color(&r.pk)));
                    }
                }
                None => {
                    r.label_text = r.name.clone();
                    r.label_pk = r.pk.clone();
                    let color = if r.owner.is_some() {
                        Color::srgb(0.45, 0.95, 0.5) // 宠物 (友方) 绿
                    } else if r.image.is_some() {
                        Color::srgb(0.86, 0.86, 0.9) // 怪物 淡白
                    } else {
                        player_pk_color(&r.pk) // 其他玩家按善恶名色
                    };
                    let l = commands
                        .spawn((
                            Text2d::new(r.name.clone()),
                            TextFont {
                                font: skin.font.clone(),
                                font_size: 12.0,
                                ..default()
                            },
                            TextColor(color),
                            // 缺省锚点非居中会让名字整体偏右
                            Anchor::Center,
                            tf,
                        ))
                        .id();
                    r.label = Some(l);
                }
            }
        } else if let Some(l) = r.label.take() {
            commands.entity(l).despawn();
        }
        // 锁定目标: 记脚下光圈落点 (逻辑格底边中心 = 站立点; 精灵包围盒
        // 含烘焙投影会偏低, 不能用 bbox 底), 略低于本体 z 压在脚下
        if net.target.as_deref() == Some(id.as_str()) && r.anim != 4 {
            let size = f.rect.size();
            let cx = r.pos.x as f32 * CELL_W;
            let cy = -(r.pos.y as f32 * CELL_H + CELL_H / 2.0 - 6.0);
            let rz = 10.0 + r.pos.y as f32 * 0.01 + 0.002;
            let w = (size.x * 0.9).clamp(44.0, 120.0);
            ring_at = Some((cx, cy, rz, w));
        }
    }
    for id in gone {
        remotes.0.remove(&id);
    }
    // 选中光圈: 有锁定目标跟随其脚底, 无则隐藏 (实体常驻, 贴图只生成一次)
    let (ent, img) = match &*ring {
        Some(v) => v.clone(),
        None => {
            let img = images.add(ring_image());
            let ent = commands
                .spawn((
                    Sprite {
                        image: img.clone(),
                        ..default()
                    },
                    Transform::default(),
                    Visibility::Hidden,
                ))
                .id();
            *ring = Some((ent, img.clone()));
            (ent, img)
        }
    };
    match ring_at {
        Some((cx, cy, z, w)) => {
            commands.entity(ent).insert((
                Sprite {
                    image: img,
                    custom_size: Some(Vec2::new(w, w * 0.5)),
                    ..default()
                },
                Transform::from_xyz(cx, cy, z),
                Visibility::Inherited,
            ));
        }
        None => {
            commands.entity(ent).insert(Visibility::Hidden);
        }
    }
}

/// 当前挡路的实体圆心 (与服务端 blockers_for 同一套规则: 活着的怪 / NPC /
/// 其他玩家; 尸体不挡)。客户端预测必须和权威判定用同一规则, 否则会回弹。
pub(crate) fn collide_points(remotes: &Remotes, net: &Net) -> Vec<(f64, f64)> {
    remotes
        .0
        .values()
        .filter(|r| r.anim != 4)
        .map(|r| (r.pos.x, r.pos.y))
        .chain(net.npcs.iter().map(|n| (n.x, n.y)))
        .collect()
}

/// 收集当前该避开的实体位置 (怪物 / NPC / 其他玩家)
///
/// 只取自己附近的: 避让判定要在 A* 的每个候选格上跑一遍, 实体一多就成了
/// 平方级开销。远处的怪对眼下这段路线没意义 —— 走近了自然会重算。
pub(crate) fn avoid_points(remotes: &Remotes, net: &Net, near: DVec2) -> Vec<DVec2> {
    const RANGE: f64 = 50.0;
    remotes
        .0
        .values()
        .filter(|r| r.anim != 4) // 死亡中的不算
        .map(|r| r.pos)
        .chain(net.npcs.iter().map(|n| DVec2::new(n.x, n.y)))
        .filter(|q| (*q - near).length() < RANGE)
        .collect()
}

/// 从 `from` 到 `goal` 算一条避开实体的路; 避不开就退回只看地形的路
pub(crate) fn plan_path(
    world: &World,
    from: DVec2,
    goal: DVec2,
    blockers: &[DVec2],
) -> Option<std::collections::VecDeque<DVec2>> {
    let avoid = |cx: i32, cy: i32| {
        let c = DVec2::new(cx as f64 + 0.5, cy as f64 + 0.5);
        if (c - from).length() < AUTOPATH_FREE {
            return false; // 起点附近不避, 否则从怪堆里出不来
        }
        blockers.iter().any(|b| (c - *b).length() < AUTOPATH_AVOID)
    };
    let path = sim::find_path_avoiding(
        &world.walk,
        (from.x, from.y),
        (goal.x, goal.y),
        BODY_RADIUS,
        avoid,
    )
    // 绕不过去 (比如目标就被怪围着) 就退回地形路线, 总比没有强
    .or_else(|| sim::find_path(&world.walk, (from.x, from.y), (goal.x, goal.y), BODY_RADIUS))?;
    let mut out: std::collections::VecDeque<DVec2> =
        path.into_iter().map(|(x, y)| DVec2::new(x, y)).collect();
    // A* 是按格心规划的。人若卡在格子边缘 (贴着墙角), 朝下一个格心的量化方向
    // 可能正对着墙, 两个轴向滑动也都被挡 —— 表现就是路算出来了却一步不动。
    // 先插一个"回到本格中心"的拐点, 把人从墙边挪开, 后面的几何才成立。
    let center = DVec2::new(from.x.floor() + 0.5, from.y.floor() + 0.5);
    if (center - from).length() > 0.2
        && world
            .walk
            .is_walkable_circle(center.x, center.y, BODY_RADIUS)
    {
        out.push_front(center);
    }
    Some(out)
}
