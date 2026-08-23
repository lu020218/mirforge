//! 对拍工具: 解码单帧并打印统计
//! 用法: cargo run -p mir-formats --example dumpframe -- <lib> <index>

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let lib = mir_formats::crystal_lib::CrystalLib::parse(std::fs::read(&a[1]).unwrap()).unwrap();
    let idx: usize = a[2].parse().unwrap();
    match lib.image(idx) {
        Ok(Some(im)) => {
            let (mut opaque, mut black) = (0u32, 0u32);
            for px in im.rgba.chunks_exact(4) {
                if px[3] > 0 {
                    opaque += 1;
                    if px[0] < 8 && px[1] < 8 && px[2] < 8 {
                        black += 1;
                    }
                }
            }
            println!(
                "v{} frame {idx}: {}x{} off ({},{}) opaque {opaque} black {black}",
                lib.version, im.width, im.height, im.offset_x, im.offset_y
            );
            if let Some(out) = a.get(3) {
                // PPM_OUT: 帧内容目检 (透明→洋红)
                let mut ppm = format!(
                    "P6
{} {}
255
",
                    im.width, im.height
                )
                .into_bytes();
                for px in im.rgba.chunks_exact(4) {
                    if px[3] == 0 {
                        ppm.extend_from_slice(&[255, 0, 255]);
                    } else {
                        ppm.extend_from_slice(&px[..3]);
                    }
                }
                std::fs::write(out, ppm).unwrap();
            }
        }
        Ok(None) => println!("frame {idx}: 空帧"),
        Err(e) => println!("frame {idx}: 错误 {e}"),
    }
}
