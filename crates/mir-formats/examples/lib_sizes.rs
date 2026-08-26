//! 调试工具: 列出 .Lib 全部帧的尺寸 (跳过空帧)。
//! 用法: cargo run -p mir-formats --example lib_sizes -- <lib路径>

use mir_formats::crystal_lib::CrystalLib;

fn main() {
    let path = std::env::args().nth(1).expect("用法: lib_sizes <lib路径>");
    let lib = CrystalLib::parse(std::fs::read(&path).expect("读取失败")).expect("解析失败");
    let mut i = 0usize;
    let mut empty_run = 0u32;
    loop {
        match lib.image(i) {
            Ok(Some(img)) => {
                empty_run = 0;
                println!("{i}: {}x{}", img.width, img.height);
            }
            Ok(None) => {
                empty_run += 1;
                if empty_run > 200 {
                    break;
                }
            }
            Err(_) => break,
        }
        i += 1;
    }
}
