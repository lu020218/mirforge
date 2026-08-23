//! 图库蒙太奇: 把一段帧铺成网格输出 PPM (目检帧布局用)
//! 用法: cargo run -p mir-formats --example libmontage -- <lib> <start> <count> <cols> <out.ppm>

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let lib = mir_formats::crystal_lib::CrystalLib::parse(std::fs::read(&a[1]).unwrap()).unwrap();
    let (start, count, cols): (usize, usize, usize) = (
        a[2].parse().unwrap(),
        a[3].parse().unwrap(),
        a[4].parse().unwrap(),
    );
    let (cw, ch) = (72usize, 100usize);
    let rows = count.div_ceil(cols);
    let (iw, ih) = (cols * cw, rows * ch);
    let mut out = vec![24u8; iw * ih * 3];
    for i in 0..count {
        let Ok(Some(im)) = lib.image(start + i) else {
            continue;
        };
        let (gx, gy) = ((i % cols) * cw, (i / cols) * ch);
        // 缩放塞进格 (最近邻)
        let s = (cw as f32 / im.width as f32)
            .min(ch as f32 / im.height as f32)
            .min(1.0);
        let (dw, dh) = (
            (im.width as f32 * s) as usize,
            (im.height as f32 * s) as usize,
        );
        for y in 0..dh {
            for x in 0..dw {
                let sx = (x as f32 / s) as usize;
                let sy = (y as f32 / s) as usize;
                let si = (sy * im.width as usize + sx) * 4;
                if im.rgba[si + 3] == 0 {
                    continue;
                }
                let di = ((gy + y) * iw + gx + x) * 3;
                out[di..di + 3].copy_from_slice(&im.rgba[si..si + 3]);
            }
        }
        // 格线
        for x in 0..cw {
            let di = ((gy + ch - 1) * iw + gx + x) * 3;
            out[di] = 60;
        }
    }
    let mut ppm = format!("P6\n{iw} {ih}\n255\n").into_bytes();
    ppm.extend_from_slice(&out);
    std::fs::write(&a[5], ppm).unwrap();
    println!("montage {}..{} cols={cols}", start, start + count);
}
