//! 调试工具: 导出 .Lib 单帧为完整尺寸 PNG。
//! 用法: cargo run -p mir-formats --example dump_frame -- <lib路径> <帧号> <输出.png>

use mir_formats::crystal_lib::CrystalLib;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let lib = CrystalLib::parse(std::fs::read(&args[1]).expect("读取失败")).expect("解析失败");
    let idx = args[2].parse::<usize>().unwrap();
    match lib.image(idx) {
        Ok(Some(img)) => {
            let buf =
                image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.rgba.clone())
                    .expect("尺寸不符");
            buf.save(&args[3]).expect("保存失败");
            println!(
                "帧 {idx}: {}x{} off=({},{}) → {}",
                img.width, img.height, img.offset_x, img.offset_y, args[3]
            );
        }
        other => println!("帧 {idx}: {other:?}"),
    }
}
