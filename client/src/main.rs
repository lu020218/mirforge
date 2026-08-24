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

mod net;

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
use protocol::{CharacterClass, CharacterSummary, ClientMessage, ServerMessage, PROTOCOL_VERSION};
use sim::{dir8_from, WalkGrid, BODY_RADIUS};

const CELL_W: f32 = 48.0;
const CELL_H: f32 = 32.0;
const CHUNK: i32 = 16; // 格/块
const VIEW_MARGIN: i32 = 1; // 视口外多保留的块圈数

/// 客户端流程状态：离线直接进游戏；联机走 登录 → 选角 → 游戏
#[derive(States, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
enum Screen {
    #[default]
    Boot,
    Login,
    CharSelect,
    InGame,
}

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
        .init_state::<Screen>()
        .init_resource::<Net>()
        .init_resource::<Remotes>()
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (
                egui_cjk_font,
                net_pump,
                net_reconnect,
                dev_autologin,
                login_ui.run_if(in_state(Screen::Login)),
                charselect_ui.run_if(in_state(Screen::CharSelect)),
                inventory_ui.run_if(in_state(Screen::InGame)),
                quest_ui.run_if(in_state(Screen::InGame)),
            )
                .chain(),
        )
        .add_systems(
            Update,
            (
                player_move,
                camera_follow,
                camera_control,
                stream_chunks,
                cast_skills,
                player_sprite,
                remote_step,
                float_damage,
                fx_step,
                upload_dirty_pages,
                net_send,
                debug_panel,
            )
                .chain(),
        )
        .run();
}

// ─────────── 联机状态 ───────────

/// 联机会话（url 为 None = 离线单机）
#[derive(Resource, Default)]
struct Net {
    url: Option<String>,
    client: Option<net::NetClient>,
    connected: bool,
    /// 断线后到点重连（Time::elapsed_secs_f64）
    reconnect_at: Option<f64>,
    token: Option<String>,
    character_id: Option<String>,
    /// 服务器分配的本角色实体 id（区分广播里的自己）
    my_id: Option<String>,
    characters: Vec<CharacterSummary>,
    skills: Vec<protocol::SkillInfo>,
    inventory: Vec<protocol::ItemInfo>,
    equipment: HashMap<String, protocol::ItemInfo>,
    quests: Vec<protocol::QuestInfo>,
    status: String,
    /// HUD 状态行 (PlayerStatus 驱动, F3 面板显示)
    hud: String,
    /// 未上报的本地位移累计（20Hz 打包发送）
    acc: DVec2,
    last_send: f64,
}

impl Net {
    fn send(&self, msg: ClientMessage) {
        if let Some(c) = &self.client {
            let _ = c.tx.send(msg);
        }
    }
}

/// 其他实体：玩家或怪物（服务器广播驱动，插值行走）
#[derive(Default)]
struct Remote {
    entity: Option<Entity>,
    /// Some(n) = 怪物, 用 Data/Monster/{n:03}.Lib; None = 玩家 (CArmour)
    image: Option<u16>,
    pos: DVec2,
    target: DVec2,
    /// 0=站 1=走 2=跑 3=攻击
    anim: u8,
    dir: usize,
    anim_t: f64,
    last_seen: f64,
}

#[derive(Resource, Default)]
struct Remotes(HashMap<String, Remote>);

// ─────────── 资源 ───────────

/// 帧标识：图层类别 + 库号 + 帧号
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Layer {
    Back,
    Mid,
    Front,
    /// 角色 (CArmour)
    Hum,
    /// 怪物 (Data/Monster/{n:03}.Lib)
    Mon(u16),
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
    /// 当前地图文件名 (小写; 与服务器 zone_id 对应)
    map_name: String,
    /// 地图名(小写) → 文件路径, 切区时按名加载
    maps: HashMap<String, PathBuf>,
    lib_dir: PathBuf,
    /// 怪物图库目录 (Data/Monster)
    mon_dir: PathBuf,
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
            Layer::Hum | Layer::Mon(_) => return None, // 专用库, 不走地图图库目录

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
            let img = match layer {
                Layer::Hum => self.hum.as_ref()?.image(idx as usize).ok().flatten()?,
                Layer::Mon(n) => {
                    let name = format!("mon{n:03}");
                    if !self.libs.contains_key(&name) {
                        let lib = std::fs::read(self.mon_dir.join(format!("{n:03}.Lib")))
                            .ok()
                            .and_then(|d| CrystalLib::parse(d).ok());
                        self.libs.insert(name.clone(), lib);
                    }
                    self.libs
                        .get(&name)
                        .and_then(|l| l.as_ref())?
                        .image(idx as usize)
                        .ok()
                        .flatten()?
                }
                _ => {
                    let name = Self::lib_name(layer, front_lib)?;
                    let lib = self.open_lib(&name)?;
                    lib.image(idx as usize).ok().flatten()?
                }
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

fn setup(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut net: ResMut<Net>,
    mut next: ResMut<NextState<Screen>>,
) {
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
    // 联机: MIRFORGE_SERVER=ws://host:port 时走 登录→选角→进图; 未设则离线单机
    if let Ok(url) = std::env::var("MIRFORGE_SERVER") {
        info!("联机模式: {url}");
        net.client = Some(net::connect(url.clone()));
        net.url = Some(url);
        net.status = "连接中...".into();
        next.set(Screen::Login);
    } else {
        commands.spawn((
            Player {
                pos: DVec2::new(sx as f64 + 0.5, sy as f64 + 0.5),
                dir: 4,
                moving: false,
                running: false,
                anim_t: 0.0,
                attack_start: None,
            },
            Sprite::default(),
            Transform::default(),
            Visibility::default(),
        ));
        info!("离线模式, 玩家已生成 @({sx},{sy})");
        next.set(Screen::InGame);
    }
    // 预建 8 页图集纹理 (不够时 upload 系统按需补)
    let mut pages = Vec::new();
    for _ in 0..8 {
        pages.push(images.add(blank_page()));
    }
    let maps: HashMap<String, PathBuf> = idx
        .maps
        .iter()
        .filter_map(|m| {
            m.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| (n.to_lowercase(), m.path.clone()))
        })
        .collect();
    // 怪物图库目录: lib_dir = <root>/Data/Map/<套名> → <root>/Data/Monster
    let mon_dir = lib_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("Monster"))
        .unwrap_or_else(|| lib_dir.join("Monster"));
    commands.insert_resource(World {
        map,
        map_name: map_name.to_lowercase(),
        maps,
        lib_dir,
        mon_dir,
        libs: HashMap::new(),
        atlas: AtlasCpu::default(),
        pages,
        frames: HashMap::new(),
        chunks: HashMap::new(),
        hum,
        walk,
    });
}

impl World {
    /// 切区: 按地图名重载地图与行走网格; 旧分块由调用方回收。
    /// 图集与帧缓存保留 (同一套图库, 跨图复用)。
    fn switch_map(&mut self, zone_id: &str) -> bool {
        let key = zone_id.to_lowercase();
        if key == self.map_name {
            return true;
        }
        let Some(path) = self.maps.get(&key) else {
            error!("切区失败: 本地找不到地图 {zone_id}");
            return false;
        };
        let Ok(bytes) = std::fs::read(path) else {
            error!("切区失败: 读地图失败 {path:?}");
            return false;
        };
        let Ok(map) = mir_formats::map::parse(&bytes) else {
            error!("切区失败: 解析地图失败 {path:?}");
            return false;
        };
        let cells = map.cells.clone();
        let (mw, mh) = (map.width, map.height);
        self.walk = WalkGrid::from_cells(mw, mh, |x, y| cells[(y * mw + x) as usize].blocked);
        info!(
            "切区 → {zone_id} ({:?} {}x{})",
            map.kind, map.width, map.height
        );
        self.map = map;
        self.map_name = key;
        true
    }
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
    /// 普攻动作开始时刻 (Time::elapsed_secs_f64; 动作期间站桩)
    attack_start: Option<f64>,
}

/// 普攻动作时长 (6 帧 × 90ms) 与客户端侧冷却
const ATTACK_ANIM_SECS: f64 = 0.54;
const ATTACK_CD_SECS: f64 = 0.6;
/// 攻击射程 (格)
const ATTACK_RANGE: f64 = 2.4;

/// 伤害飘字
#[derive(Component)]
struct Floater {
    born: f64,
}

// 速度对齐原版节奏: 走路一步(1 格)≈0.6s, 跑步一个周期跨 2 格
const WALK_SPEED: f64 = 1.7; // 格/秒
const RUN_SPEED: f64 = 3.4;
// 动画与位移锁定: 走/跑一个 6 帧周期恰好覆盖 1/2 格, 脚步不打滑(否则视觉发飘)
const WALK_FRAME_DT: f64 = 1.0 / (WALK_SPEED * 6.0);
const RUN_FRAME_DT: f64 = 2.0 / (RUN_SPEED * 6.0);
/// 光标离角色近于此距离(格)时不再追(防原地抖动)
const CURSOR_DEADZONE: f64 = 0.4;

/// 经典传奇操作: 左键点怪=普攻, 左键按住空地=走路, 右键按住=跑步
#[allow(clippy::too_many_arguments)]
fn player_move(
    time: Res<Time>,
    buttons: Res<ButtonInput<MouseButton>>,
    mut ev_cursor: EventReader<CursorMoved>,
    q_cam: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
    world: Res<World>,
    mut net: ResMut<Net>,
    remotes: Res<Remotes>,
    mut egui_ctx: EguiContexts,
    mut last_cursor: Local<Option<Vec2>>,
    mut last_atk: Local<f64>,
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
    // egui 面板占用指针时不当作行走输入
    if egui_ctx.ctx_mut().wants_pointer_input() {
        return;
    }
    let dt = time.delta_secs_f64();
    let now_t = time.elapsed_secs_f64();
    // 攻击动作期间站桩
    if p.attack_start.is_some_and(|t| now_t - t < ATTACK_ANIM_SECS) {
        p.moving = false;
        p.anim_t += dt;
        return;
    }
    // 光标世界格坐标 (Bevy y 向上, 世界 y 向下取负还原); 每帧换算,
    // 镜头滚动时朝向随光标屏幕位置更新
    let cursor_cell = last_cursor
        .and_then(|c| {
            let (cam, cam_tf) = q_cam.get_single().ok()?;
            cam.viewport_to_world_2d(cam_tf, c).ok()
        })
        .map(|w| DVec2::new(w.x as f64 / CELL_W as f64, -(w.y as f64) / CELL_H as f64));
    let run = buttons.pressed(MouseButton::Right);
    let held = buttons.pressed(MouseButton::Left) || run;
    // 左键按在怪身上 = 普攻 (光标 bbox 近似命中, 死亡中的怪忽略)
    if buttons.pressed(MouseButton::Left) && !run {
        if let Some(cc) = cursor_cell {
            let hit = remotes
                .0
                .iter()
                .filter(|(_, r)| r.image.is_some() && r.anim != 4)
                .find(|(_, r)| (r.pos.x - cc.x).abs() < 0.8 && (r.pos.y - cc.y).abs() < 1.1);
            if let Some((mid, m)) = hit {
                let d = m.pos - p.pos;
                p.dir = dir8_from(d.x, d.y);
                p.moving = false;
                if d.length() <= ATTACK_RANGE && now_t - *last_atk > ATTACK_CD_SECS {
                    *last_atk = now_t;
                    p.attack_start = Some(now_t);
                    p.anim_t = 0.0;
                    net.send(ClientMessage::Attack {
                        target_id: mid.clone(),
                        skill_id: "basic".into(),
                    });
                } else {
                    p.anim_t += dt;
                }
                return;
            }
        }
    }
    let mut v = DVec2::ZERO;
    if held {
        if let Some(cc) = cursor_cell {
            let d = cc - p.pos;
            if d.length() > CURSOR_DEADZONE {
                // 方向吸附 8 向: 身体走向与精灵朝向结构上一致 (阶梯路径, 经典手感)
                let dir = dir8_from(d.x, d.y);
                v = DVec2::new(sim::DIR8[dir].0, sim::DIR8[dir].1);
            }
        }
    }
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
        // 预测即时生效, 实际位移累计入网络上报队列
        net.acc += DVec2::new(nx - p.pos.x, ny - p.pos.y);
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
    net: Res<Net>,
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
            if !net.hud.is_empty() {
                ui.label(net.hud.clone());
            }
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
fn player_sprite(
    time: Res<Time>,
    mut world: ResMut<World>,
    mut q: Query<(&Player, &mut Sprite, &mut Transform)>,
) {
    let Ok((p, mut sprite, mut tf)) = q.get_single_mut() else {
        return;
    };
    let now_t = time.elapsed_secs_f64();
    // 帧表 (男, CArmour 实证布局): 站 0+dir*4 (4帧); 走 32+dir*6; 跑 80+dir*6; 攻 128+dir*6 (一次性)
    let frame_idx = if let Some(t) = p.attack_start.filter(|t| now_t - t < ATTACK_ANIM_SECS) {
        128 + p.dir * 6 + (((now_t - t) / 0.09) as usize).min(5)
    } else if p.moving && p.running {
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

// ─────────── 联机系统 ───────────

/// egui 默认字体无中文; 从系统字体目录加载 (仅运行时读取, 不入库)
fn egui_cjk_font(mut ctx: EguiContexts, mut done: Local<bool>) {
    if *done {
        return;
    }
    *done = true;
    for cand in [
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simhei.ttf",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    ] {
        if let Ok(bytes) = std::fs::read(cand) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("cjk".into(), egui::FontData::from_owned(bytes));
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".into());
            }
            ctx.ctx_mut().set_fonts(fonts);
            return;
        }
    }
    warn!("未找到系统中文字体, UI 中文将显示为方块");
}

/// 处理服务器消息 (联机核心状态机)
#[allow(clippy::too_many_arguments)]
fn net_pump(
    mut commands: Commands,
    time: Res<Time>,
    mut net: ResMut<Net>,
    mut world: ResMut<World>,
    mut remotes: ResMut<Remotes>,
    mut next: ResMut<NextState<Screen>>,
    screen: Res<State<Screen>>,
    mut q_player: Query<&mut Player>,
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
                    } else {
                        net.status = "请登录".into();
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
                ServerMessage::ZoneChanged {
                    zone_id, position, ..
                } => {
                    // 跨地图: 重载地图/行走网格, 回收旧分块与远程玩家
                    if zone_id.to_lowercase() != world.map_name && world.switch_map(&zone_id) {
                        for (_, e) in world.chunks.drain() {
                            commands.entity(e).despawn_recursive();
                        }
                        for (_, mut r) in remotes.0.drain() {
                            if let Some(ent) = r.entity.take() {
                                commands.entity(ent).despawn();
                            }
                        }
                    }
                    let pos = DVec2::new(position.x, position.y);
                    if let Ok(mut p) = q_player.get_single_mut() {
                        p.pos = pos;
                    } else {
                        commands.spawn((
                            Player {
                                pos,
                                dir: 4,
                                moving: false,
                                running: false,
                                anim_t: 0.0,
                                attack_start: None,
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
                ServerMessage::SkillList { skills } => net.skills = skills,
                ServerMessage::QuestState { quests } => net.quests = quests,
                ServerMessage::InventoryState {
                    inventory,
                    equipment,
                } => {
                    net.inventory = inventory;
                    net.equipment = equipment;
                }
                ServerMessage::PlayerStatus {
                    level,
                    experience,
                    required_experience,
                    hp,
                    max_hp,
                    ..
                } => {
                    net.hud = format!(
                        "Lv{level}  exp {experience}/{required_experience}  hp {hp}/{max_hp}"
                    );
                }
                ServerMessage::Notification { message, .. } => {
                    info!("通知: {message}");
                }
                ServerMessage::SkillEffect {
                    skill_id,
                    position,
                    targets,
                    ..
                } => {
                    let color = skill_color(&skill_id);
                    let mut points = vec![DVec2::new(position.x, position.y)];
                    for t in &targets {
                        if let Some(r) = remotes.0.get(t) {
                            points.push(r.pos);
                        }
                    }
                    for pt in points {
                        let px = pt.x as f32 * CELL_W - CELL_W / 2.0;
                        let py = pt.y as f32 * CELL_H - CELL_H / 2.0;
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
                ServerMessage::DamageNumber {
                    target_id, amount, ..
                } => {
                    let mine = net.my_id.as_deref() == Some(target_id.as_str());
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
                            }
                            continue;
                        }
                        let r = remotes.0.entry(e.id.clone()).or_default();
                        r.last_seen = now;
                        // id 形如 mon_{image}_{template}_{n} → 怪物
                        if r.image.is_none() && e.id.starts_with("mon_") {
                            r.image = e.id.split('_').nth(1).and_then(|s| s.parse().ok());
                        }
                        if let Some(pos) = e.position {
                            let t = DVec2::new(pos.x, pos.y);
                            if r.entity.is_none() {
                                r.pos = t; // 首见直接落位
                            }
                            r.target = t;
                        }
                        let anim = match e.animation.as_deref() {
                            Some("run") => 2,
                            Some("walk") => 1,
                            Some("attack") => 3,
                            Some("die") => 4,
                            _ => 0,
                        };
                        if anim != r.anim {
                            r.anim_t = 0.0; // 动作切换从头播 (攻击/死亡一次性动画)
                        }
                        r.anim = anim;
                        if let Some(d) = e.dir {
                            r.dir = (d as usize) % 8;
                        }
                    }
                }
                _ => {}
            },
        }
    }
}

/// 断线到点重连
fn net_reconnect(time: Res<Time>, mut net: ResMut<Net>) {
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
fn net_send(time: Res<Time>, mut net: ResMut<Net>) {
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

/// 其他玩家插值行走 + 精灵帧 (与本地玩家同一套 CArmour 帧表)
fn remote_step(
    mut commands: Commands,
    time: Res<Time>,
    mut world: ResMut<World>,
    mut remotes: ResMut<Remotes>,
) {
    let dt = time.delta_secs_f64();
    let now = time.elapsed_secs_f64();
    let mut gone = Vec::new();
    for (id, r) in remotes.0.iter_mut() {
        // 3s 未出现在广播里视为离场
        if now - r.last_seen > 3.0 {
            if let Some(ent) = r.entity.take() {
                commands.entity(ent).despawn();
            }
            gone.push(id.clone());
            continue;
        }
        // 朝目标插值: 速度按动画档, 距离偏大时加速收敛防积压
        let d = r.target - r.pos;
        let dist = d.length();
        let speed = match r.anim {
            2 => RUN_SPEED,
            1 => WALK_SPEED * if r.image.is_some() { 1.6 } else { 1.0 },
            _ => 0.0,
        };
        let step = (speed * dt).max(dist * 4.0 * dt);
        let moving = dist > 0.02 && r.anim < 3;
        if moving {
            r.dir = dir8_from(d.x, d.y);
            r.pos += if dist <= step { d } else { d / dist * step };
        } else if r.anim < 3 {
            r.pos = r.target;
        }
        r.anim_t += dt;
        // 帧表: 玩家=CArmour (站/走/跑), 怪物=Mon 库 (站/走/攻/死)
        let (layer, frame_idx) = if let Some(n) = r.image {
            let idx = match r.anim {
                4 => 144 + r.dir * 10 + (((r.anim_t / 0.13) as usize).min(9)), // 死亡一次性, 停在末帧
                3 => 80 + r.dir * 6 + ((r.anim_t / 0.15) as usize % 6),
                _ if moving => 32 + r.dir * 6 + ((r.anim_t / 0.12) as usize % 6),
                _ => r.dir * 4 + ((r.anim_t / 0.25) as usize % 4),
            };
            (Layer::Mon(n), idx)
        } else {
            let idx = if moving && r.anim == 2 {
                80 + r.dir * 6 + ((r.anim_t / RUN_FRAME_DT) as usize % 6)
            } else if moving {
                32 + r.dir * 6 + ((r.anim_t / WALK_FRAME_DT) as usize % 6)
            } else {
                r.dir * 4 + ((r.anim_t / 0.2) as usize % 4)
            };
            (Layer::Hum, idx)
        };
        let Some(f) = world.frame(layer, 0, frame_idx as i32) else {
            continue;
        };
        let px = r.pos.x as f32 * CELL_W - CELL_W / 2.0 + f.off.x;
        let py = r.pos.y as f32 * CELL_H - CELL_H / 2.0 + f.off.y;
        let z = 10.0 + r.pos.y as f32 * 0.01 + 0.004; // 略低于本地玩家
        let sprite = Sprite {
            image: world.pages[f.page.min(world.pages.len() - 1)].clone(),
            rect: Some(f.rect),
            anchor: Anchor::TopLeft,
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
    }
    for id in gone {
        remotes.0.remove(&id);
    }
}

/// 开发钩子: MIRFORGE_AUTOLOGIN=user:pass 自动 注册→(已存在则登录)→建角→选角
/// (联调/自动化测试用; 角色名 = 用户名)
fn dev_autologin(
    time: Res<Time>,
    mut net: ResMut<Net>,
    screen: Res<State<Screen>>,
    mut stage: Local<u8>,
    mut wait_since: Local<f64>,
) {
    let Ok(cred) = std::env::var("MIRFORGE_AUTOLOGIN") else {
        return;
    };
    let Some((user, pass)) = cred.split_once(':') else {
        return;
    };
    match screen.get() {
        Screen::Login if net.connected => {
            if *stage == 0 {
                *stage = 1;
                net.send(ClientMessage::Register {
                    username: user.into(),
                    password: pass.into(),
                });
            } else if *stage == 1 && net.status == "用户名已存在" {
                *stage = 2;
                net.send(ClientMessage::Login {
                    username: user.into(),
                    password: pass.into(),
                });
            }
        }
        Screen::CharSelect => {
            if *wait_since == 0.0 {
                *wait_since = time.elapsed_secs_f64();
            }
            if *stage >= 5 {
                return;
            }
            if let Some(c) = net.characters.iter().find(|c| c.name == user) {
                *stage = 5;
                let id = c.id.clone();
                net.character_id = Some(id.clone());
                net.send(ClientMessage::SelectCharacter { character_id: id });
            } else if *stage < 4 && time.elapsed_secs_f64() - *wait_since > 1.0 {
                *stage = 4;
                net.send(ClientMessage::CreateCharacter {
                    name: user.into(),
                    class: CharacterClass::Warrior,
                    gender: "male".into(),
                });
            }
        }
        _ => {}
    }
}

/// 技能特效闪光 (过渡版: 彩色扩散圈; M4 接原版 Magic 特效图库)
#[derive(Component)]
struct Fx {
    born: f64,
}

fn skill_color(id: &str) -> Color {
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
fn cast_skills(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    net: Res<Net>,
    remotes: Res<Remotes>,
    mut cds: Local<HashMap<String, f64>>,
    mut q: Query<&mut Player>,
) {
    let Ok(mut p) = q.get_single_mut() else {
        return;
    };
    let idx = if keys.just_pressed(KeyCode::Digit1) {
        0
    } else if keys.just_pressed(KeyCode::Digit2) {
        1
    } else if keys.just_pressed(KeyCode::Digit3) {
        2
    } else {
        return;
    };
    let Some(s) = net.skills.get(idx).cloned() else {
        return;
    };
    let now = time.elapsed_secs_f64();
    if cds.get(&s.id).is_some_and(|&t| now < t) {
        return;
    }
    let target = if s.self_cast {
        None
    } else {
        // 射程内最近的活怪
        let found = remotes
            .0
            .iter()
            .filter(|(_, r)| r.image.is_some() && r.anim != 4)
            .map(|(id, r)| (id.clone(), (r.pos - p.pos).length(), r.pos))
            .filter(|(_, d, _)| *d <= s.range)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let Some(t) = found else {
            return; // 无目标不施放
        };
        Some(t)
    };
    if let Some((_, _, mp)) = &target {
        p.dir = dir8_from(mp.x - p.pos.x, mp.y - p.pos.y);
    }
    cds.insert(s.id.clone(), now + s.cooldown_ms as f64 / 1000.0);
    p.attack_start = Some(now);
    p.anim_t = 0.0;
    net.send(ClientMessage::UseSkill {
        skill_id: s.id,
        target_id: target.map(|(id, _, _)| id),
        position: None,
    });
}

/// 特效步进: 扩散 + 渐隐
fn fx_step(
    mut commands: Commands,
    time: Res<Time>,
    mut q: Query<(Entity, &mut Transform, &mut Sprite, &Fx)>,
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
}

/// 伤害飘字上浮渐隐
fn float_damage(
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

/// 过渡版背包(B)/装备(C)面板
fn inventory_ui(
    mut ctx: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    net: Res<Net>,
    mut show_bag: Local<bool>,
    mut show_equip: Local<bool>,
) {
    if keys.just_pressed(KeyCode::KeyB) {
        *show_bag = !*show_bag;
    }
    if keys.just_pressed(KeyCode::KeyC) {
        *show_equip = !*show_equip;
    }
    let fmt_stats = |i: &protocol::ItemInfo| {
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
    };
    if *show_bag {
        let items = net.inventory.clone();
        egui::Window::new("背包 (B)")
            .default_pos((40.0, 300.0))
            .show(ctx.ctx_mut(), |ui| {
                if items.is_empty() {
                    ui.label("空空如也");
                }
                for item in &items {
                    ui.horizontal(|ui| {
                        ui.label(format!("{}  {}", item.name, fmt_stats(item)));
                        if ui.small_button("装备").clicked() {
                            net.send(ClientMessage::Equip {
                                item_id: item.id.clone(),
                                slot: item.slot.clone(),
                            });
                        }
                    });
                }
            });
    }
    if *show_equip {
        let equipment = net.equipment.clone();
        egui::Window::new("装备 (C)")
            .default_pos((40.0, 520.0))
            .show(ctx.ctx_mut(), |ui| {
                const SLOTS: [(&str, &str); 5] = [
                    ("weapon", "武器"),
                    ("armor", "衣服"),
                    ("helmet", "头盔"),
                    ("necklace", "项链"),
                    ("ring", "戒指"),
                ];
                for (slot, label) in SLOTS {
                    ui.horizontal(|ui| {
                        match equipment.get(slot) {
                            Some(item) => {
                                ui.label(format!("{label}: {}  {}", item.name, fmt_stats(item)));
                                if ui.small_button("卸下").clicked() {
                                    net.send(ClientMessage::Unequip {
                                        slot: slot.to_string(),
                                    });
                                }
                            }
                            None => {
                                ui.label(format!("{label}: -"));
                            }
                        };
                    });
                }
            });
    }
}

/// 过渡版任务面板 (L)
fn quest_ui(
    mut ctx: EguiContexts,
    keys: Res<ButtonInput<KeyCode>>,
    net: Res<Net>,
    mut show: Local<bool>,
) {
    if keys.just_pressed(KeyCode::KeyL) {
        *show = !*show;
    }
    if !*show {
        return;
    }
    fn mob_name(t: &str) -> &str {
        match t {
            "chicken" => "鸡",
            "deer" => "鹿",
            "scarecrow" => "稻草人",
            other => other,
        }
    }
    let quests = net.quests.clone();
    egui::Window::new("任务 (L)")
        .default_pos((1500.0, 60.0))
        .show(ctx.ctx_mut(), |ui| {
            if quests.is_empty() {
                ui.label("暂无可接任务");
            }
            for q in &quests {
                let tag = match q.state.as_str() {
                    "active" => "[进行中]",
                    "completed" => "[已完成]",
                    _ => "[可接取]",
                };
                ui.horizontal(|ui| {
                    ui.label(format!("{tag} {}  (经验 {})", q.name, q.exp_reward));
                    match q.state.as_str() {
                        "available" if ui.small_button("接取").clicked() => {
                            net.send(ClientMessage::AcceptQuest {
                                quest_id: q.id.clone(),
                            });
                        }
                        "active" => {
                            let done = q.objectives.iter().all(|o| o.current >= o.required);
                            if done && ui.small_button("交付").clicked() {
                                net.send(ClientMessage::CompleteQuest {
                                    quest_id: q.id.clone(),
                                });
                            }
                            if ui.small_button("放弃").clicked() {
                                net.send(ClientMessage::AbandonQuest {
                                    quest_id: q.id.clone(),
                                });
                            }
                        }
                        _ => {}
                    }
                });
                if q.state == "active" {
                    for o in &q.objectives {
                        ui.label(format!(
                            "    击杀{}: {}/{}",
                            mob_name(&o.target_id),
                            o.current,
                            o.required
                        ));
                    }
                }
            }
        });
}

// ─────────── 过渡版登录/选角 UI (egui 素排版, M4 换 Bevy UI 皮肤) ───────────

fn login_ui(
    mut ctx: EguiContexts,
    mut net: ResMut<Net>,
    mut user: Local<String>,
    mut pass: Local<String>,
) {
    egui::Window::new("MirForge 登录")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, (0.0, 0.0))
        .show(ctx.ctx_mut(), |ui| {
            ui.set_width(260.0);
            ui.horizontal(|ui| {
                ui.label("账号");
                ui.text_edit_singleline(&mut *user);
            });
            ui.horizontal(|ui| {
                ui.label("密码");
                ui.add(egui::TextEdit::singleline(&mut *pass).password(true));
            });
            ui.add_space(8.0);
            let ready = net.connected && !user.is_empty() && !pass.is_empty();
            ui.horizontal(|ui| {
                if ui.add_enabled(ready, egui::Button::new("登录")).clicked() {
                    net.send(ClientMessage::Login {
                        username: user.clone(),
                        password: pass.clone(),
                    });
                    net.status = "登录中...".into();
                }
                if ui.add_enabled(ready, egui::Button::new("注册")).clicked() {
                    net.send(ClientMessage::Register {
                        username: user.clone(),
                        password: pass.clone(),
                    });
                    net.status = "注册中...".into();
                }
            });
            if !net.status.is_empty() {
                ui.add_space(4.0);
                ui.label(net.status.clone());
            }
        });
}

fn charselect_ui(
    mut ctx: EguiContexts,
    mut net: ResMut<Net>,
    mut name: Local<String>,
    mut class_idx: Local<usize>,
) {
    const CLASSES: [(&str, CharacterClass); 3] = [
        ("战士", CharacterClass::Warrior),
        ("法师", CharacterClass::Mage),
        ("道士", CharacterClass::Taoist),
    ];
    egui::Window::new("选择角色")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, (0.0, 0.0))
        .show(ctx.ctx_mut(), |ui| {
            ui.set_width(300.0);
            let characters = net.characters.clone();
            if characters.is_empty() {
                ui.label("暂无角色, 先创建一个:");
            }
            for c in &characters {
                let cls = CLASSES
                    .iter()
                    .find(|(_, k)| *k == c.class)
                    .map(|(n, _)| *n)
                    .unwrap_or("?");
                if ui
                    .button(format!("{}  Lv.{}  {cls}", c.name, c.level))
                    .clicked()
                {
                    net.character_id = Some(c.id.clone());
                    net.send(ClientMessage::SelectCharacter {
                        character_id: c.id.clone(),
                    });
                    net.status = "进入游戏...".into();
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("角色名");
                ui.text_edit_singleline(&mut *name);
            });
            ui.horizontal(|ui| {
                for (i, (label, _)) in CLASSES.iter().enumerate() {
                    ui.selectable_value(&mut *class_idx, i, *label);
                }
            });
            if ui
                .add_enabled(!name.is_empty(), egui::Button::new("创建角色"))
                .clicked()
            {
                net.send(ClientMessage::CreateCharacter {
                    name: name.clone(),
                    class: CLASSES[*class_idx].1,
                    gender: "male".into(),
                });
                name.clear();
            }
            if !net.status.is_empty() {
                ui.add_space(4.0);
                ui.label(net.status.clone());
            }
        });
}
