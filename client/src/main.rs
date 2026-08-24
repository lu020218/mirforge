//! MirForge 客户端（Bevy）。
//!
//! M1 角色行走版：直读原版资源渲染地图与角色（CArmour 8 向动画），
//! 分块按需生成与回收，帧图按需解码进运行时图集；移动判定走共享 sim crate。
//!
//! 环境变量：
//! - `MIRFORGE_RES`  资源根目录（必需，指向含 Map/ 与图库的目录）
//! - `MIRFORGE_MAP`  地图文件名（默认 `0.map`）
//! - `MIRFORGE_START` 初始镜头格坐标（默认 `330,150`，0.map 的比奇城一带）
//!
//! 操作（经典传奇）：鼠标左键按住 = 朝光标走路，右键按住 = 跑步（均沿墙滑行）；
//! PageUp/PageDown 或 +/- 缩放，F 键 1x/2x/3x 整数缩放循环。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bevy::asset::RenderAssetUsages;
use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::math::DVec2;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::sprite::Anchor;
use bevy::window::PresentMode;
use bevy_egui::{egui, EguiContexts, EguiPlugin};
use mir_atlas::{AtlasCpu, PAGE_SIZE};
use mir_formats::crystal_lib::CrystalLib;
use mir_formats::map::MirMap;
use sim::{dir8_from, WalkGrid, BODY_RADIUS};

const CELL_W: f32 = 48.0;
const CELL_H: f32 = 32.0;
const CHUNK: i32 = 16; // 格/块
const VIEW_MARGIN: i32 = 1; // 视口外多保留的块圈数

fn main() {
    App::new()
        .add_plugins(
            DefaultPlugins
                .set(ImagePlugin::default_nearest())
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "MirForge".into(),
                        present_mode: PresentMode::AutoVsync,
                        ..default()
                    }),
                    ..default()
                }),
        )
        .add_plugins((EguiPlugin, FrameTimeDiagnosticsPlugin))
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                player_move,
                camera_follow,
                camera_control,
                stream_chunks,
                player_sprite,
                upload_dirty_pages,
                debug_panel,
            )
                .chain(),
        )
        .run();
}

// ─────────── 资源 ───────────

/// 帧标识：图层类别 + 库号 + 帧号
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Layer {
    Back,
    Mid,
    Front,
    /// 角色 (CArmour)
    Hum,
}

#[derive(Clone, Copy)]
struct FrameRef {
    page: usize,
    rect: Rect,
    /// Mir 帧自带偏移
    off: Vec2,
    size: Vec2,
}

#[derive(Resource)]
struct World {
    map: MirMap,
    lib_dir: PathBuf,
    libs: HashMap<String, Option<CrystalLib>>,
    atlas: AtlasCpu,
    pages: Vec<Handle<Image>>,
    frames: HashMap<(Layer, i16, i32), Option<FrameRef>>,
    chunks: HashMap<(i32, i32), Entity>,
    hum: Option<CrystalLib>,
    walk: WalkGrid,
}

impl World {
    fn lib_name(layer: Layer, front_lib: i16) -> Option<String> {
        // 与既有约定一致: back→Tiles, mid→SmTiles, front: 0=Tiles 1=SmTiles 2=Objects n=Objects{n-1}
        Some(match layer {
            Layer::Hum => return None, // Hum 专用库, 不走目录查找
            Layer::Back => "Tiles".into(),
            Layer::Mid => "SmTiles".into(),
            Layer::Front => match front_lib {
                0 => "Tiles".into(),
                1 => "SmTiles".into(),
                2 => "Objects".into(),
                n if n > 2 => format!("Objects{}", n - 1),
                _ => return None,
            },
        })
    }

    fn open_lib(&mut self, name: &str) -> Option<&CrystalLib> {
        if !self.libs.contains_key(name) {
            let mut lib = None;
            for cand in [format!("{name}.Lib"), format!("{name}.lib")] {
                let p = self.lib_dir.join(&cand);
                if p.exists() {
                    if let Ok(data) = std::fs::read(&p) {
                        lib = CrystalLib::parse(data).ok();
                        break;
                    }
                }
            }
            self.libs.insert(name.to_string(), lib);
        }
        self.libs.get(name).and_then(|l| l.as_ref())
    }

    /// 取帧（按需解码进图集）
    fn frame(&mut self, layer: Layer, front_lib: i16, idx: i32) -> Option<FrameRef> {
        let key = (layer, front_lib, idx);
        if let Some(cached) = self.frames.get(&key) {
            return *cached;
        }
        let fref = (|| {
            let img = if layer == Layer::Hum {
                self.hum.as_ref()?.image(idx as usize).ok().flatten()?
            } else {
                let name = Self::lib_name(layer, front_lib)?;
                let lib = self.open_lib(&name)?;
                lib.image(idx as usize).ok().flatten()?
            };
            let placed = self
                .atlas
                .insert(img.width as u32, img.height as u32, &img.rgba)?;
            Some(FrameRef {
                page: placed.page,
                rect: Rect::new(
                    placed.x as f32,
                    placed.y as f32,
                    (placed.x + img.width as u32) as f32,
                    (placed.y + img.height as u32) as f32,
                ),
                off: Vec2::new(img.offset_x as f32, img.offset_y as f32),
                size: Vec2::new(img.width as f32, img.height as f32),
            })
        })();
        self.frames.insert(key, fref);
        fref
    }
}

// ─────────── 启动 ───────────

fn setup(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let root = std::env::var("MIRFORGE_RES").unwrap_or_else(|_| {
        error!("请设置 MIRFORGE_RES 指向传奇资源目录");
        std::process::exit(2);
    });
    let idx = mir_formats::scan::ResourceIndex::scan(Path::new(&root));
    let map_name = std::env::var("MIRFORGE_MAP").unwrap_or_else(|_| "0.map".into());
    let Some(entry) = idx.maps.iter().find(|m| {
        m.path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(&map_name))
    }) else {
        error!(
            "资源目录中找不到地图 {map_name} (共扫描到 {} 张)",
            idx.maps.len()
        );
        std::process::exit(2);
    };
    let map = mir_formats::map::parse(&std::fs::read(&entry.path).expect("读地图失败"))
        .expect("解析地图失败");
    // 图库目录 = Tiles.Lib 所在目录
    let lib_set = std::env::var("MIRFORGE_LIBSET").unwrap_or_else(|_| "WemadeMir2".into());
    // 资源目录可能含多套图库 (WemadeMir2/ShandaMir2/WemadeMir3), 地图与图库必须同套;
    // 优先取路径含 MIRFORGE_LIBSET (默认 WemadeMir2) 的 Tiles.Lib, 否则取第一个
    let tiles: Vec<_> = idx
        .libs
        .iter()
        .filter(|l| {
            l.path
                .file_stem()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("tiles"))
        })
        .collect();
    let lib_dir = tiles
        .iter()
        .find(|l| {
            l.path
                .to_string_lossy()
                .to_lowercase()
                .contains(&lib_set.to_lowercase())
        })
        .or_else(|| tiles.first())
        .map(|l| l.path.parent().unwrap().to_path_buf())
        .unwrap_or_else(|| {
            error!("资源目录中找不到 Tiles.Lib");
            std::process::exit(2);
        });
    info!(
        "地图 {map_name}: {:?} {}x{}, 图库目录 {:?}",
        map.kind, map.width, map.height, lib_dir
    );

    let start = std::env::var("MIRFORGE_START").unwrap_or_else(|_| "330,150".into());
    let (sx, sy) = start
        .split_once(',')
        .and_then(|(a, b)| Some((a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?)))
        .unwrap_or((330.0, 150.0));

    commands.spawn((
        Camera2d,
        Transform::from_xyz(sx * CELL_W, -sy * CELL_H, 1000.0),
    ));
    // 角色库: CArmour/00.Lib (男 0..808, 女 808..1616; 站 0+4/向, 走 32+6/向, 跑 80+6/向)
    let hum = idx
        .libs
        .iter()
        .find(|l| {
            l.path.to_string_lossy().to_lowercase().contains("carmour")
                && l.path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s == "00")
        })
        .and_then(|l| std::fs::read(&l.path).ok())
        .and_then(|d| CrystalLib::parse(d).ok());
    if hum.is_none() {
        warn!("未找到 CArmour/00.Lib, 角色将不可见");
    }
    // 行走网格: 原版地图格级阻挡 → 1/4 子格
    let cells = map.cells.clone();
    let (mw, mh) = (map.width, map.height);
    let walk = WalkGrid::from_cells(mw, mh, |x, y| cells[(y * mw + x) as usize].blocked);
    // 玩家
    commands.spawn((
        Player {
            pos: DVec2::new(sx as f64 + 0.5, sy as f64 + 0.5),
            dir: 4,
            moving: false,
            running: false,
            anim_t: 0.0,
        },
        Sprite::default(),
        Transform::default(),
        Visibility::default(),
    ));
    info!("玩家已生成 @({sx},{sy})");
    // 预建 8 页图集纹理 (不够时 upload 系统按需补)
    let mut pages = Vec::new();
    for _ in 0..8 {
        pages.push(images.add(blank_page()));
    }
    commands.insert_resource(World {
        map,
        lib_dir,
        libs: HashMap::new(),
        atlas: AtlasCpu::default(),
        pages,
        frames: HashMap::new(),
        chunks: HashMap::new(),
        hum,
        walk,
    });
}

fn blank_page() -> Image {
    Image::new(
        Extent3d {
            width: PAGE_SIZE,
            height: PAGE_SIZE,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        vec![0; (PAGE_SIZE * PAGE_SIZE * 4) as usize],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    )
}

// ─────────── 玩家 ───────────

/// 玩家状态 (位置为格坐标, 连续浮点)
#[derive(Component)]
struct Player {
    pos: DVec2,
    /// Mir 8 向 (0=上, 顺时针)
    dir: usize,
    moving: bool,
    running: bool,
    anim_t: f64,
}

// 速度对齐原版节奏: 走路一步(1 格)≈0.6s, 跑步一个周期跨 2 格
const WALK_SPEED: f64 = 1.7; // 格/秒
const RUN_SPEED: f64 = 3.4;
// 动画与位移锁定: 走/跑一个 6 帧周期恰好覆盖 1/2 格, 脚步不打滑(否则视觉发飘)
const WALK_FRAME_DT: f64 = 1.0 / (WALK_SPEED * 6.0);
const RUN_FRAME_DT: f64 = 2.0 / (RUN_SPEED * 6.0);
/// 光标离角色近于此距离(格)时不再追(防原地抖动)
const CURSOR_DEADZONE: f64 = 0.4;

/// 经典传奇操作: 左键按住=朝光标走路, 右键按住=跑步 (点到 NPC/怪的分流留待后续实体系统)
fn player_move(
    time: Res<Time>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut ev_cursor: EventReader<CursorMoved>,
    q_cam: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    world: Res<World>,
    mut last_cursor: Local<Option<Vec2>>,
    mut q: Query<&mut Player>,
) {
    let Ok(mut p) = q.get_single_mut() else {
        return;
    };
    // 光标屏幕位置走事件流记忆: Window::cursor_position 在光标离窗时清 None,
    // 事件不会——按住拖出窗口也能沿最后方向继续走 (经典手感)
    for e in ev_cursor.read() {
        *last_cursor = Some(e.position);
    }
    let run = buttons.pressed(MouseButton::Right);
    let held = buttons.pressed(MouseButton::Left) || run;
    let mut v = DVec2::ZERO;
    if held {
        if let (Some(cursor), Ok((cam, cam_tf))) = (*last_cursor, q_cam.get_single()) {
            if let Ok(wpt) = cam.viewport_to_world_2d(cam_tf, cursor) {
                // 世界像素 → 格坐标 (Bevy y 向上, 世界 y 向下取负还原); 每帧换算,
                // 镜头滚动时朝向随光标屏幕位置更新
                let target = DVec2::new(
                    wpt.x as f64 / CELL_W as f64,
                    -(wpt.y as f64) / CELL_H as f64,
                );
                let d = target - p.pos;
                if d.length() > CURSOR_DEADZONE {
                    // 方向吸附 8 向: 身体走向与精灵朝向结构上一致 (阶梯路径, 经典手感)
                    let dir = dir8_from(d.x, d.y);
                    v = DVec2::new(sim::DIR8[dir].0, sim::DIR8[dir].1);
                }
            }
        }
    }
    let dt = time.delta_secs_f64();
    if v == DVec2::ZERO {
        if p.moving {
            p.moving = false;
            p.anim_t = 0.0;
        } else {
            p.anim_t += dt;
        }
        return;
    }
    let speed = if run { RUN_SPEED } else { WALK_SPEED };
    let v = v * speed * dt;
    let (nx, ny) = world.walk.try_move(p.pos.x, p.pos.y, v.x, v.y, BODY_RADIUS);
    let moved = (nx - p.pos.x).abs() > 1e-9 || (ny - p.pos.y).abs() > 1e-9;
    if moved {
        p.dir = dir8_from(nx - p.pos.x, ny - p.pos.y);
        p.pos = DVec2::new(nx, ny);
    }
    if moved != p.moving || (moved && run != p.running) {
        p.anim_t = 0.0;
    }
    p.moving = moved;
    p.running = run;
    if moved {
        p.anim_t += dt;
    }
}

/// F3 调试面板 (egui 默认字体无中文, 面板用英文)
#[allow(clippy::too_many_arguments)]
fn debug_panel(
    mut ctx: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    mut show: Local<bool>,
    diag: Res<DiagnosticsStore>,
    world: Res<World>,
    q_player: Query<&Player>,
    q_proj: Query<&OrthographicProjection, With<Camera2d>>,
) {
    if keys.just_pressed(KeyCode::F3) {
        *show = !*show;
    }
    if !*show {
        return;
    }
    let fps = diag
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|d| d.smoothed())
        .unwrap_or(0.0);
    egui::Window::new("Debug (F3)")
        .default_pos((8.0, 8.0))
        .show(ctx.ctx_mut(), |ui| {
            ui.label(format!("FPS: {fps:.0}"));
            if let Ok(p) = q_player.get_single() {
                let state = match (p.moving, p.running) {
                    (true, true) => "run",
                    (true, false) => "walk",
                    _ => "stand",
                };
                ui.label(format!(
                    "player: ({:.2}, {:.2}) dir={} {state}",
                    p.pos.x, p.pos.y, p.dir
                ));
            }
            if let Ok(proj) = q_proj.get_single() {
                ui.label(format!("zoom: {:.2}x", 1.0 / proj.scale));
            }
            ui.label(format!(
                "map: {:?} {}x{}",
                world.map.kind, world.map.width, world.map.height
            ));
            ui.separator();
            ui.label(format!(
                "chunks: {}  frame cache: {}",
                world.chunks.len(),
                world.frames.len()
            ));
            ui.label(format!("atlas pages: {}", world.atlas.pages.len()));
            for i in 0..world.atlas.pages.len() {
                ui.label(format!(
                    "  page {i}: {:.0}% full",
                    world.atlas.fill_ratio(i) * 100.0
                ));
            }
        });
}

/// 按动作/方向/时间挑帧并更新精灵与变换
fn player_sprite(mut world: ResMut<World>, mut q: Query<(&Player, &mut Sprite, &mut Transform)>) {
    let Ok((p, mut sprite, mut tf)) = q.get_single_mut() else {
        return;
    };
    // 帧表 (男, CArmour 实证布局): 站 0 + dir*4 + f(4, 200ms); 走 32 + dir*6; 跑 80 + dir*6
    let frame_idx = if p.moving && p.running {
        80 + p.dir * 6 + ((p.anim_t / RUN_FRAME_DT) as usize % 6)
    } else if p.moving {
        32 + p.dir * 6 + ((p.anim_t / WALK_FRAME_DT) as usize % 6)
    } else {
        p.dir * 4 + ((p.anim_t / 0.2) as usize % 4)
    };
    let Some(f) = world.frame(Layer::Hum, 0, frame_idx as i32) else {
        warn_once!("角色帧 {frame_idx} 不可用");
        return;
    };
    sprite.image = world.pages[f.page.min(world.pages.len() - 1)].clone();
    sprite.rect = Some(f.rect);
    sprite.anchor = Anchor::TopLeft;
    // Mir 角色帧偏移相对所在格左上角; 位置取格坐标向下取整的格原点 + 帧内偏移 + 连续余量
    let px = p.pos.x as f32 * CELL_W - CELL_W / 2.0 + f.off.x;
    let py = p.pos.y as f32 * CELL_H - CELL_H / 2.0 + f.off.y;
    // 与前景高物件同一行深度体系; +0.005 让同行时角色压在物件之上
    let z = 10.0 + p.pos.y as f32 * 0.01 + 0.005;
    tf.translation = Vec3::new(px, -py, z);
}

fn camera_follow(q_player: Query<&Player>, mut q_cam: Query<&mut Transform, With<Camera2d>>) {
    let (Ok(p), Ok(mut cam)) = (q_player.get_single(), q_cam.get_single_mut()) else {
        return;
    };
    cam.translation.x = (p.pos.x as f32 - 0.5) * CELL_W;
    cam.translation.y = -((p.pos.y as f32 - 0.5) * CELL_H);
}

// ─────────── 镜头 ───────────

fn camera_control(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut q: Query<&mut OrthographicProjection, With<Camera2d>>,
) {
    let Ok(mut proj) = q.get_single_mut() else {
        return;
    };

    if keys.pressed(KeyCode::PageUp) || keys.pressed(KeyCode::Equal) {
        proj.scale = (proj.scale * (1.0 - time.delta_secs())).max(0.2);
    }
    if keys.pressed(KeyCode::PageDown) || keys.pressed(KeyCode::Minus) {
        proj.scale = (proj.scale * (1.0 + time.delta_secs())).min(6.0);
    }
    if keys.just_pressed(KeyCode::KeyF) {
        // 整数倍缩放循环 1x → 2x → 3x (高分屏像素锐利档)
        proj.scale = match proj.scale {
            s if s > 0.9 => 0.5,
            s if s > 0.4 => 1.0 / 3.0,
            _ => 1.0,
        };
    }
}

// ─────────── 地图分块流送 ───────────

fn stream_chunks(
    mut commands: Commands,
    mut world: ResMut<World>,
    q_cam: Query<(&Transform, &OrthographicProjection), With<Camera2d>>,
    windows: Query<&Window>,
) {
    let Ok((cam, proj)) = q_cam.get_single() else {
        return;
    };
    let Ok(win) = windows.get_single() else {
        return;
    };
    let half_w = win.width() * 0.5 * proj.scale;
    let half_h = win.height() * 0.5 * proj.scale;
    let (cx, cy) = (cam.translation.x, -cam.translation.y);
    let c0x = (((cx - half_w) / CELL_W).floor() as i32 / CHUNK - VIEW_MARGIN).max(0);
    let c1x = ((cx + half_w) / CELL_W).ceil() as i32 / CHUNK + VIEW_MARGIN;
    let c0y = (((cy - half_h) / CELL_H).floor() as i32 / CHUNK - VIEW_MARGIN).max(0);
    let c1y = ((cy + half_h) / CELL_H).ceil() as i32 / CHUNK + VIEW_MARGIN;

    // 回收视野外的块
    let keep: Vec<(i32, i32)> = world
        .chunks
        .keys()
        .filter(|(kx, ky)| *kx < c0x - 1 || *kx > c1x + 1 || *ky < c0y - 1 || *ky > c1y + 1)
        .copied()
        .collect();
    for k in keep {
        if let Some(e) = world.chunks.remove(&k) {
            commands.entity(e).despawn_recursive();
        }
    }
    // 生成缺失的块 (每帧限量, 避免首屏卡顿)
    let mut budget = 6;
    for ky in c0y..=c1y {
        for kx in c0x..=c1x {
            if budget == 0 {
                return;
            }
            if world.chunks.contains_key(&(kx, ky)) {
                continue;
            }
            let e = spawn_chunk(&mut commands, &mut world, kx, ky);
            world.chunks.insert((kx, ky), e);
            budget -= 1;
        }
    }
}

fn spawn_chunk(commands: &mut Commands, world: &mut World, kx: i32, ky: i32) -> Entity {
    let mut sprites: Vec<(Sprite, Transform)> = Vec::new();
    let (w, h) = (world.map.width as i32, world.map.height as i32);
    for cy in ky * CHUNK..(ky + 1) * CHUNK {
        for cx in kx * CHUNK..(kx + 1) * CHUNK {
            if cx < 0 || cy < 0 || cx >= w || cy >= h {
                continue;
            }
            let cell = *world.map.cell(cx as u32, cy as u32).unwrap();
            // back: 96×64 大砖只画偶数格 (覆盖 2×2)
            if cell.back >= 0 && cx % 2 == 0 && cy % 2 == 0 {
                if let Some(f) = world.frame(Layer::Back, cell.back_lib, cell.back) {
                    sprites.push(sprite_at(world, f, cx, cy, 0.0, false));
                }
            }
            if cell.mid >= 0 {
                if let Some(f) = world.frame(Layer::Mid, cell.mid_lib, cell.mid) {
                    sprites.push(sprite_at(world, f, cx, cy, 1.0, false));
                }
            }
            if cell.front >= 0 {
                if let Some(f) = world.frame(Layer::Front, cell.front_lib, cell.front) {
                    // 前景层沿用 Crystal 画序: 高 32/64 的平铺地表在 floor 阶段 (角色之下),
                    // 更高的物件才按行深度参与遮挡 (下行画在上行前)
                    let h = f.size.y;
                    let z = if h == CELL_H || h == CELL_H * 2.0 {
                        2.0
                    } else {
                        10.0 + cy as f32 * 0.01
                    };
                    sprites.push(sprite_at(world, f, cx, cy, z, true));
                }
            }
        }
    }
    let mut parent = commands.spawn((Transform::default(), Visibility::default()));
    parent.with_children(|p| {
        for (s, t) in sprites {
            p.spawn((s, t));
        }
    });
    parent.id()
}

fn sprite_at(
    world: &World,
    f: FrameRef,
    cx: i32,
    cy: i32,
    z: f32,
    front: bool,
) -> (Sprite, Transform) {
    // Mir 放置: 左上角 = (cx*48 + off.x, cy*32 + off.y - (h - 32)); back/mid off 恒为绘制原点
    let (px, py) = if front {
        (
            cx as f32 * CELL_W + f.off.x,
            cy as f32 * CELL_H + f.off.y - (f.size.y - CELL_H),
        )
    } else {
        (cx as f32 * CELL_W, cy as f32 * CELL_H)
    };
    (
        Sprite {
            image: world.pages[f.page.min(world.pages.len() - 1)].clone(),
            rect: Some(f.rect),
            anchor: Anchor::TopLeft,
            ..default()
        },
        // Bevy y 轴向上, 世界像素 y 取负
        Transform::from_xyz(px, -py, z),
    )
}

// ─────────── 图集脏页上传 ───────────

fn upload_dirty_pages(mut world: ResMut<World>, mut images: ResMut<Assets<Image>>) {
    // 页纹理不足时补建
    while world.pages.len() < world.atlas.pages.len() {
        let h = images.add(blank_page());
        world.pages.push(h);
    }
    let world = &mut *world;
    for (i, page) in world.atlas.pages.iter_mut().enumerate() {
        if !page.dirty {
            continue;
        }
        if let Some(img) = images.get_mut(&world.pages[i]) {
            img.data.copy_from_slice(&page.rgba);
            page.dirty = false;
        }
    }
}
