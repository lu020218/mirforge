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

mod hud;
mod net;
mod panels;
mod screens;

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
                .set(AssetPlugin {
                    // Bevy 的相对 file_path 基于 exe 目录 (或 CARGO_MANIFEST_DIR),
                    // 从仓库根直接跑 exe 时找不到 client/assets —— 用 cwd 绝对化
                    file_path: std::env::current_dir()
                        .ok()
                        .map(|d| d.join("client/assets"))
                        .filter(|p| p.is_dir())
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "assets".into()),
                    ..default()
                })
                .set(ImagePlugin::default_nearest())
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "MirForge".into(),
                        present_mode: PresentMode::AutoVsync,
                        // 覆盖系统 DPI: adapt_scale 按窗口高动态设缩放系数,
                        // UI 恒以 1080 逻辑高适配 (设计稿 1:1 基准)
                        resolution: bevy::window::WindowResolution::new(1600.0, 900.0)
                            .with_scale_factor_override(1.0),
                        position: bevy::window::WindowPosition::Centered(
                            bevy::window::MonitorSelection::Primary,
                        ),
                        resize_constraints: bevy::window::WindowResizeConstraints {
                            min_width: 800.0,
                            min_height: 600.0,
                            ..default()
                        },
                        ..default()
                    }),
                    ..default()
                }),
        )
        .add_plugins((EguiPlugin, FrameTimeDiagnosticsPlugin))
        .init_state::<Screen>()
        .init_resource::<Net>()
        .init_resource::<Remotes>()
        .add_systems(Startup, (hud::load_skin, setup))
        .add_systems(
            OnEnter(Screen::InGame),
            (make_portrait, hud::setup, panels::setup),
        )
        .add_systems(OnExit(Screen::InGame), (hud::teardown, panels::teardown))
        .add_systems(OnEnter(Screen::Login), screens::login_setup)
        .add_systems(OnExit(Screen::Login), screens::login_teardown)
        .add_systems(OnEnter(Screen::CharSelect), screens::charselect_setup)
        .add_systems(OnExit(Screen::CharSelect), screens::charselect_teardown)
        .init_resource::<panels::Drag>()
        .init_resource::<screens::CharSelectState>()
        .init_resource::<hud::ChatState>()
        .init_resource::<MiniMap>()
        .init_resource::<ItemIcons>()
        .init_resource::<GroundEntities>()
        .init_resource::<Zoom>()
        .add_systems(
            Update,
            (
                screens::adapt_scale,
                egui_cjk_font,
                net_pump,
                net_reconnect,
                dev_autologin,
                (screens::text_input, screens::login_update)
                    .chain()
                    .run_if(in_state(Screen::Login)),
                (
                    screens::text_input,
                    screens::charselect_update,
                    screens::create_panel_update,
                )
                    .chain()
                    .run_if(in_state(Screen::CharSelect)),
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
                animate_tiles,
                load_minimap,
                ground_render,
                cast_skills,
                player_sprite,
                remote_step,
                float_damage,
                fx_step,
                upload_dirty_pages,
                net_send,
                hud::update.run_if(in_state(Screen::InGame)),
                (
                    hud::chat_input,
                    panels::toggle,
                    hud::menu_clicks,
                    panels::drag,
                    panels::close,
                    panels::clicks,
                    panels::drop_clicks,
                    panels::refresh,
                )
                    .run_if(in_state(Screen::InGame)),
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
    /// HUD 数值 (PlayerStatus 驱动)
    stat: Option<Stat>,
    /// 通知堆栈 (msg, 类型, 出生时刻)
    notices: Vec<(String, String, f64)>,
    /// 技能冷却结束时刻 (id → elapsed_secs)
    cds: HashMap<String, f64>,
    /// 面板内容重建计数 (对应数据变化时递增)
    inv_rev: u32,
    quest_rev: u32,
    stat_rev: u32,
    notice_rev: u32,
    /// 当前区域显示名 (小地图)
    zone_name: String,
    /// 当前区域小地图帧号 (服务器下发, 管理台配置)
    zone_minimap: Option<u16>,
    /// 聊天框滚动: (标签 "系统"/玩家名, 内容)
    chatlog: Vec<(String, String)>,
    /// 地面掉落物 (服务器快照驱动)
    ground: Vec<protocol::GroundItemInfo>,
    ground_rev: u32,
    /// 未上报的本地位移累计（20Hz 打包发送）
    acc: DVec2,
    last_send: f64,
}

/// HUD 数值快照
#[derive(Clone, Copy)]
struct Stat {
    level: u32,
    exp: u64,
    req: u64,
    hp: i32,
    max_hp: i32,
    mp: i32,
    max_mp: i32,
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
    /// 血条前景/背景实体 (受伤怪才显示)
    bar: Option<(Entity, Entity)>,
    /// (当前, 最大) — 首见按满血记最大
    hp: Option<(i32, i32)>,
    /// Some(n) = 怪物, 用 Data/Monster/{n:03}.Lib; None = 玩家 (CArmour)
    image: Option<u16>,
    pos: DVec2,
    target: DVec2,
    /// 0=站 1=走 2=跑 3=攻击
    anim: u8,
    dir: usize,
    anim_t: f64,
    /// 行走相位 (累计位移格数): 脚步帧与地面锁定, 走一格恰好一轮 6 帧
    walk_phase: f64,
    /// 最近实际位移时刻 (行走动画 0.25s 去抖, 防插值追上目标后站/走高频交替)
    last_move_t: f64,
    /// 玩家外观 (CArmour 库号 / CWeapon 库号)
    armour: u16,
    weapon: Option<u16>,
    /// 武器叠层实体
    wep_entity: Option<Entity>,
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
    /// 角色衣甲 (CArmour/{n:02}.Lib)
    Hum(u16),
    /// 手持武器 (CWeapon/{n:02}.Lib)
    Weapon(u16),
    /// 怪物 (Monster/{n:03}.Lib)
    Mon(u16),
    /// 技能特效 (0=Magic.Lib, 1=Magic2.Lib)
    Fx(u8),
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
    /// 资源数据根 (Data/, 其下 Map/ Monster/ CArmour/ CWeapon/ Magic 等)
    data_root: PathBuf,
    libs: HashMap<String, Option<CrystalLib>>,
    atlas: AtlasCpu,
    pages: Vec<Handle<Image>>,
    frames: HashMap<(Layer, i16, i32, bool), Option<FrameRef>>,
    chunks: HashMap<(i32, i32), Entity>,
    walk: WalkGrid,
}

impl World {
    /// 库号 → 相对 Data/Map 的库文件名 (Crystal Libraries.MapLibs 注册表完整移植;
    /// back/mid/front 三层共用同一索引空间, 逐格取 cell.*_lib)
    fn lib_name(layer: Layer, lib: i16) -> Option<String> {
        match layer {
            Layer::Hum(n) => return Some(format!("CArmour/{n:02}")),
            Layer::Weapon(n) => return Some(format!("CWeapon/{n:02}")),
            Layer::Mon(n) => return Some(format!("Monster/{n:03}")),
            Layer::Fx(0) => return Some("Magic".into()),
            Layer::Fx(_) => return Some("Magic2".into()),
            _ => {}
        }
        const MIR3_NAMES: [&str; 14] = [
            "Tilesc",
            "Tiles30c",
            "Tiles5c",
            "Smtilesc",
            "Housesc",
            "Cliffsc",
            "Dungeonsc",
            "Innersc",
            "Furnituresc",
            "Wallsc",
            "smObjectsc",
            "Animationsc",
            "Object1c",
            "Object2c",
        ];
        const MIR3_STATE: [&str; 5] = ["", "wood", "sand", "snow", "forest"];
        let l = lib as i32;
        Some(match l {
            0 => "Map/WemadeMir2/Tiles".into(),
            1 => "Map/WemadeMir2/SmTiles".into(),
            2 => "Map/WemadeMir2/Objects".into(),
            3..=28 => format!("Map/WemadeMir2/Objects{}", l - 1),
            90 => "Map/WemadeMir2/Objects_32bit".into(),
            100 => "Map/ShandaMir2/Tiles".into(),
            101..=109 => format!("Map/ShandaMir2/Tiles{}", l - 99),
            110 => "Map/ShandaMir2/SmTiles".into(),
            111..=119 => format!("Map/ShandaMir2/SmTiles{}", l - 109),
            120 => "Map/ShandaMir2/Objects".into(),
            121..=150 => format!("Map/ShandaMir2/Objects{}", l - 119),
            190 => "Map/ShandaMir2/AniTiles1".into(),
            200..=274 => {
                let o = (l - 200) as usize;
                let (s, n) = (o / 15, o % 15);
                if s >= MIR3_STATE.len() || n >= MIR3_NAMES.len() {
                    return None;
                }
                let dir = if s == 0 {
                    String::new()
                } else {
                    format!("{}/", MIR3_STATE[s])
                };
                format!("Map/WemadeMir3/{dir}{}", MIR3_NAMES[n])
            }
            300..=374 => {
                let o = (l - 300) as usize;
                let (s, n) = (o / 15, o % 15);
                if s >= MIR3_STATE.len() || n >= MIR3_NAMES.len() {
                    return None;
                }
                format!("Map/ShandaMir3/{}{}", MIR3_NAMES[n], MIR3_STATE[s])
            }
            _ => return None,
        })
    }

    fn open_lib(&mut self, name: &str) -> Option<&CrystalLib> {
        if !self.libs.contains_key(name) {
            let mut lib = None;
            for cand in [format!("{name}.Lib"), format!("{name}.lib")] {
                let p = self.data_root.join(&cand);
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
        self.frame_ex(layer, front_lib, idx, false)
    }

    /// blend=true: 加色混合帧 (灯光/法阵光晕)。Bevy Sprite 无逐精灵混合
    /// 模式, 用标准近似: alpha=像素亮度 (黑=全透明, 等效柔性 additive)
    fn frame_ex(
        &mut self,
        layer: Layer,
        front_lib: i16,
        idx: i32,
        blend: bool,
    ) -> Option<FrameRef> {
        let key = (layer, front_lib, idx, blend);
        if let Some(cached) = self.frames.get(&key) {
            return *cached;
        }
        let fref = (|| {
            let mut img = {
                let name = Self::lib_name(layer, front_lib)?;
                let lib = self.open_lib(&name)?;
                lib.image(idx as usize).ok().flatten()?
            };
            if blend {
                for px in img.rgba.chunks_exact_mut(4) {
                    px[3] = px[3].min(px[0].max(px[1]).max(px[2]));
                }
            }
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

/// 角色面板立绘 (CArmour 朝南站立帧)
#[derive(Resource, Default)]
pub struct Portrait(pub Option<(Handle<Image>, Vec2)>);

/// 物品图标 (Items.Lib 帧 → 独立 Image, 惰性缓存)
#[derive(Resource, Default)]
pub struct ItemIcons {
    lib: Option<CrystalLib>,
    cache: HashMap<u16, Option<Handle<Image>>>,
}

impl ItemIcons {
    pub fn get(
        &mut self,
        image: u16,
        data_root: &Path,
        images: &mut Assets<Image>,
    ) -> Option<Handle<Image>> {
        if image == 0 {
            return None;
        }
        if let Some(c) = self.cache.get(&image) {
            return c.clone();
        }
        if self.lib.is_none() {
            self.lib = std::fs::read(data_root.join("Items.Lib"))
                .ok()
                .and_then(|d| CrystalLib::parse(d).ok());
        }
        let h = self
            .lib
            .as_ref()
            .and_then(|l| l.image(image as usize).ok().flatten())
            .map(|img| {
                images.add(Image::new(
                    Extent3d {
                        width: img.width as u32,
                        height: img.height as u32,
                        depth_or_array_layers: 1,
                    },
                    TextureDimension::D2,
                    img.rgba,
                    TextureFormat::Rgba8UnormSrgb,
                    RenderAssetUsages::RENDER_WORLD,
                ))
            });
        self.cache.insert(image, h.clone());
        h
    }
}

/// 当前区域小地图 (Data/mmap.Lib 帧 → 独立 Image)
#[derive(Resource, Default)]
pub struct MiniMap {
    pub image: Option<(Handle<Image>, Vec2)>,
    /// 已加载的 (地图名, 帧号)
    loaded_for: (String, Option<u16>),
}

/// 切区时按地图名重载小地图帧
fn load_minimap(
    mut mm: ResMut<MiniMap>,
    world: Res<World>,
    net: Res<Net>,
    mut images: ResMut<Assets<Image>>,
) {
    // 帧号由服务器随区域下发 (管理台配置); 离线模式无值则不显示
    let key = (world.map_name.clone(), net.zone_minimap);
    if mm.loaded_for == key {
        return;
    }
    mm.loaded_for = key;
    mm.image = net.zone_minimap.map(|f| f as usize).and_then(|idx| {
        let path = world.data_root.join("mmap.Lib");
        let lib = CrystalLib::parse(std::fs::read(path).ok()?).ok()?;
        let img = lib.image(idx).ok().flatten()?;
        let size = Vec2::new(img.width as f32, img.height as f32);
        let handle = images.add(Image::new(
            Extent3d {
                width: img.width as u32,
                height: img.height as u32,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            img.rgba,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        ));
        Some((handle, size))
    });
    if mm.image.is_none() {
        info!("地图 {} 未配置小地图帧", world.map_name);
    }
}

/// 地面物品实体池 (id → sprite 实体)
#[derive(Resource, Default)]
struct GroundEntities(HashMap<String, Entity>);

/// 地面掉落物渲染: 快照版本变化时增删实体 (Items.Lib 图标, 地板上物件下)
fn ground_render(
    mut commands: Commands,
    net: Res<Net>,
    world: Res<World>,
    mut icons: ResMut<ItemIcons>,
    mut images: ResMut<Assets<Image>>,
    mut ents: ResMut<GroundEntities>,
    mut last_rev: Local<u32>,
) {
    if *last_rev == net.ground_rev {
        return;
    }
    *last_rev = net.ground_rev;
    let alive: std::collections::HashSet<&str> = net.ground.iter().map(|g| g.id.as_str()).collect();
    ents.0.retain(|id, e| {
        if alive.contains(id.as_str()) {
            true
        } else {
            commands.entity(*e).despawn();
            false
        }
    });
    for g in &net.ground {
        if ents.0.contains_key(&g.id) {
            continue;
        }
        let Some(h) = icons.get(g.image, &world.data_root, &mut images) else {
            continue;
        };
        let px = g.x as f32 * CELL_W - CELL_W / 2.0;
        let py = g.y as f32 * CELL_H - CELL_H / 2.0;
        let e = commands
            .spawn((
                Sprite {
                    image: h,
                    ..default()
                },
                Transform::from_xyz(px, -py, 3.0),
                Visibility::default(),
            ))
            .id();
        ents.0.insert(g.id.clone(), e);
    }
}

/// 进入游戏时按性别取立绘帧 → 独立 Image (面板 ImageNode 用)
fn make_portrait(
    mut commands: Commands,
    mut world: ResMut<World>,
    mut images: ResMut<Assets<Image>>,
    net: Res<Net>,
) {
    let female = net
        .characters
        .iter()
        .find(|c| Some(&c.id) == net.character_id.as_ref())
        .map(|c| c.gender == "female")
        .unwrap_or(false);
    // 站立帧表 0+dir*4, dir4=南(面向镜头); 女装基址 +808
    let idx = if female { 808 + 16 } else { 16 };
    let portrait = world
        .open_lib("CArmour/00")
        .and_then(|l| l.image(idx).ok().flatten())
        .map(|img| {
            let size = Vec2::new(img.width as f32, img.height as f32);
            let handle = images.add(Image::new(
                bevy::render::render_resource::Extent3d {
                    width: img.width as u32,
                    height: img.height as u32,
                    depth_or_array_layers: 1,
                },
                bevy::render::render_resource::TextureDimension::D2,
                img.rgba,
                bevy::render::render_resource::TextureFormat::Rgba8UnormSrgb,
                bevy::asset::RenderAssetUsages::RENDER_WORLD,
            ));
            (handle, size)
        });
    commands.insert_resource(Portrait(portrait));
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
    // 定位到含 Tiles.Lib 的套目录 (Data/Map/<套>), 上溯两级到 Data/ 资源根
    let data_root = tiles
        .iter()
        .find(|l| {
            l.path
                .to_string_lossy()
                .to_lowercase()
                .contains(&lib_set.to_lowercase())
        })
        .or_else(|| tiles.first())
        .and_then(|l| Some(l.path.parent()?.parent()?.parent()?.to_path_buf()))
        .unwrap_or_else(|| {
            error!("资源目录中找不到 Tiles.Lib");
            std::process::exit(2);
        });
    info!(
        "地图 {map_name}: {:?} {}x{}, 资源根 {:?}",
        map.kind, map.width, map.height, data_root
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
    commands.insert_resource(World {
        map,
        map_name: map_name.to_lowercase(),
        maps,
        data_root,
        libs: HashMap::new(),
        atlas: AtlasCpu::default(),
        pages,
        frames: HashMap::new(),
        chunks: HashMap::new(),
        walk,
    });
}

impl World {
    /// 切区: 按地图名重载地图与行走网格; 旧分块由调用方回收。
    /// 图集与帧缓存保留 (同一套图库, 跨图复用)。
    /// 图集与 GPU 页句柄同步。frame() 解码可能开新图集页, 而页纹理要到
    /// upload_dirty_pages 才补建 —— 消费 FrameRef 前必须调用本方法, 否则
    /// 新页上的帧会绑定到旧页纹理 (显示别的图案: 地图碎片/精灵闪烁的根源)。
    fn ensure_pages(&mut self, images: &mut Assets<Image>) {
        while self.pages.len() < self.atlas.pages.len() {
            let h = images.add(blank_page());
            self.pages.push(h);
        }
    }

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
            // 点在地面物品上且够得着 → 拾取 (超距则照常走路靠近)
            if buttons.just_pressed(MouseButton::Left) {
                let pick = net
                    .ground
                    .iter()
                    .find(|g| (g.x - cc.x).abs() < 0.7 && (g.y - cc.y).abs() < 0.7);
                if let Some(g) = pick {
                    let d = DVec2::new(g.x, g.y) - p.pos;
                    if d.length() <= 2.0 {
                        p.dir = dir8_from(d.x, d.y);
                        net.send(ClientMessage::PickupItem {
                            drop_id: g.id.clone(),
                        });
                        return;
                    }
                }
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
            if let Some(s) = &net.stat {
                ui.label(format!(
                    "Lv{} exp {}/{} hp {}/{} mp {}/{}",
                    s.level, s.exp, s.req, s.hp, s.max_hp, s.mp, s.max_mp
                ));
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

/// 按动作/方向/时间挑帧并更新精灵与变换 (paperdoll: 衣甲换库 + 武器叠层)
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn player_sprite(
    mut commands: Commands,
    time: Res<Time>,
    mut world: ResMut<World>,
    mut images: ResMut<Assets<Image>>,
    net: Res<Net>,
    mut wep_entity: Local<Option<Entity>>,
    mut debug_shapes: Local<Option<Option<(u16, Option<u16>)>>>,
    mut q: Query<(&Player, &mut Sprite, &mut Transform)>,
    mut q_wep: Query<
        (&mut Sprite, &mut Transform, &mut Visibility),
        (With<WeaponSprite>, Without<Player>),
    >,
) {
    let Ok((p, mut sprite, mut tf)) = q.get_single_mut() else {
        return;
    };
    // 外观: MIRFORGE_SHAPES=armour[,weapon] 调试覆盖 (离线可视验证);
    // 否则由已穿装备的 shape 决定
    let dbg = *debug_shapes.get_or_insert_with(|| {
        std::env::var("MIRFORGE_SHAPES").ok().map(|v| {
            let mut it = v.split(',');
            let a = it.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
            let w = it.next().and_then(|s| s.trim().parse().ok());
            (a, w)
        })
    });
    let (armour, weapon) = dbg.unwrap_or_else(|| {
        (
            net.equipment.get("armor").map(|i| i.shape).unwrap_or(0),
            net.equipment.get("weapon").map(|i| i.shape),
        )
    });
    let now_t = time.elapsed_secs_f64();
    // 帧表 (Crystal FrameSet.Player 权威定义): 站 0+dir*4; 走 32+dir*6;
    // 跑 80+dir*6; 战斗站架 128+dir*1; 攻击 136+dir*6 (Attack1)
    let frame_idx = if let Some(t) = p.attack_start.filter(|t| now_t - t < ATTACK_ANIM_SECS) {
        136 + p.dir * 6 + (((now_t - t) / 0.09) as usize).min(5)
    } else if p.moving && p.running {
        80 + p.dir * 6 + ((p.anim_t / RUN_FRAME_DT) as usize % 6)
    } else if p.moving {
        32 + p.dir * 6 + ((p.anim_t / WALK_FRAME_DT) as usize % 6)
    } else {
        p.dir * 4 + ((p.anim_t / 0.2) as usize % 4)
    };
    let Some(f) = world.frame(Layer::Hum(armour), 0, frame_idx as i32) else {
        warn_once!("角色帧 {frame_idx} 不可用");
        return;
    };
    let wf = weapon.and_then(|s| world.frame(Layer::Weapon(s), 0, frame_idx as i32));
    world.ensure_pages(&mut images);
    sprite.image = world.pages[f.page].clone();
    sprite.rect = Some(f.rect);
    sprite.anchor = Anchor::TopLeft;
    // Mir 角色帧偏移相对所在格左上角; 位置取格坐标向下取整的格原点 + 帧内偏移 + 连续余量
    let bx = p.pos.x as f32 * CELL_W - CELL_W / 2.0;
    let by = p.pos.y as f32 * CELL_H - CELL_H / 2.0;
    // 与前景高物件同一行深度体系; +0.005 让同行时角色压在物件之上
    let z = 10.0 + p.pos.y as f32 * 0.01 + 0.005;
    tf.translation = Vec3::new(bx + f.off.x, -(by + f.off.y), z);
    // 武器叠层: 与身体同帧号同格原点, z 微高
    let wep = *wep_entity.get_or_insert_with(|| {
        commands
            .spawn((
                WeaponSprite,
                Sprite::default(),
                Transform::default(),
                Visibility::Hidden,
            ))
            .id()
    });
    // 武器前后层序 (Crystal PlayerObject.Draw): 朝左/上系 (0,5,6,7)
    // 武器画在身体后, 朝右/下系 (1,2,3,4) 画在身体前
    let wz = if matches!(p.dir, 0 | 5 | 6 | 7) {
        z - 0.0005
    } else {
        z + 0.0005
    };
    if let Ok((mut ws, mut wt, mut vis)) = q_wep.get_mut(wep) {
        match wf {
            Some(w) => {
                ws.image = world.pages[w.page].clone();
                ws.rect = Some(w.rect);
                ws.anchor = Anchor::TopLeft;
                wt.translation = Vec3::new(bx + w.off.x, -(by + w.off.y), wz);
                *vis = Visibility::Inherited;
            }
            None => *vis = Visibility::Hidden,
        }
    }
}

/// 武器叠层精灵标记 (本地玩家/远程实体共用)
#[derive(Component)]
struct WeaponSprite;

fn camera_follow(q_player: Query<&Player>, mut q_cam: Query<&mut Transform, With<Camera2d>>) {
    let (Ok(p), Ok(mut cam)) = (q_player.get_single(), q_cam.get_single_mut()) else {
        return;
    };
    cam.translation.x = (p.pos.x as f32 - 0.5) * CELL_W;
    cam.translation.y = -((p.pos.y as f32 - 0.5) * CELL_H);
}

// ─────────── 镜头 ───────────

/// 世界缩放档 = 精灵的物理像素放大倍数。恒为整数, 保证 nearest 采样
/// 下每个源像素占等宽物理像素 (非整数倍会让像素时宽时窄, 观感发虚)
#[derive(Resource)]
struct Zoom(u32);

impl Default for Zoom {
    fn default() -> Self {
        // 默认 1× = 不缩放, 源像素与物理像素 1:1 (最锐利);
        // 高分屏嫌人物小可用 PageUp/+ 提档或 F 循环
        Zoom(1)
    }
}

fn camera_control(
    keys: Res<ButtonInput<KeyCode>>,
    chat: Res<hud::ChatState>,
    mut zoom: ResMut<Zoom>,
    windows: Query<&Window>,
    mut q: Query<&mut OrthographicProjection, With<Camera2d>>,
) {
    if !chat.active {
        if keys.just_pressed(KeyCode::PageUp) || keys.just_pressed(KeyCode::Equal) {
            zoom.0 = (zoom.0 + 1).min(6);
        }
        if keys.just_pressed(KeyCode::PageDown) || keys.just_pressed(KeyCode::Minus) {
            zoom.0 = zoom.0.saturating_sub(1).max(1);
        }
        if keys.just_pressed(KeyCode::KeyF) {
            zoom.0 = match zoom.0 {
                2 => 3,
                3 => 4,
                4 => 1,
                _ => 2,
            };
        }
    }
    // 投影 scale = 窗口缩放系数 / 档位 → 物理放大恰为 zoom 整数倍
    let Ok(win) = windows.get_single() else {
        return;
    };
    let Ok(mut proj) = q.get_single_mut() else {
        return;
    };
    let target = win.resolution.scale_factor() / zoom.0 as f32;
    if (proj.scale - target).abs() > 1e-4 {
        proj.scale = target;
    }
}

// ─────────── 地图分块流送 ───────────

fn stream_chunks(
    mut commands: Commands,
    mut world: ResMut<World>,
    mut images: ResMut<Assets<Image>>,
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
            let e = spawn_chunk(&mut commands, &mut world, &mut images, kx, ky);
            world.chunks.insert((kx, ky), e);
            budget -= 1;
        }
    }
}

/// 标准地表尺寸 (Crystal DrawFloor 判据: 恰为 48×32 或 96×64 才属地板层)
fn is_floor_size(size: Vec2) -> bool {
    (size.x == CELL_W && size.y == CELL_H) || (size.x == CELL_W * 2.0 && size.y == CELL_H * 2.0)
}

fn spawn_chunk(
    commands: &mut Commands,
    world: &mut World,
    images: &mut Assets<Image>,
    kx: i32,
    ky: i32,
) -> Entity {
    // 先解码收集帧 (期间图集可能开新页), 全部到位后统一补页再构造精灵,
    // 避免帧绑定到尚未建立的页纹理
    let mut placed: Vec<(FrameRef, f32, f32, f32)> = Vec::new();
    let mut anims: Vec<AnimatedTile> = Vec::new();
    let (w, h) = (world.map.width as i32, world.map.height as i32);
    for cy in ky * CHUNK..(ky + 1) * CHUNK {
        for cx in kx * CHUNK..(kx + 1) * CHUNK {
            if cx < 0 || cy < 0 || cx >= w || cy >= h {
                continue;
            }
            let cell = *world.map.cell(cx as u32, cy as u32).unwrap();
            let (fx, fy) = (cx as f32 * CELL_W, cy as f32 * CELL_H);
            // back: 96×64 大砖只画偶数格 (覆盖 2×2)
            if cell.back >= 0 && cx % 2 == 0 && cy % 2 == 0 {
                if let Some(f) = world.frame(Layer::Back, cell.back_lib, cell.back) {
                    placed.push((f, fx, fy, 0.0));
                }
            }
            // mid: 标准尺寸入地板层; 其余锚底进对象层按行遮挡 (Crystal DrawObjects mir3 middle)
            if cell.mid >= 0 {
                if let Some(f) = world.frame(Layer::Mid, cell.mid_lib, cell.mid) {
                    if is_floor_size(f.size) {
                        placed.push((f, fx, fy, 1.0));
                    } else {
                        let z = 10.0 + cy as f32 * 0.01;
                        placed.push((f, fx, (cy + 1) as f32 * CELL_H - f.size.y, z));
                    }
                }
            }
            // front: 标准尺寸画地板层; 非标准尺寸或带动画的进对象层
            // (Crystal: floor 画标准基帧, 对象层跳过"标准且无动画", 动画帧覆盖地板)
            if cell.front >= 0 {
                let blend = cell.ani_frame & 0x80 > 0;
                if let Some(f) = world.frame_ex(Layer::Front, cell.front_lib, cell.front, blend) {
                    let frames = cell.ani_frame & 0x7F;
                    if is_floor_size(f.size) {
                        placed.push((f, fx, fy, 2.0));
                    }
                    if !is_floor_size(f.size) || frames > 0 {
                        let z = 10.0 + cy as f32 * 0.01 + 0.002;
                        if frames > 0 {
                            anims.push(AnimatedTile {
                                lib: cell.front_lib,
                                base: cell.front,
                                frames,
                                tick: cell.ani_tick,
                                blend,
                                cx,
                                cy,
                                z,
                            });
                        } else {
                            let (px, py) =
                                object_pos(&f, cx, cy, cell.front_lib, cell.front, blend);
                            placed.push((f, px, py, z));
                        }
                    }
                }
            }
        }
    }
    world.ensure_pages(images);
    let mut parent = commands.spawn((Transform::default(), Visibility::default()));
    parent.with_children(|p| {
        for (f, px, py, z) in placed {
            p.spawn(sprite_from(world, f, px, py, z));
        }
        for a in anims {
            let t = Transform::from_xyz(a.cx as f32 * CELL_W, -(a.cy as f32 * CELL_H), a.z);
            p.spawn((Sprite::default(), Visibility::Hidden, t, a));
        }
    });
    parent.id()
}

/// 对象层放置 (Crystal DrawObjects 逐条移植)。基准 = 格底边 (cy+1)*32:
/// - blend + 库 14/27/100-198: 上移 3 格并加帧偏移
/// - blend + 帧 2723-2732 (灯火光晕): 锚底并加帧偏移 (火焰对准灯柱顶)
/// - 非 blend + 库 28 且帧带偏移: 上移 1 格并加帧偏移
/// - 其余: 锚底, 不加帧偏移
fn object_pos(f: &FrameRef, cx: i32, cy: i32, lib: i16, idx: i32, blend: bool) -> (f32, f32) {
    let bx = cx as f32 * CELL_W;
    let by = (cy + 1) as f32 * CELL_H;
    if blend {
        if matches!(lib as i32, 14 | 27 | 100..=198) {
            (bx + f.off.x, by - 3.0 * CELL_H + f.off.y)
        } else if (2723..=2732).contains(&idx) {
            (bx + f.off.x, by - f.size.y + f.off.y)
        } else {
            (bx, by - f.size.y)
        }
    } else if lib as i32 == 28 && (f.off.x != 0.0 || f.off.y != 0.0) {
        (bx + f.off.x, by - CELL_H + f.off.y)
    } else {
        (bx, by - f.size.y)
    }
}

fn sprite_from(world: &World, f: FrameRef, px: f32, py: f32, z: f32) -> (Sprite, Transform) {
    (
        Sprite {
            image: world.pages[f.page].clone(),
            rect: Some(f.rect),
            anchor: Anchor::TopLeft,
            ..default()
        },
        // Bevy y 轴向上, 世界像素 y 取负
        Transform::from_xyz(px, -py, z),
    )
}

/// 动画前景格 (火把/水面/旗帜等): base + AnimationCount 循环换帧
#[derive(Component)]
struct AnimatedTile {
    lib: i16,
    base: i32,
    frames: u8,
    tick: u8,
    /// 加色混合光效 (ani_frame 0x80 位)
    blend: bool,
    cx: i32,
    cy: i32,
    z: f32,
}

/// 地图动画层: 每 100ms 递增全局帧计数 (Crystal AnimationCount 节拍),
/// index = base + (count % (a + a*tick)) / (1 + tick)
fn animate_tiles(
    time: Res<Time>,
    mut world: ResMut<World>,
    mut images: ResMut<Assets<Image>>,
    mut state: Local<(f64, u32)>,
    mut q: Query<(&AnimatedTile, &mut Sprite, &mut Transform, &mut Visibility)>,
) {
    let now = time.elapsed_secs_f64();
    if now - state.0 < 0.1 {
        return;
    }
    state.0 = now;
    state.1 = state.1.wrapping_add(1);
    let count = state.1;
    for (a, mut sp, mut tf, mut vis) in q.iter_mut() {
        let (n, k) = (a.frames as u32, a.tick as u32);
        let idx = a.base + ((count % (n + n * k)) / (1 + k)) as i32;
        if let Some(f) = world.frame_ex(Layer::Front, a.lib, idx, a.blend) {
            world.ensure_pages(&mut images);
            sp.image = world.pages[f.page].clone();
            sp.rect = Some(f.rect);
            sp.anchor = Anchor::TopLeft;
            let (px, py) = object_pos(&f, a.cx, a.cy, a.lib, idx, a.blend);
            tf.translation.x = px;
            tf.translation.y = -py;
            *vis = Visibility::Inherited;
        }
    }
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
                    zone_id,
                    zone_name,
                    position,
                    minimap,
                } => {
                    net.zone_name = zone_name;
                    net.zone_minimap = minimap;
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
                    let fx = skill_fx(&skill_id);
                    for pt in points {
                        let px = pt.x as f32 * CELL_W - CELL_W / 2.0;
                        let py = pt.y as f32 * CELL_H - CELL_H / 2.0;
                        match fx {
                            // 原版 Magic 库帧动画特效
                            Some((lib, base, frames)) => {
                                commands.spawn((
                                    Sprite::default(),
                                    Transform::from_xyz(px, -py, 700.0),
                                    Visibility::Hidden,
                                    EffectAnim {
                                        lib,
                                        base,
                                        frames,
                                        born: time.elapsed_secs_f64(),
                                        px,
                                        py,
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
                                if let Some((a, b)) = r.bar.take() {
                                    commands.entity(a).despawn();
                                    commands.entity(b).despawn();
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
                            _ => 0,
                        };
                        if anim != r.anim {
                            r.anim_t = 0.0; // 动作切换从头播 (攻击/死亡一次性动画)
                        }
                        r.anim = anim;
                        if let Some(d) = e.dir {
                            r.dir = (d as usize) % 8;
                        }
                        if let Some(a) = e.armour {
                            r.armour = a;
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
    mut images: ResMut<Assets<Image>>,
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
            if let Some((a, b)) = r.bar.take() {
                commands.entity(a).despawn();
                commands.entity(b).despawn();
            }
            if let Some(w) = r.wep_entity.take() {
                commands.entity(w).despawn();
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
        // 帧表: 玩家=CArmour (站/走/跑), 怪物=Mon 库 (站/走/攻/死)
        let (layer, frame_idx) = if let Some(n) = r.image {
            let idx = match r.anim {
                4 => 144 + r.dir * 10 + (((r.anim_t / 0.13) as usize).min(9)), // 死亡一次性, 停在末帧
                3 => 80 + r.dir * 6 + ((r.anim_t / 0.15) as usize % 6),
                _ if walking => 32 + r.dir * 6 + foot,
                _ => r.dir * 4 + ((r.anim_t / 0.25) as usize % 4),
            };
            (Layer::Mon(n), idx)
        } else {
            let idx = if walking && r.anim == 2 {
                80 + r.dir * 6 + foot
            } else if walking {
                32 + r.dir * 6 + foot
            } else {
                r.dir * 4 + ((r.anim_t / 0.2) as usize % 4)
            };
            (Layer::Hum(r.armour), idx)
        };
        let Some(f) = world.frame(layer, 0, frame_idx as i32) else {
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
                    color: Color::srgb(0.88, 0.25, 0.25),
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
    let mut it = cred.splitn(3, ':');
    let (Some(user), Some(pass)) = (it.next(), it.next()) else {
        return;
    };
    let class = match it.next() {
        Some("mage") => CharacterClass::Mage,
        Some("taoist") => CharacterClass::Taoist,
        _ => CharacterClass::Warrior,
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
                    class,
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

/// 技能 → 原版特效 (库 0=Magic/1=Magic2, 起始帧, 帧数)。
/// 帧号出处: Crystal PlayerObject.cs 各 Spell 的 Effect(...) 定义
fn skill_fx(id: &str) -> Option<(u8, i32, u8)> {
    Some(match id {
        "huoqiu" => (0, 170, 10),       // FireBall 命中爆焰
        "zhiyu" => (0, 370, 10),        // Healing 金光
        "leidian" => (1, 10, 5),        // ThunderBolt 落雷
        "shidu" => (0, 770, 10),        // Poisoning 毒雾
        "huofu" => (0, 1360, 10),       // SoulFireBall 符爆
        "bingpaoxiao" => (0, 3850, 20), // IceStorm 冰暴
        "shizihou" => (1, 710, 20),     // LionRoar 吼波
        _ => return None,
    })
}

/// 原版技能特效帧动画 (100ms/帧, 播完自毁; blend 亮度透明)
#[derive(Component)]
struct EffectAnim {
    lib: u8,
    base: i32,
    frames: u8,
    born: f64,
    px: f32,
    py: f32,
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
    net.cds
        .insert(s.id.clone(), now + s.cooldown_ms as f64 / 1000.0);
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
        let k = ((now - fx.born) / 0.1) as i32;
        if k >= fx.frames as i32 {
            commands.entity(e).despawn();
            continue;
        }
        if let Some(f) = world.frame_ex(Layer::Fx(fx.lib), 0, fx.base + k, true) {
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
