//! 技能施放与特效: 弹体/段动画/飘字(自 main.rs 机械拆出)。
use crate::*;

/// 技能特效闪光 (过渡版: 彩色扩散圈; M4 接原版 Magic 特效图库)
#[derive(Component)]
pub(crate) struct Fx {
    pub(crate) born: f64,
}

/// 原版技能特效帧动画 (100ms/帧, 播完自毁; blend 亮度透明)
#[derive(Component)]
pub(crate) struct EffectAnim {
    /// 特效名 (magic/<名>.mfl)
    pub(crate) fx: String,
    pub(crate) base: i32,
    /// 0 = 自适应: 按 10 槽块实测帧数 (起手段等按经典布局推导的段用)
    pub(crate) frames: u8,
    pub(crate) born: f64,
    pub(crate) px: f32,
    pub(crate) py: f32,
    /// >= 0: 等该 10 槽块 (起手段) 播完再开播 —— 二段命中衔接用
    pub(crate) wait_base: i32,
}

/// 技能飞行弹体: 从施放者直线飞向目标, 循环播 16 向飞行帧, 到达再播命中段
#[derive(Component)]
pub(crate) struct Projectile {
    /// 特效名 (magic/<名>.mfl)
    pub(crate) fx: String,
    /// 本向飞行行基址 ([`fxl::fly_base`], 帧数逐块实测)
    pub(crate) row: i32,
    pub(crate) born: f64,
    pub(crate) dur: f64,
    /// 起终点 (世界像素, y 向下为正)
    pub(crate) from: Vec2,
    pub(crate) to: Vec2,
    pub(crate) hit_base: i32,
    pub(crate) hit_frames: u8,
    /// >= 0: 起手段基址, 起手播完才起飞 (段顺序衔接)
    pub(crate) cast_base: i32,
}

pub(crate) fn skill_color(id: &str) -> Color {
    match id {
        "huoqiu" | "liehuo" => Color::srgb(1.0, 0.45, 0.1), // 火焰橙
        "leidian" => Color::srgb(0.5, 0.7, 1.0),            // 雷电蓝白
        "bingpaoxiao" => Color::srgb(0.4, 0.85, 1.0),       // 寒冰
        "zhiyu" => Color::srgb(0.4, 1.0, 0.5),              // 治疗绿
        "shidu" => Color::srgb(0.5, 0.8, 0.2),              // 毒
        "huofu" => Color::srgb(1.0, 0.85, 0.3),             // 符咒金
        "yeman" => Color::srgb(1.0, 0.6, 0.3),
        "shizihou" => Color::srgb(1.0, 0.8, 0.2),
        _ => Color::WHITE,
    }
}

/// 1/2/3 键施放技能: 自我施法直接放, 其余选射程内最近的怪
#[allow(clippy::too_many_arguments)]
pub(crate) fn cast_skills(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    chat: Res<hud::ChatState>,
    mut net: ResMut<Net>,
    remotes: Res<Remotes>,
    mut q: Query<&mut Player>,
) {
    if chat.active {
        return;
    }
    let Ok(mut p) = q.get_single_mut() else {
        return;
    };
    let idx = if keys.just_pressed(KeyCode::Digit1) {
        0
    } else if keys.just_pressed(KeyCode::Digit2) {
        1
    } else if keys.just_pressed(KeyCode::Digit3) {
        2
    } else if keys.just_pressed(KeyCode::Digit4) {
        3
    } else if keys.just_pressed(KeyCode::Digit5) {
        4
    } else {
        return;
    };
    let Some(s) = net.skills.get(idx).cloned() else {
        return;
    };
    let now = time.elapsed_secs_f64();
    if net.cds.get(&s.id).is_some_and(|&t| now < t) {
        return;
    }
    let target = if s.self_cast {
        None
    } else {
        // 射程内最近的活怪
        let found = remotes
            .0
            .iter()
            .filter(|(_, r)| r.image.is_some() && r.anim != 4 && r.owner.is_none())
            .map(|(id, r)| (id.clone(), (r.pos - p.pos).length(), r.pos))
            .filter(|(_, d, _)| *d <= s.range)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let Some(t) = found else {
            return; // 无目标不施放
        };
        Some(t)
    };
    // 治愈类: 目标启发式 — 掉血宝宝(自己或他人的)优先, 其次自己(掉血),
    // 再次附近其他玩家; 都没有则治自己 (target None)
    let target = if s.kind == "heal" {
        heal_target(&net, &remotes, &p, s.range.max(6.0))
    } else {
        target
    };
    if let Some((_, _, mp)) = &target {
        p.dir = dir8_from(mp.x - p.pos.x, mp.y - p.pos.y);
    }
    net.cds
        .insert(s.id.clone(), now + s.cooldown_ms as f64 / 1000.0);
    p.attack_start = Some(now);
    p.attack_base = if s.anim == "attack" {
        hum::ATTACK
    } else {
        hum::CAST
    };
    p.anim_t = 0.0;
    net.send(ClientMessage::UseSkill {
        skill_id: s.id,
        target_id: target.map(|(id, _, _)| id),
        position: None,
    });
}

/// 治愈目标启发式: 掉血宠物 → 自己(掉血) → 附近其他玩家 → 自己
fn heal_target(
    net: &Net,
    remotes: &Remotes,
    p: &Player,
    range: f64,
) -> Option<(String, f64, DVec2)> {
    // 掉血宠物 (任何主人的; 血条信息只有怪物实体带)
    let hurt_pet = remotes
        .0
        .iter()
        .filter(|(_, r)| r.owner.is_some() && r.anim != 4)
        .filter(|(_, r)| r.hp.is_some_and(|(cur, max)| cur > 0 && cur < max))
        .map(|(id, r)| (id.clone(), (r.pos - p.pos).length(), r.pos))
        .filter(|(_, d, _)| *d <= range)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    if hurt_pet.is_some() {
        return hurt_pet;
    }
    // 自己掉血: 治自己 (None = 服务端回落自身)
    if net.stat.is_some_and(|st| st.hp < st.max_hp) {
        return None;
    }
    // 附近其他玩家 (客户端不知其血量, 交给服务端封顶)
    remotes
        .0
        .iter()
        .filter(|(_, r)| r.image.is_none() && r.anim != 4)
        .map(|(id, r)| (id.clone(), (r.pos - p.pos).length(), r.pos))
        .filter(|(_, d, _)| *d <= range)
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

/// 开发钩子: MIRFORGE_CAST=skill_id[:秒][,skill_id[:秒]…] — 周期性轮流
/// 施放多个技能 (特效/战斗链路自查用)
pub(crate) fn dev_cast(
    time: Res<Time>,
    net: ResMut<Net>,
    remotes: Res<Remotes>,
    mut q: Query<&mut Player>,
    mut next_at: Local<f64>,
    mut slot: Local<usize>,
) {
    let Ok(spec) = std::env::var("MIRFORGE_CAST") else {
        return;
    };
    let Ok(mut p) = q.get_single_mut() else {
        return;
    };
    // 逗号分隔多技能轮播: zhaohuan:9,zhiyu:3 — 每次到点施放下一个
    let entries: Vec<(String, f64)> = spec
        .split(',')
        .map(|e| match e.split_once(':') {
            Some((a, b)) => (a.to_string(), b.parse().unwrap_or(2.0)),
            None => (e.to_string(), 2.0),
        })
        .collect();
    if entries.is_empty() {
        return;
    }
    let now = time.elapsed_secs_f64();
    if now < *next_at {
        return;
    }
    let (id, every) = entries[*slot % entries.len()].clone();
    let Some(s) = net.skills.iter().find(|s| s.id == id).cloned() else {
        *slot += 1; // 未学会的跳过, 不卡轮播
        return;
    };
    let target = if s.kind == "heal" {
        heal_target(&net, &remotes, &p, s.range.max(6.0))
    } else if s.self_cast {
        None
    } else {
        let Some(t) = remotes
            .0
            .iter()
            .filter(|(_, r)| r.image.is_some() && r.anim != 4 && r.owner.is_none())
            .map(|(tid, r)| (tid.clone(), (r.pos - p.pos).length(), r.pos))
            .filter(|(_, d, _)| *d <= s.range)
            .min_by(|a, b| a.1.total_cmp(&b.1))
        else {
            return; // 射程内无目标, 下帧再试
        };
        Some(t)
    };
    *slot += 1;
    *next_at = now + every;
    if let Some((_, _, mp)) = &target {
        p.dir = dir8_from(mp.x - p.pos.x, mp.y - p.pos.y);
    }
    p.attack_start = Some(now);
    p.attack_base = if s.anim == "attack" {
        hum::ATTACK
    } else {
        hum::CAST
    };
    p.anim_t = 0.0;
    net.send(ClientMessage::UseSkill {
        skill_id: s.id,
        target_id: target.map(|(t, _, _)| t),
        position: None,
    });
}

/// 特效步进: 扩散 + 渐隐
pub(crate) fn fx_step(
    mut commands: Commands,
    time: Res<Time>,
    mut world: ResMut<World>,
    mut images: ResMut<Assets<Image>>,
    mut q: Query<(Entity, &mut Transform, &mut Sprite, &Fx)>,
    mut q_anim: Query<
        (
            Entity,
            &mut Transform,
            &mut Sprite,
            &mut Visibility,
            &EffectAnim,
        ),
        Without<Fx>,
    >,
) {
    let now = time.elapsed_secs_f64();
    for (e, mut tf, mut sprite, fx) in q.iter_mut() {
        let age = (now - fx.born) as f32;
        if age > 0.5 {
            commands.entity(e).despawn();
            continue;
        }
        tf.scale = Vec3::splat(1.0 + age * 5.0);
        sprite.color.set_alpha(0.85 * (1.0 - age * 2.0));
    }
    // 原版特效帧动画: 100ms/帧, blend (加色近似) 解码
    for (e, mut tf, mut sp, mut vis, fx) in q_anim.iter_mut() {
        let layer = world.fx_layer(&fx.fx);
        let eff = if fx.frames == 0 {
            world.block_len(layer, fx.base, fxl::SLOT).min(10)
        } else {
            fx.frames
        };
        let delay = if fx.wait_base >= 0 {
            world.block_len(layer, fx.wait_base, fxl::SLOT).min(10) as f64 * 0.1
        } else {
            0.0
        };
        let age = now - fx.born - delay;
        if age < 0.0 {
            continue; // 前段未播完, 保持隐藏
        }
        let k = (age / 0.1) as i32;
        if k >= eff as i32 {
            commands.entity(e).despawn();
            continue;
        }
        if let Some(f) = world.frame_ex(layer, 0, fx.base + k, true) {
            world.ensure_pages(&mut images);
            sp.image = world.pages[f.page].clone();
            sp.rect = Some(f.rect);
            sp.anchor = Anchor::TopLeft;
            tf.translation.x = fx.px + f.off.x;
            tf.translation.y = -(fx.py + f.off.y);
            *vis = Visibility::Inherited;
        }
    }
}

/// 冲锋拖尾: 跟随施放者的方向性循环特效 (野蛮冲撞火焰冲击波)。
/// fly 段 16 向×10 槽, 8 向素材放偶数行 (row = dir×2); 100ms/帧循环,
/// 撞击包到达或走满时限即灭。
#[derive(Component)]
pub(crate) struct DashTrail {
    pub fx: String,
    /// 施放者: None = 本地玩家, Some(id) = 旁观远端
    pub caster: Option<String>,
    /// fly 段行基址 (fxl::FLY + row×SLOT)
    pub row_base: i32,
    pub born: f64,
    pub until: f64,
}

pub(crate) fn dash_trail_step(
    mut commands: Commands,
    time: Res<Time>,
    mut world: ResMut<World>,
    mut images: ResMut<Assets<Image>>,
    remotes: Res<Remotes>,
    q_player: Query<&Player>,
    mut q: Query<(
        Entity,
        &mut Transform,
        &mut Sprite,
        &mut Visibility,
        &DashTrail,
    )>,
) {
    let now = time.elapsed_secs_f64();
    for (e, mut tf, mut sp, mut vis, tr) in q.iter_mut() {
        if now >= tr.until {
            commands.entity(e).despawn();
            continue;
        }
        // 跟随施放者当前位置
        let pos = match &tr.caster {
            None => q_player.get_single().ok().map(|p| p.pos),
            Some(id) => remotes.0.get(id).map(|r| r.pos),
        };
        let Some(pos) = pos else {
            commands.entity(e).despawn();
            continue;
        };
        let layer = world.fx_layer(&tr.fx);
        let n = world.block_len(layer, tr.row_base, fxl::SLOT).min(10);
        if n == 0 {
            commands.entity(e).despawn();
            continue;
        }
        let k = (((now - tr.born) / 0.1) as i32) % n as i32;
        if let Some(f) = world.frame_ex(layer, 0, tr.row_base + k, true) {
            world.ensure_pages(&mut images);
            sp.image = world.pages[f.page].clone();
            sp.rect = Some(f.rect);
            sp.anchor = Anchor::TopLeft;
            let px = pos.x as f32 * CELL_W - CELL_W / 2.0;
            let py = pos.y as f32 * CELL_H - CELL_H / 2.0;
            tf.translation.x = px + f.off.x;
            tf.translation.y = -(py + f.off.y);
            tf.translation.z = 690.0;
            *vis = Visibility::Inherited;
        }
    }
}

/// 技能弹体步进: 直线插值飞行, 循环飞行帧; 到达 (或该向无帧) 时播命中段
pub(crate) fn projectile_step(
    mut commands: Commands,
    time: Res<Time>,
    mut world: ResMut<World>,
    mut images: ResMut<Assets<Image>>,
    mut q: Query<(
        Entity,
        &mut Transform,
        &mut Sprite,
        &mut Visibility,
        &Projectile,
    )>,
) {
    let now = time.elapsed_secs_f64();
    for (e, mut tf, mut sp, mut vis, pj) in q.iter_mut() {
        let layer = world.fx_layer(&pj.fx);
        let cast_dur = if pj.cast_base >= 0 {
            world.block_len(layer, pj.cast_base, fxl::SLOT).min(10) as f64 * 0.1
        } else {
            0.0
        };
        let tt = now - pj.born - cast_dur;
        if tt < 0.0 {
            continue; // 起手未播完, 弹体待发
        }
        let t = (tt / pj.dur).min(1.0);
        let k = world.block_len(layer, pj.row, fxl::SLOT).min(10);
        if t >= 1.0 || k == 0 {
            commands.entity(e).despawn();
            commands.spawn((
                Sprite::default(),
                Transform::from_xyz(pj.to.x, -pj.to.y, 700.0),
                Visibility::Hidden,
                EffectAnim {
                    fx: pj.fx.clone(),
                    base: pj.hit_base,
                    frames: pj.hit_frames,
                    born: now,
                    px: pj.to.x,
                    py: pj.to.y,
                    wait_base: -1,
                },
            ));
            continue;
        }
        let fi = pj.row + ((tt / 0.09) as i32) % k as i32;
        if let Some(f) = world.frame_ex(layer, 0, fi, true) {
            world.ensure_pages(&mut images);
            sp.image = world.pages[f.page].clone();
            sp.rect = Some(f.rect);
            sp.anchor = Anchor::TopLeft;
            let x = pj.from.x + (pj.to.x - pj.from.x) * t as f32;
            let y = pj.from.y + (pj.to.y - pj.from.y) * t as f32;
            tf.translation.x = x + f.off.x;
            tf.translation.y = -(y + f.off.y);
            *vis = Visibility::Inherited;
        }
    }
}

/// 伤害飘字上浮渐隐
pub(crate) fn float_damage(
    mut commands: Commands,
    time: Res<Time>,
    mut q: Query<(Entity, &mut Transform, &mut TextColor, &Floater)>,
) {
    let now = time.elapsed_secs_f64();
    for (e, mut tf, mut color, f) in q.iter_mut() {
        let age = (now - f.born) as f32;
        if age > 1.0 {
            commands.entity(e).despawn();
            continue;
        }
        tf.translation.y += 38.0 * time.delta_secs();
        color.0.set_alpha(1.0 - (age - 0.5).max(0.0) * 2.0);
    }
}
