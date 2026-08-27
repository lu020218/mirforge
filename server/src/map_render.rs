//! 地图原图瓦片渲染（管理台预览用）。
//!
//! 按 Crystal 画法把 `.map` 的 back/mid/front 三层解码合成为 PNG 瓦片：
//! 每块 [`TILE_CELLS`]² 格 = 768×512 像素，前端拼成可拖动的大图。
//! 与客户端 `client/src/main.rs` 的绘制规则保持一致（地板判据、对象锚底、
//! 加色帧亮度透明、库注册表）。

use std::collections::HashMap;
use std::sync::Mutex;

use mir_formats::crystal_lib::CrystalLib;
use mir_formats::map::MirMap;

pub const CELL_W: u32 = 48;
pub const CELL_H: u32 = 32;
/// 每块瓦片的格数（正方）
pub const TILE_CELLS: u32 = 16;
/// 高物件最多向上溢出的格数（渲染时多扫这些行，避免瓦片下缘缺半截建筑）
const OVERFLOW_ROWS: u32 = 12;

static MAP_LIBS: Mutex<Option<HashMap<String, Option<CrystalLib>>>> = Mutex::new(None);
/// 瓦片 PNG 缓存 (map, tx, ty) → bytes；管理台预览只读, 无需失效
type TileCache = HashMap<(String, u32, u32), std::sync::Arc<Vec<u8>>>;
static TILE_CACHE: Mutex<Option<TileCache>> = Mutex::new(None);

/// 带缓存的瓦片渲染
pub fn tile_cached(
    res_root: &std::path::Path,
    map_name: &str,
    map: &MirMap,
    tx: u32,
    ty: u32,
) -> std::sync::Arc<Vec<u8>> {
    let key = (map_name.to_string(), tx, ty);
    if let Ok(mut g) = TILE_CACHE.lock() {
        if let Some(hit) = g.get_or_insert_with(TileCache::new).get(&key) {
            return hit.clone();
        }
    }
    let png = std::sync::Arc::new(render_tile(res_root, map, tx, ty));
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

/// 库号 → Data/Map 下的库文件名（Crystal Libraries.MapLibs 注册表）
fn lib_name(lib: i16) -> Option<String> {
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
        0 => "WemadeMir2/Tiles".into(),
        1 => "WemadeMir2/SmTiles".into(),
        2 => "WemadeMir2/Objects".into(),
        3..=28 => format!("WemadeMir2/Objects{}", l - 1),
        90 => "WemadeMir2/Objects_32bit".into(),
        100 => "ShandaMir2/Tiles".into(),
        101..=109 => format!("ShandaMir2/Tiles{}", l - 99),
        110 => "ShandaMir2/SmTiles".into(),
        111..=119 => format!("ShandaMir2/SmTiles{}", l - 109),
        120 => "ShandaMir2/Objects".into(),
        121..=150 => format!("ShandaMir2/Objects{}", l - 119),
        190 => "ShandaMir2/AniTiles1".into(),
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
            format!("WemadeMir3/{dir}{}", MIR3_NAMES[n])
        }
        300..=374 => {
            let o = (l - 300) as usize;
            let (s, n) = (o / 15, o % 15);
            if s >= MIR3_STATE.len() || n >= MIR3_NAMES.len() {
                return None;
            }
            format!("ShandaMir3/{}{}", MIR3_NAMES[n], MIR3_STATE[s])
        }
        _ => return None,
    })
}

/// 取一帧图像（进程内缓存已解析的库）
fn with_frame<R>(
    res_root: &std::path::Path,
    lib: i16,
    idx: i32,
    f: impl FnOnce(&mir_formats::DecodedImage) -> R,
) -> Option<R> {
    if idx < 0 {
        return None;
    }
    let name = lib_name(lib)?;
    let mut guard = MAP_LIBS.lock().ok()?;
    let cache = guard.get_or_insert_with(HashMap::new);
    if !cache.contains_key(&name) {
        let mut parsed = None;
        for cand in [format!("{name}.Lib"), format!("{name}.lib")] {
            let p = res_root.join("Data/Map").join(&cand);
            if let Ok(data) = std::fs::read(&p) {
                parsed = CrystalLib::parse(data).ok();
                break;
            }
        }
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
pub fn render_tile(res_root: &std::path::Path, map: &MirMap, tx: u32, ty: u32) -> Vec<u8> {
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
                with_frame(res_root, c.back_lib, c.back, |img| {
                    blit(&mut canvas, img, dx, dy, false)
                });
            }
            if c.mid >= 0 {
                let floor = with_frame(res_root, c.mid_lib, c.mid, |img| {
                    is_floor_size(img.width, img.height)
                })
                .unwrap_or(false);
                if floor {
                    with_frame(res_root, c.mid_lib, c.mid, |img| {
                        blit(&mut canvas, img, dx, dy, false)
                    });
                }
            }
            if c.front >= 0 {
                let floor = with_frame(res_root, c.front_lib, c.front, |img| {
                    is_floor_size(img.width, img.height)
                })
                .unwrap_or(false);
                if floor {
                    with_frame(res_root, c.front_lib, c.front, |img| {
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
                with_frame(res_root, c.mid_lib, c.mid, |img| {
                    if !is_floor_size(img.width, img.height) {
                        blit(&mut canvas, img, base_x, bottom - img.height as i64, false);
                    }
                });
            }
            if c.front >= 0 {
                let blend = c.ani_frame & 0x80 > 0;
                with_frame(res_root, c.front_lib, c.front, |img| {
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
