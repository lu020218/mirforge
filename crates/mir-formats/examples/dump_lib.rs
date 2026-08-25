//! 调试工具: 导出 .Lib 帧为 PNG 网格图, 供人工核对帧号。
//! 用法: cargo run -p mir-formats --example dump_lib -- <lib路径> <起始帧> <帧数> <输出.png>

use mir_formats::crystal_lib::CrystalLib;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (path, start, count, out) = (
        &args[1],
        args[2].parse::<usize>().unwrap(),
        args[3].parse::<usize>().unwrap(),
        &args[4],
    );
    let lib = CrystalLib::parse(std::fs::read(path).expect("读取失败")).expect("解析失败");
    // 网格: 每行 10 帧, 单元 64×64, 帧号标注省略 (按序即帧号)
    let cols = 10usize;
    let rows = count.div_ceil(cols);
    let (cw, ch) = (64u32, 64u32);
    let mut canvas = image::RgbaImage::new(cols as u32 * cw, rows as u32 * ch);
    // 棋盘底便于看透明
    for (x, y, p) in canvas.enumerate_pixels_mut() {
        let dark = ((x / 8) + (y / 8)) % 2 == 0;
        *p = image::Rgba(if dark {
            [40, 40, 48, 255]
        } else {
            [56, 56, 66, 255]
        });
    }
    for i in 0..count {
        let Ok(Some(img)) = lib.image(start + i) else {
            continue;
        };
        let (col, row) = (i % cols, i / cols);
        let (ox, oy) = (col as u32 * cw, row as u32 * ch);
        for y in 0..img.height.min(ch as u16) {
            for x in 0..img.width.min(cw as u16) {
                let si = (y as usize * img.width as usize + x as usize) * 4;
                let px = &img.rgba[si..si + 4];
                if px[3] > 0 {
                    canvas.put_pixel(
                        ox + x as u32,
                        oy + y as u32,
                        image::Rgba([px[0], px[1], px[2], px[3]]),
                    );
                }
            }
        }
    }
    canvas.save(out).expect("保存失败");
    println!("已导出 {count} 帧 (自 {start}) → {out}");
}
