//! 地图原图瓦片渲染（管理台预览用）。
//!
//! 按 Crystal 画法把 `.map` 的 back/mid/front 三层解码合成为 PNG 瓦片：
//! 每块 [`TILE_CELLS`]² 格 = 768×512 像素，前端拼成可拖动的大图。
//! 与客户端 `client/src/main.rs` 的绘制规则保持一致（地板判据、对象锚底、
//! 加色帧亮度透明、库注册表）。

use std::collections::HashMap;
use std::sync::Mutex;

use mir_formats::map::MirMap;
use mir_formats::mfl::AnyLib;

pub const CELL_W: u32 = 48;
pub const CELL_H: u32 = 32;
/// 每块瓦片的格数（正方）
pub const TILE_CELLS: u32 = 16;
/// 高物件最多向上溢出的格数（渲染时多扫这些行，避免瓦片下缘缺半截建筑）
const OVERFLOW_ROWS: u32 = 12;

static MAP_LIBS: Mutex<Option<HashMap<String, Option<AnyLib>>>> = Mutex::new(None);
/// 瓦片 PNG 缓存 (map, tx, ty) → bytes；管理台预览只读, 无需失效
type TileCache = HashMap<(String, u32, u32), std::sync::Arc<Vec<u8>>>;
static TILE_CACHE: Mutex<Option<TileCache>> = Mutex::new(None);

/// 带缓存的瓦片渲染
pub fn tile_cached(map_name: &str, map: &MirMap, tx: u32, ty: u32) -> std::sync::Arc<Vec<u8>> {
    let key = (map_name.to_string(), tx, ty);
    if let Ok(mut g) = TILE_CACHE.lock() {
        if let Some(hit) = g.get_or_insert_with(TileCache::new).get(&key) {
            return hit.clone();
        }
    }
    let png = std::sync::Arc::new(render_tile(map, tx, ty));
    if let Ok(mut g) = TILE_CACHE.lock() {
        let c = g.get_or_insert_with(TileCache::new);
        // 简单上限, 超出即清空 (预览场景足够)
        if c.len() > 600 {
            c.clear();
        }
        c.insert(key, png.clone());
    }
    png
}

/// 三层各自的库基址 (盛大格式: back=100/mid=110/front=120)
#[derive(Clone, Copy)]
pub enum MapLayer {
    Back,
    Mid,
    Front,
}

/// 库号 → packs/map 下的库文件名。
/// 盛大格式: 库文件后缀 = 值 - 层基址 + 1 (后缀 1 = 无后缀基础套)。
/// 与客户端 lib_name 同一套规则 (三张市售图实测验证: 207/187/100 套)。
fn lib_name(layer: MapLayer, lib: i16) -> Option<String> {
    let (base, stem) = match layer {
        MapLayer::Back => (100, "Tiles"),
        MapLayer::Mid => (110, "SmTiles"),
        MapLayer::Front => (120, "Objects"),
    };
    let suffix = lib as i32 - base + 1;
    if suffix < 1 {
        return None;
    }
    Some(if suffix == 1 {
        stem.to_string()
    } else {
        format!("{stem}{suffix}")
    })
}

/// 取一帧图像（进程内缓存已解析的库）
fn with_frame<R>(
    layer: MapLayer,
    lib: i16,
    idx: i32,
    f: impl FnOnce(&mir_formats::DecodedImage) -> R,
) -> Option<R> {
    if idx < 0 {
        return None;
    }
    let name = lib_name(layer, lib)?;
    let mut guard = MAP_LIBS.lock().ok()?;
    let cache = guard.get_or_insert_with(HashMap::new);
    if !cache.contains_key(&name) {
        // 图库只走 packs/map
        let packs = std::env::var("MIRFORGE_PACKS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("packs"));
        let parsed = AnyLib::open(&packs.join("map").join(format!("{name}.mfl"))).ok();
        cache.insert(name.clone(), parsed);
    }
    let img = cache
        .get(&name)?
        .as_ref()?
        .image(idx as usize)
        .ok()
        .flatten()?;
    Some(f(&img))
}

fn is_floor_size(w: u16, h: u16) -> bool {
    (w as u32 == CELL_W && h as u32 == CELL_H) || (w as u32 == CELL_W * 2 && h as u32 == CELL_H * 2)
}

/// 把一帧混合到画布（source-over；blend=true 时按亮度当作 alpha，近似加色）
#[allow(clippy::too_many_arguments)]
fn blit(
    canvas: &mut image::RgbaImage,
    img: &mir_formats::DecodedImage,
    dx: i64,
    dy: i64,
    blend: bool,
) {
    let (cw, ch) = (canvas.width() as i64, canvas.height() as i64);
    for y in 0..img.height as i64 {
        let ty = dy + y;
        if ty < 0 || ty >= ch {
            continue;
        }
        for x in 0..img.width as i64 {
            let tx = dx + x;
            if tx < 0 || tx >= cw {
                continue;
            }
            let si = ((y * img.width as i64 + x) * 4) as usize;
            let px = &img.rgba[si..si + 4];
            let mut a = px[3] as u32;
            if blend {
                a = a.min(px[0].max(px[1]).max(px[2]) as u32);
            }
            if a == 0 {
                continue;
            }
            let dst = canvas.get_pixel_mut(tx as u32, ty as u32);
            for c in 0..3 {
                dst[c] = ((px[c] as u32 * a + dst[c] as u32 * (255 - a)) / 255) as u8;
            }
            dst[3] = 255;
        }
    }
}

/// 渲染一块瓦片：cells [tx*16,ty*16) 起的 16×16 格
pub fn render_tile(map: &MirMap, tx: u32, ty: u32) -> Vec<u8> {
    let (tw, th) = (TILE_CELLS * CELL_W, TILE_CELLS * CELL_H);
    let mut canvas = image::RgbaImage::from_pixel(tw, th, image::Rgba([14, 16, 24, 255]));
    let (ox, oy) = ((tx * TILE_CELLS) as i64, (ty * TILE_CELLS) as i64);
    // 画布原点对应的世界像素
    let (px0, py0) = (ox * CELL_W as i64, oy * CELL_H as i64);

    let cell_at = |x: i64, y: i64| {
        if x < 0 || y < 0 || x >= map.width as i64 || y >= map.height as i64 {
            None
        } else {
            map.cell(x as u32, y as u32).copied()
        }
    };

    // ── 地板层 ──
    for cy in oy..oy + TILE_CELLS as i64 {
        for cx in ox..ox + TILE_CELLS as i64 {
            let Some(c) = cell_at(cx, cy) else { continue };
            let (dx, dy) = (cx * CELL_W as i64 - px0, cy * CELL_H as i64 - py0);
            if c.back >= 0 && cx % 2 == 0 && cy % 2 == 0 {
                with_frame(MapLayer::Back, c.back_lib, c.back, |img| {
                    blit(&mut canvas, img, dx, dy, false)
                });
            }
            if c.mid >= 0 {
                let floor = with_frame(MapLayer::Mid, c.mid_lib, c.mid, |img| {
                    is_floor_size(img.width, img.height)
                })
                .unwrap_or(false);
                if floor {
                    with_frame(MapLayer::Mid, c.mid_lib, c.mid, |img| {
                        blit(&mut canvas, img, dx, dy, false)
                    });
                }
            }
            if c.front >= 0 {
                let floor = with_frame(MapLayer::Front, c.front_lib, c.front, |img| {
                    is_floor_size(img.width, img.height)
                })
                .unwrap_or(false);
                if floor {
                    with_frame(MapLayer::Front, c.front_lib, c.front, |img| {
                        blit(&mut canvas, img, dx, dy, false)
                    });
                }
            }
        }
    }

    // ── 对象层（锚底；多扫下方若干行，让高建筑正确溢出到本瓦片） ──
    for cy in oy..oy + (TILE_CELLS + OVERFLOW_ROWS) as i64 {
        for cx in ox..ox + TILE_CELLS as i64 {
            let Some(c) = cell_at(cx, cy) else { continue };
            let base_x = cx * CELL_W as i64 - px0;
            let bottom = (cy + 1) * CELL_H as i64 - py0;
            // mid 非标准尺寸 → 对象
            if c.mid >= 0 {
                with_frame(MapLayer::Mid, c.mid_lib, c.mid, |img| {
                    if !is_floor_size(img.width, img.height) {
                        blit(&mut canvas, img, base_x, bottom - img.height as i64, false);
                    }
                });
            }
            if c.front >= 0 {
                let blend = c.ani_frame & 0x80 > 0;
                with_frame(MapLayer::Front, c.front_lib, c.front, |img| {
                    if is_floor_size(img.width, img.height) && c.ani_frame & 0x7F == 0 {
                        return; // 已在地板层画过
                    }
                    // Crystal 对象放置特例
                    let (dx, dy) = if blend && matches!(c.front_lib as i32, 14 | 27 | 100..=198) {
                        (
                            base_x + img.offset_x as i64,
                            bottom - 3 * CELL_H as i64 + img.offset_y as i64,
                        )
                    } else if blend && (2723..=2732).contains(&c.front) {
                        (
                            base_x + img.offset_x as i64,
                            bottom - img.height as i64 + img.offset_y as i64,
                        )
                    } else if c.front_lib == 28 && (img.offset_x != 0 || img.offset_y != 0) {
                        (
                            base_x + img.offset_x as i64,
                            bottom - CELL_H as i64 + img.offset_y as i64,
                        )
                    } else {
                        (base_x, bottom - img.height as i64)
                    };
                    blit(&mut canvas, img, dx, dy, blend);
                });
            }
        }
    }

    let mut buf = std::io::Cursor::new(Vec::new());
    let _ = canvas.write_to(&mut buf, image::ImageFormat::Png);
    buf.into_inner()
}
