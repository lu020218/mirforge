//! 调试工具: 打印地图指定区域各格三层引用与帧元数据 (库/帧号/尺寸/偏移)。
//! 用法: cargo run -p mir-formats --example cell_probe -- <map> <libroot> <x0> <y0> <x1> <y1> [仅blend:1]

use mir_formats::crystal_lib::CrystalLib;
use std::collections::HashMap;

fn lib_name(l: i16) -> Option<String> {
    Some(match l as i32 {
        0 => "WemadeMir2/Tiles".into(),
        1 => "WemadeMir2/SmTiles".into(),
        2 => "WemadeMir2/Objects".into(),
        n @ 3..=28 => format!("WemadeMir2/Objects{}", n - 1),
        90 => "WemadeMir2/Objects_32bit".into(),
        _ => return None,
    })
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let map = mir_formats::map::parse(&std::fs::read(&a[1]).unwrap()).unwrap();
    let root = std::path::Path::new(&a[2]);
    let (x0, y0, x1, y1) = (
        a[3].parse::<u32>().unwrap(),
        a[4].parse::<u32>().unwrap(),
        a[5].parse::<u32>().unwrap(),
        a[6].parse::<u32>().unwrap(),
    );
    let only_blend = a.get(7).map(|s| s == "1").unwrap_or(false);
    let mut libs: HashMap<i16, Option<CrystalLib>> = HashMap::new();
    for y in y0..=y1 {
        for x in x0..=x1 {
            let Some(c) = map.cell(x, y) else { continue };
            if c.front < 0 {
                continue;
            }
            let blend = c.ani_frame & 0x80 != 0;
            if only_blend && !blend {
                continue;
            }
            let lib = libs
                .entry(c.front_lib)
                .or_insert_with(|| {
                    lib_name(c.front_lib)
                        .and_then(|n| std::fs::read(root.join(format!("{n}.Lib"))).ok())
                        .and_then(|d| CrystalLib::parse(d).ok())
                })
                .as_ref();
            let meta = lib
                .and_then(|l| l.image(c.front as usize).ok().flatten())
                .map(|i| {
                    format!(
                        "{}x{} off=({},{})",
                        i.width, i.height, i.offset_x, i.offset_y
                    )
                })
                .unwrap_or_else(|| "?".into());
            println!(
                "({x},{y}) front lib={} idx={} ani=0x{:02X} tick={} {}",
                c.front_lib, c.front, c.ani_frame, c.ani_tick, meta
            );
        }
    }
}
