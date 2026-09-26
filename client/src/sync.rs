//! 服务器消息同步: net_pump 全量消息处理 + 上报/重连(自 main.rs 机械拆出)。
use crate::*;

/// 处理服务器消息 (联机核心状态机)
#[allow(clippy::too_many_arguments)]
pub(crate) fn net_pump(
    mut commands: Commands,
    time: Res<Time>,
    mut net: ResMut<Net>,
    mut world: ResMut<World>,
    mut remotes: ResMut<Remotes>,
    mut next: ResMut<NextState<Screen>>,
    screen: Res<State<Screen>>,
    mut q_player: Query<&mut Player>,
    q_trails: Query<(Entity, &crate::fx::DashTrail)>,
    mut sfx: EventWriter<crate::audio::Sfx>,
) {
    let mut events = Vec::new();
    if let Some(c) = &net.client {
        while let Ok(ev) = c.rx.try_recv() {
            events.push(ev);
        }
    }
    for ev in events {
        match ev {
            net::NetEvent::Connected => {
                net.connected = true;
                net.status = "已连接, 协商协议...".into();
                net.send(ClientMessage::Hello {
                    version: PROTOCOL_VERSION,
                });
            }
            net::NetEvent::Disconnected => {
                net.connected = false;
                net.client = None;
                net.reconnect_at = Some(time.elapsed_secs_f64() + 2.0);
                net.status = "连接断开, 重连中...".into();
            }
            net::NetEvent::Msg(msg) => match msg {
                ServerMessage::HelloAck { .. } => {
                    // 重连场景: 有令牌与角色 → 直接 Resume 原位恢复
                    if let (Some(token), Some(cid)) = (net.token.clone(), net.character_id.clone())
                    {
                        net.status = "会话恢复中...".into();
                        net.send(ClientMessage::Resume {
                            token,
                            character_id: cid,
                        });
                    } else if let Ok(ticket) = std::env::var("MIRFORGE_TICKET") {
                        // 登录器拉起: 一次性票据免密进选角 (票据用后即焚,
                        // 之后的断线重连走 Resume 令牌, 与普通登录无异)
                        std::env::remove_var("MIRFORGE_TICKET");
                        net.status = "验证启动票据...".into();
                        net.send(ClientMessage::TicketAuth { ticket });
                    } else {
                        net.status = "等待登录器启动票据".into();
                    }
                }
                ServerMessage::Error { message } => net.status = message,
                ServerMessage::LoginResult {
                    success, message, ..
                } => {
                    net.status = message;
                    if success && *screen.get() == Screen::Login {
                        next.set(Screen::CharSelect);
                    }
                }
                ServerMessage::SessionToken { token } => net.token = Some(token),
                ServerMessage::CharacterList { characters } => net.characters = characters,
                ServerMessage::CharacterCreated { name, .. } => {
                    net.status = format!("角色 {name} 已创建");
                }
                ServerMessage::LoginSuccess { player_id, .. } => net.my_id = Some(player_id),
                ServerMessage::NpcDialog {
                    npc_id,
                    name,
                    page,
                    text,
                    options,
                } => {
                    net.dialog = Some(NpcDialog {
                        npc_id,
                        name,
                        page,
                        text,
                        options,
                    });
                    net.dialog_rev += 1;
                }
                ServerMessage::NpcShop {
                    npc_id,
                    name,
                    items,
                    sell_rate,
                } => {
                    net.shop = Some(NpcShop {
                        npc_id,
                        name,
                        items,
                        sell_rate,
                    });
                    net.shop_rev += 1;
                }
                ServerMessage::GoldChanged { gold } => {
                    net.gold = gold;
                    net.inv_rev += 1; // 背包底栏显示金币
                    if net.shop.is_some() {
                        net.shop_rev += 1; // 商店窗底栏也显示金币
                    }
                }
                ServerMessage::TradeInvited { from_id, from_name } => {
                    net.notices.push((
                        format!("{from_name} 请求与你交易"),
                        "info".into(),
                        time.elapsed_secs_f64(),
                    ));
                    net.trade_invite = Some((from_id, from_name));
                    net.trade_rev += 1;
                }
                ServerMessage::TradeState {
                    partner,
                    my_items,
                    their_items,
                    my_gold,
                    their_gold,
                    my_ok,
                    their_ok,
                } => {
                    net.trade = Some(crate::TradeView {
                        partner,
                        my_items,
                        their_items,
                        my_gold,
                        their_gold,
                        my_ok,
                        their_ok,
                    });
                    net.trade_invite = None;
                    net.trade_rev += 1;
                }
                ServerMessage::TradeClosed { reason, done } => {
                    net.trade = None;
                    net.trade_rev += 1;
                    let kind = if done { "info" } else { "warn" };
                    net.notices
                        .push((reason, kind.into(), time.elapsed_secs_f64()));
                    if net.notices.len() > 6 {
                        net.notices.remove(0);
                    }
                }
                ServerMessage::PkMode { mode } => {
                    net.pk_mode = mode;
                }
                ServerMessage::StorageState { npc_id, items, cap } => {
                    net.storage = Some(crate::StorageView { npc_id, items, cap });
                    net.storage_rev += 1;
                }
                ServerMessage::NpcDialogEnd => {
                    net.dialog = None;
                    net.dialog_rev += 1;
                }
                ServerMessage::NpcList { npcs } => {
                    net.npcs = npcs;
                    net.npc_rev += 1;
                }
                ServerMessage::ZoneChanged {
                    zone_id,
                    zone_name,
                    position,
                    minimap,
                    bgm,
                    safe_zones,
                } => {
                    net.safe_zones = safe_zones;
                    net.zone_name = zone_name;
                    net.zone_minimap = minimap;
                    net.zone_bgm = bgm.unwrap_or_default();
                    if net.storage.take().is_some() {
                        net.storage_rev += 1;
                    }
                    // 跨地图: 重载地图/行走网格, 回收旧分块与远程玩家
                    if zone_id.to_lowercase() != world.map_name && world.switch_map(&zone_id) {
                        for (_, e) in world.chunks.drain() {
                            commands.entity(e).despawn_recursive();
                        }
                        for (_, mut r) in remotes.0.drain() {
                            if let Some(ent) = r.entity.take() {
                                commands.entity(ent).despawn();
                            }
                            if let Some((a, b)) = r.bar.take() {
                                commands.entity(a).despawn();
                                commands.entity(b).despawn();
                            }
                            if let Some(l) = r.label.take() {
                                commands.entity(l).despawn();
                            }
                        }
                    }
                    let pos = DVec2::new(position.x, position.y);
                    if let Ok(mut p) = q_player.get_single_mut() {
                        p.pos = pos;
                        p.dying = None; // 复活/传送快照到达
                    } else {
                        commands.spawn((
                            Player {
                                pos,
                                dir: 4,
                                moving: false,
                                running: false,
                                anim_t: 0.0,
                                attack_start: None,
                                attack_base: hum::ATTACK,
                                dash_dir: DVec2::ZERO,
                                dash_left: 0.0,
                                label_anchor: Vec2::ZERO,
                                dying: None,
                            },
                            Sprite::default(),
                            Transform::default(),
                            Visibility::default(),
                        ));
                    }
                    net.acc = DVec2::ZERO;
                    net.status.clear();
                    next.set(Screen::InGame);
                }
                ServerMessage::ResumeFailed { message } => {
                    net.token = None;
                    net.character_id = None;
                    net.my_id = None;
                    net.status = format!("恢复失败: {message}");
                    next.set(Screen::Login);
                }
                ServerMessage::SkillList { skills } => {
                    net.skills = skills;
                    net.stat_rev += 1; // 技能面板跟 stat_rev 走
                }
                ServerMessage::QuestState { quests } => {
                    net.quests = quests;
                    net.quest_rev += 1;
                }
                ServerMessage::InventoryState {
                    inventory,
                    equipment,
                } => {
                    net.inventory = inventory;
                    net.equipment = equipment;
                    net.inv_rev += 1;
                }
                ServerMessage::PlayerStatus {
                    level,
                    experience,
                    required_experience,
                    hp,
                    max_hp,
                    mp,
                    max_mp,
                } => {
                    if net.stat.is_some_and(|st| level > st.level) {
                        sfx.send(crate::audio::Sfx::ui("ui/levelup"));
                    }
                    if hp <= 0 && net.stat.is_some_and(|st| st.hp > 0) {
                        if let Ok(mut p) = q_player.get_single_mut() {
                            p.dying = Some(time.elapsed_secs_f64());
                            p.moving = false;
                        }
                        sfx.send(crate::audio::Sfx {
                            name: "hum/die_m".into(),
                            vol: 1.0,
                        });
                    }
                    net.stat_rev += 1;
                    net.stat = Some(Stat {
                        level,
                        exp: experience,
                        req: required_experience,
                        hp,
                        max_hp,
                        mp,
                        max_mp,
                    });
                }
                ServerMessage::Notification {
                    message,
                    notification_type,
                } => {
                    info!("通知: {message}");
                    let now = time.elapsed_secs_f64();
                    net.notices.push((message.clone(), notification_type, now));
                    if net.notices.len() > 6 {
                        net.notices.remove(0);
                    }
                    net.chatlog.push(("系统".into(), message));
                    if net.chatlog.len() > 30 {
                        net.chatlog.remove(0);
                    }
                    net.notice_rev += 1;
                }
                ServerMessage::GroundItems { items } => {
                    net.ground = items;
                    net.ground_rev += 1;
                }
                ServerMessage::ChatMessage {
                    sender, content, ..
                } => {
                    net.chatlog.push((sender, content));
                    if net.chatlog.len() > 30 {
                        net.chatlog.remove(0);
                    }
                    net.notice_rev += 1;
                }
                ServerMessage::SkillEffect {
                    caster_id,
                    skill_id,
                    kind,
                    position,
                    targets,
                    level,
                    fx,
                    fx_base,
                    fx_frames,
                    anim,
                    stages,
                    src,
                    ..
                } => {
                    // 旁观视角: 施放者播挥砍/施法动作 (本地玩家自己已就地播过;
                    // anim 空 = 撞击特效补发包, 不重播动作)
                    if Some(&caster_id) != net.my_id.as_ref() && !anim.is_empty() {
                        if let Some(r) = remotes.0.get_mut(&caster_id) {
                            r.act_start = Some(time.elapsed_secs_f64());
                            r.act_base = if anim == "attack" {
                                hum::ATTACK
                            } else {
                                hum::CAST
                            };
                        }
                    }
                    // 施放/命中音: magic/{fx}_cast / _hit; 三段技命中音在
                    // 弹体到达时播 (fx.rs); 补发包 (anim 空) 不重播施放音
                    if !fx.is_empty() {
                        if let Ok(p) = q_player.get_single() {
                            let d = (DVec2::new(position.x, position.y) - p.pos).length();
                            if !anim.is_empty() {
                                if let Some(ev) =
                                    crate::audio::Sfx::at(format!("magic/{fx}_cast"), d)
                                {
                                    sfx.send(ev);
                                }
                            }
                            if stages < 3 && !targets.is_empty() {
                                if let Some(ev) =
                                    crate::audio::Sfx::at(format!("magic/{fx}_hit"), d)
                                {
                                    sfx.send(ev);
                                }
                            }
                        }
                    }
                    // 冲锋类分流: 起手包 (anim 非空) 起跟随拖尾 + 本地位移
                    // 预表现; 撞击包 (anim 空) 灭拖尾后走通用命中特效
                    if kind == "charge" {
                        let mine = Some(&caster_id) == net.my_id.as_ref();
                        if !anim.is_empty() {
                            // 起手: 本地玩家同参起冲锋 (见 player_move 冲锋分支)
                            let range = net
                                .skills
                                .iter()
                                .find(|sk| sk.id == skill_id)
                                .map(|sk| sk.range)
                                .unwrap_or(5.0);
                            let mut dashed = false;
                            if let Some(sp) = src {
                                let d = DVec2::new(position.x - sp.x, position.y - sp.y);
                                if d.length() > 0.05 {
                                    if mine {
                                        if let Ok(mut p) = q_player.get_single_mut() {
                                            p.dash_dir = d.normalize();
                                            p.dash_left = range;
                                        }
                                    }
                                    // 拖尾: 8 向素材放 fly 段偶数行 (row = dir×2)
                                    if !fx.is_empty() {
                                        let row = (sim::dir8_from(d.x, d.y) as i32) * 2;
                                        commands.spawn((
                                            Sprite::default(),
                                            Transform::from_xyz(0.0, 0.0, 690.0),
                                            Visibility::Hidden,
                                            crate::fx::DashTrail {
                                                fx: fx.clone(),
                                                caster: (!mine).then(|| caster_id.clone()),
                                                row_base: fxl::FLY + row * fxl::SLOT,
                                                born: time.elapsed_secs_f64(),
                                                until: time.elapsed_secs_f64()
                                                    + range / sim::DASH_SPEED
                                                    + 0.25,
                                            },
                                        ));
                                    }
                                    dashed = true;
                                }
                            }
                            let _ = dashed;
                            continue; // 起手包不走通用命中特效
                        }
                        // 撞击包: 灭该施放者的拖尾, 继续走通用命中特效
                        for (te, tr) in q_trails.iter() {
                            let hit = match &tr.caster {
                                None => mine,
                                Some(id) => id == &caster_id,
                            };
                            if hit {
                                commands.entity(te).despawn();
                            }
                        }
                    }
                    let color = skill_color(&skill_id);
                    // 4 级起 (超官设满级的私服玩法) 叠一圈金色冲击环, 一眼认出高修炼
                    let empowered = level >= 4;
                    let mut points = vec![DVec2::new(position.x, position.y)];
                    for t in &targets {
                        if let Some(r) = remotes.0.get(t) {
                            points.push(r.pos);
                        }
                    }
                    // 特效帧段随广播下发 (管理台配置); 帧数 0 = 无帧动画画扩散圈
                    let fx = (fx_frames > 0 && !fx.is_empty()).then_some((
                        fx,
                        fx_base as i32,
                        fx_frames,
                    ));
                    // 二/三段: 起手特效在施放者脚下。单技能标准文件固定布局:
                    // 起手@0, 飞行@10 (16 向×10 槽), 命中@170; 帧数逐块实测
                    let src_pt = src.map(|sp| DVec2::new(sp.x, sp.y));
                    let now_s = time.elapsed_secs_f64();
                    // 二段: 命中段等起手块 (@0) 播完再开 (三段由弹体到达自然衔接)
                    let hit_wait = match (stages, src_pt.is_some(), &fx) {
                        (2, true, Some(_)) => fxl::CAST,
                        _ => -1,
                    };
                    if stages >= 2 {
                        if let (Some(sp), Some((name, _, _))) = (src_pt, &fx) {
                            let cx = sp.x as f32 * CELL_W - CELL_W / 2.0;
                            let cy = sp.y as f32 * CELL_H - CELL_H / 2.0;
                            commands.spawn((
                                Sprite::default(),
                                Transform::from_xyz(cx, -cy, 700.0),
                                Visibility::Hidden,
                                EffectAnim {
                                    fx: name.clone(),
                                    base: fxl::CAST,
                                    frames: 0,
                                    born: now_s,
                                    px: cx,
                                    py: cy,
                                    wait_base: -1,
                                },
                            ));
                        }
                    }
                    for pt in points {
                        let px = pt.x as f32 * CELL_W - CELL_W / 2.0;
                        let py = pt.y as f32 * CELL_H - CELL_H / 2.0;
                        // 三段: 飞行弹体压阵, 命中段等到达再播
                        if stages >= 3 {
                            if let (Some(sp), Some((name, base, frames))) = (src_pt, &fx) {
                                let d = pt - sp;
                                if d.length() > 0.3 {
                                    // Mir 16 向: 0=上, 顺时针
                                    let row = fxl::fly_row(d.x, d.y);
                                    let sx = sp.x as f32 * CELL_W - CELL_W / 2.0;
                                    let sy = sp.y as f32 * CELL_H - CELL_H / 2.0;
                                    commands.spawn((
                                        Sprite::default(),
                                        Transform::from_xyz(sx, -sy, 700.0),
                                        Visibility::Hidden,
                                        Projectile {
                                            fx: name.clone(),
                                            row: fxl::fly_base(row),
                                            born: now_s,
                                            dur: (d.length() / fxl::FLY_SPEED).max(0.08),
                                            from: Vec2::new(sx, sy),
                                            to: Vec2::new(px, py),
                                            hit_base: *base,
                                            hit_frames: *frames,
                                            cast_base: fxl::CAST,
                                        },
                                    ));
                                    continue;
                                }
                            }
                        }
                        if empowered {
                            commands.spawn((
                                Sprite {
                                    color: Color::srgb(1.0, 0.85, 0.35),
                                    custom_size: Some(Vec2::splat(40.0)),
                                    ..default()
                                },
                                Transform::from_xyz(px, -py, 699.0),
                                Fx {
                                    born: time.elapsed_secs_f64(),
                                },
                            ));
                        }
                        match &fx {
                            // 原版 Magic 库帧动画特效
                            Some((name, base, frames)) => {
                                commands.spawn((
                                    Sprite::default(),
                                    Transform::from_xyz(px, -py, 700.0),
                                    Visibility::Hidden,
                                    EffectAnim {
                                        fx: name.clone(),
                                        base: *base,
                                        frames: *frames,
                                        born: time.elapsed_secs_f64(),
                                        px,
                                        py,
                                        wait_base: hit_wait,
                                    },
                                ));
                            }
                            // 无独立特效的技能 (刀光在人物动画): 淡色扩散圈
                            None => {
                                commands.spawn((
                                    Sprite {
                                        color,
                                        custom_size: Some(Vec2::splat(26.0)),
                                        ..default()
                                    },
                                    Transform::from_xyz(px, -py, 700.0),
                                    Fx {
                                        born: time.elapsed_secs_f64(),
                                    },
                                ));
                            }
                        }
                    }
                }
                ServerMessage::DamageNumber {
                    target_id, amount, ..
                } => {
                    let mine = net.my_id.as_deref() == Some(target_id.as_str());
                    if mine {
                        sfx.send(crate::audio::Sfx {
                            name: "hum/struck".into(),
                            vol: 0.9,
                        });
                    }
                    let pos = if mine {
                        q_player.get_single().ok().map(|p| p.pos)
                    } else {
                        remotes.0.get(&target_id).map(|r| r.pos)
                    };
                    if let Some(pos) = pos {
                        let px = pos.x as f32 * CELL_W - CELL_W / 2.0;
                        let py = pos.y as f32 * CELL_H - CELL_H / 2.0 - 44.0;
                        commands.spawn((
                            Text2d::new(format!("-{amount}")),
                            TextFont {
                                font_size: 22.0,
                                ..default()
                            },
                            TextColor(if mine {
                                Color::srgb(1.0, 0.3, 0.25) // 挨打红字
                            } else {
                                Color::srgb(1.0, 0.88, 0.35) // 输出黄字
                            }),
                            Transform::from_xyz(px, -py, 800.0),
                            Floater {
                                born: time.elapsed_secs_f64(),
                            },
                        ));
                    }
                }
                ServerMessage::StateUpdate { entities, .. } => {
                    let now = time.elapsed_secs_f64();
                    for e in entities {
                        // 自己: 权威纠偏 (预测与服务器同源 sim, 常态几乎零漂移)
                        if net.my_id.as_deref() == Some(e.id.as_str()) {
                            if let Some(n) = &e.name {
                                if net.my_name != *n {
                                    net.my_name = n.clone();
                                }
                            }
                            if let (Some(pos), Ok(mut p)) = (e.position, q_player.get_single_mut())
                            {
                                let server = DVec2::new(pos.x, pos.y);
                                if (server - p.pos).length() > 2.0 {
                                    p.pos = server;
                                    net.acc = DVec2::ZERO;
                                }
                            }
                            continue;
                        }
                        if e.removed == Some(true) {
                            if let Some(mut r) = remotes.0.remove(&e.id) {
                                if let Some(ent) = r.entity.take() {
                                    commands.entity(ent).despawn();
                                }
                                if let Some((a, b)) = r.bar.take() {
                                    commands.entity(a).despawn();
                                    commands.entity(b).despawn();
                                }
                                if let Some(l) = r.label.take() {
                                    commands.entity(l).despawn();
                                }
                            }
                            continue;
                        }
                        let r = remotes.0.entry(e.id.clone()).or_default();
                        r.last_seen = now;
                        // 图库号由服务端下发 (曾从 id 里解析, 但 id 格式一变就
                        // 静默退化成人物精灵, 见 EntityUpdate::image)
                        if let Some(img) = e.image {
                            r.image = Some(img);
                        }
                        if let Some(pos) = e.position {
                            let t = DVec2::new(pos.x, pos.y);
                            if r.entity.is_none() {
                                r.pos = t; // 首见直接落位
                            }
                            r.target = t;
                        }
                        if let Some(hp) = e.hp {
                            r.hp = Some(match r.hp {
                                Some((_, max)) => (hp.min(max), max),
                                None => (hp, hp.max(1)),
                            });
                        }
                        let anim = match e.animation.as_deref() {
                            Some("run") => 2,
                            Some("walk") => 1,
                            Some("attack") => 3,
                            Some("die") => 4,
                            // 怪物受击 (编号避开玩家 run=2, 不进移动插值判定)
                            Some("struck") => 5,
                            _ => 0,
                        };
                        if anim != r.anim {
                            r.anim_t = 0.0; // 动作切换从头播 (攻击/死亡一次性动画)
                            // 动作音: 怪物按音效基址 (-1 攻/-2 受/-3 死),
                            // 其他玩家挥砍/倒地; 按与本地玩家的距离衰减
                            if matches!(anim, 3 | 4 | 5) {
                                if let Ok(p) = q_player.get_single() {
                                    let d = (r.pos - p.pos).length();
                                    let name = match (r.image.is_some(), anim) {
                                        (true, 3) => Some(format!("mon/{:03}-1", r.sound)),
                                        (true, 5) => Some(format!("mon/{:03}-2", r.sound)),
                                        (true, 4) => Some(format!("mon/{:03}-3", r.sound)),
                                        (false, 3) => Some("hum/swing".to_string()),
                                        (false, 4) => Some("hum/die_m".to_string()),
                                        _ => None,
                                    };
                                    if let Some(n) = name {
                                        if let Some(ev) = crate::audio::Sfx::at(n, d) {
                                            sfx.send(ev);
                                        }
                                    }
                                }
                            }
                        }
                        r.anim = anim;
                        if let Some(d) = e.dir {
                            r.dir = (d as usize) % 8;
                        }
                        if let Some(a) = e.armour {
                            r.armour = a;
                        }
                        if let Some(b) = e.image_base {
                            r.image_base = b;
                        }
                        if let Some(p) = e.poisoned {
                            r.poisoned = p;
                        }
                        if let Some(st) = &e.statuses {
                            r.stunned = st.iter().any(|s| s == "stun");
                        }
                        if e.owner.is_some() {
                            r.owner = e.owner.clone();
                        }
                        if let Some(n) = &e.name {
                            if r.name != *n {
                                r.name = n.clone();
                            }
                        }
                        if e.level.is_some() {
                            r.level = e.level;
                        }
                        if let Some(sd) = e.sound {
                            r.sound = sd;
                        }
                        if let Some(pk) = &e.pk {
                            if r.pk != *pk {
                                r.pk = pk.clone();
                            }
                        }
                        if e.weapon.is_some() {
                            r.weapon = e.weapon;
                        }
                    }
                }
                _ => {}
            },
        }
    }
}

/// 断线到点重连
pub(crate) fn net_reconnect(time: Res<Time>, mut net: ResMut<Net>) {
    let Some(at) = net.reconnect_at else { return };
    if time.elapsed_secs_f64() < at {
        return;
    }
    net.reconnect_at = None;
    if let Some(url) = net.url.clone() {
        net.status = "重连中...".into();
        net.client = Some(net::connect(url));
    }
}

/// 20Hz 上报本地位移
pub(crate) fn net_send(time: Res<Time>, mut net: ResMut<Net>) {
    if !net.connected || net.my_id.is_none() {
        return;
    }
    let now = time.elapsed_secs_f64();
    if now - net.last_send < 0.05 || net.acc == DVec2::ZERO {
        return;
    }
    net.last_send = now;
    let acc = net.acc;
    net.acc = DVec2::ZERO;
    net.send(ClientMessage::Move {
        direction: protocol::Position { x: acc.x, y: acc.y },
    });
}
