//! 离屏合成: 走与客户端相同的 帧→图集→采样 路径, 输出 PPM 供目检
//! 用法: cargo run -p mirforge-client --example compose -- <res> <x0> <y0> <w> <h> <out.ppm>

use std::collections::HashMap;
use std::path::Path;

use mir_atlas::{AtlasCpu, PAGE_SIZE};
use mir_formats::crystal_lib::CrystalLib;

/// (页, x, y, 宽, 高, 偏移x, 偏移y)
type FrameInfo = (usize, u32, u32, u32, u32, i16, i16);
type FrameCache = HashMap<(u8, i16, i32), Option<FrameInfo>>;

fn lib_name(front_lib: i16) -> Option<String> {
    Some(match front_lib {
        0 => "Tiles".into(),
        1 => "SmTiles".into(),
        2 => "Objects".into(),
        n if n > 2 => format!("Objects{}", n - 1),
        _ => return None,
    })
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let root = Path::new(&a[1]);
    let (x0, y0, w, h): (i32, i32, i32, i32) = (
        a[2].parse().unwrap(),
        a[3].parse().unwrap(),
        a[4].parse().unwrap(),
        a[5].parse().unwrap(),
    );
    let idx = mir_formats::scan::ResourceIndex::scan(root);
    let map_path = idx
        .maps
        .iter()
        .find(|m| m.path.file_name().unwrap() == "0.map")
        .unwrap();
    let map = mir_formats::map::parse(&std::fs::read(&map_path.path).unwrap()).unwrap();
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
        .unwrap();

    let mut libs: HashMap<String, Option<CrystalLib>> = HashMap::new();
    let mut atlas = AtlasCpu::default();
    let mut frames: FrameCache = HashMap::new();

    let (iw, ih) = ((w * 48) as usize, (h * 32) as usize);
    let mut out = vec![20u8; iw * ih * 3];

    let get_frame = |layer: u8,
                     fl: i16,
                     fi: i32,
                     libs: &mut HashMap<String, Option<CrystalLib>>,
                     atlas: &mut AtlasCpu,
                     frames: &mut FrameCache| {
        let key = (layer, fl, fi);
        if let Some(c) = frames.get(&key) {
            return *c;
        }
        let name = match layer {
            0 => Some("Tiles".to_string()),
            1 => Some("SmTiles".to_string()),
            _ => lib_name(fl),
        };
        let v = name.and_then(|n| {
            let lib = libs
                .entry(n.clone())
                .or_insert_with(|| {
                    for cand in [format!("{n}.Lib"), format!("{n}.lib")] {
                        let p = lib_dir.join(&cand);
                        if p.exists() {
                            return std::fs::read(p)
                                .ok()
                                .and_then(|d| CrystalLib::parse(d).ok());
                        }
                    }
                    None
                })
                .as_ref()?;
            let img = lib.image(fi as usize).ok().flatten()?;
            if std::env::var("MIRFORGE_NO_ATLAS").is_ok() {
                // 旁路: 这帧独占一整页 (定位在 0,0), 排除打包器嫌疑
                let mut page = mir_atlas::PageBuf::default();
                for row in 0..img.height as usize {
                    let src = row * img.width as usize * 4;
                    let dst = row * mir_atlas::PAGE_SIZE as usize * 4;
                    page.rgba[dst..dst + img.width as usize * 4]
                        .copy_from_slice(&img.rgba[src..src + img.width as usize * 4]);
                }
                let pi = atlas.pages.len();
                atlas.pages.push(page);
                let v = Some((
                    pi,
                    0,
                    0,
                    img.width as u32,
                    img.height as u32,
                    img.offset_x,
                    img.offset_y,
                ));
                frames.insert(key, v);
                return v;
            }
            let placed = atlas.insert(img.width as u32, img.height as u32, &img.rgba)?;
            Some((
                placed.page,
                placed.x,
                placed.y,
                img.width as u32,
                img.height as u32,
                img.offset_x,
                img.offset_y,
            ))
        });
        frames.insert(key, v);
        v
    };

    let blit = |atlas: &AtlasCpu, f: FrameInfo, px: i32, py: i32, out: &mut Vec<u8>| {
        let (page, ax, ay, fw, fh, _, _) = f;
        let pg = &atlas.pages[page].rgba;
        for y in 0..fh as i32 {
            for x in 0..fw as i32 {
                let (dx, dy) = (px + x, py + y);
                if dx < 0 || dy < 0 || dx >= iw as i32 || dy >= ih as i32 {
                    continue;
                }
                let si = (((ay + y as u32) * PAGE_SIZE + ax + x as u32) * 4) as usize;
                let al = pg[si + 3] as u32;
                if al == 0 {
                    continue;
                }
                let di = (dy as usize * iw + dx as usize) * 3;
                for c in 0..3 {
                    out[di + c] =
                        ((pg[si + c] as u32 * al + out[di + c] as u32 * (255 - al)) / 255) as u8;
                }
            }
        }
    };

    for cy in y0..y0 + h {
        for cx in x0..x0 + w {
            let cell = *map.cell(cx as u32, cy as u32).unwrap();
            if cell.back >= 0 && cx % 2 == 0 && cy % 2 == 0 {
                if let Some(f) = get_frame(
                    0,
                    cell.back_lib,
                    cell.back,
                    &mut libs,
                    &mut atlas,
                    &mut frames,
                ) {
                    blit(&atlas, f, (cx - x0) * 48, (cy - y0) * 32, &mut out);
                }
            }
        }
    }
    for cy in y0..y0 + h {
        for cx in x0..x0 + w {
            let cell = *map.cell(cx as u32, cy as u32).unwrap();
            if cell.mid >= 0 {
                if let Some(f) = get_frame(
                    1,
                    cell.mid_lib,
                    cell.mid,
                    &mut libs,
                    &mut atlas,
                    &mut frames,
                ) {
                    blit(&atlas, f, (cx - x0) * 48, (cy - y0) * 32, &mut out);
                }
            }
            if cell.front >= 0 {
                let got = get_frame(
                    2,
                    cell.front_lib,
                    cell.front,
                    &mut libs,
                    &mut atlas,
                    &mut frames,
                );
                if got.is_none() {
                    println!(
                        "MISS front {}@{} at ({cx},{cy})",
                        cell.front, cell.front_lib
                    );
                }
                if let Some(f) = got {
                    let (_, _, _, _, fh, ox, oy) = f;
                    blit(
                        &atlas,
                        f,
                        (cx - x0) * 48 + ox as i32,
                        (cy - y0) * 32 + oy as i32 - (fh as i32 - 32),
                        &mut out,
                    );
                }
            }
        }
    }
    let mut ppm = format!("P6\n{iw} {ih}\n255\n").into_bytes();
    ppm.extend_from_slice(&out);
    std::fs::write(&a[6], ppm).unwrap();
    println!("composed {}x{} pages={}", iw, ih, atlas.pages.len());
}
