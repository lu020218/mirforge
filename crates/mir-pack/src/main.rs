//! packs/ 自有资源打包工具。
//!
//! 市售素材普遍是 Crystal LibraryEditor 的解包形态: 逐帧 `NNNNN.PNG` +
//! `Placements/NNNNN.txt` (两行 = X/Y 锚点偏移)。本工具把这种目录打成
//! 引擎直读的 `.mfl`, 也能反向出预览图供验货。
//!
//! 用法:
//!   mir-pack pack <PNG目录> <输出.mfl>
//!   mir-pack info <输入.mfl>
//!   mir-pack preview <输入.mfl> <输出.png> [起始帧] [帧数]

use std::path::{Path, PathBuf};

use mir_formats::mfl::{self, MflFrame, MflLib};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("pack") if args.len() == 3 => pack(Path::new(&args[1]), Path::new(&args[2])),
        Some("info") if args.len() == 2 => info(Path::new(&args[1])),
        Some("preview") if (3..=5).contains(&args.len()) => {
            let start = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let n = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(16);
            preview(Path::new(&args[1]), Path::new(&args[2]), start, n)
        }
        _ => {
            eprintln!("用法:");
            eprintln!("  mir-pack pack <PNG目录> <输出.mfl>     打包 (目录含 NNNNN.PNG + Placements/)");
            eprintln!("  mir-pack info <输入.mfl>              查看帧数统计");
            eprintln!("  mir-pack preview <输入.mfl> <输出.png> [起始帧] [帧数]  出预览拼图");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("失败: {e}");
        std::process::exit(1);
    }
}

type AnyErr = Box<dyn std::error::Error>;

/// 收集目录下的 NNNNN.PNG (大小写不限), 返回 帧号 → 路径
fn scan_frames(dir: &Path) -> Result<Vec<(usize, PathBuf)>, AnyErr> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let is_png = p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("png"));
        if !is_png {
            continue;
        }
        if let Ok(idx) = stem.parse::<usize>() {
            out.push((idx, p));
        }
    }
    out.sort_by_key(|(i, _)| *i);
    Ok(out)
}

/// 读 Placements/NNNNN.txt: 两行整数 = X/Y 偏移; 缺失按 (0,0)
fn placement(dir: &Path, idx: usize) -> (i16, i16) {
    let p = dir.join("Placements").join(format!("{idx:05}.txt"));
    let Ok(s) = std::fs::read_to_string(&p) else {
        return (0, 0);
    };
    let mut it = s.split_whitespace().filter_map(|t| t.parse::<i16>().ok());
    (it.next().unwrap_or(0), it.next().unwrap_or(0))
}

fn pack(src: &Path, out: &Path) -> Result<(), AnyErr> {
    let files = scan_frames(src)?;
    if files.is_empty() {
        return Err(format!("{} 下没有 NNNNN.PNG", src.display()).into());
    }
    let count = files.last().unwrap().0 + 1;
    let mut frames: Vec<Option<MflFrame>> = (0..count).map(|_| None).collect();
    let (mut ok, mut bad) = (0usize, 0usize);
    for (idx, path) in files {
        // 素材包里空帧常是 0 字节占位 PNG, 解不开的一律按空帧
        let Ok(img) = image::open(&path) else {
            bad += 1;
            continue;
        };
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 {
            bad += 1;
            continue;
        }
        let (x, y) = placement(src, idx);
        frames[idx] = Some(MflFrame {
            width: w as u16,
            height: h as u16,
            offset_x: x,
            offset_y: y,
            rgba: rgba.into_raw(),
        });
        ok += 1;
    }
    let bytes = mfl::write(&frames)?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(out, &bytes)?;
    println!(
        "打包完成: {} 帧位, 实帧 {ok}, 空/坏帧 {bad}, {} KB → {}",
        count,
        bytes.len() / 1024,
        out.display()
    );
    Ok(())
}

fn info(path: &Path) -> Result<(), AnyErr> {
    let lib = MflLib::parse(std::fs::read(path)?)?;
    let mut real = 0usize;
    let (mut wmax, mut hmax) = (0u16, 0u16);
    for i in 0..lib.len() {
        if let Some(f) = lib.image(i)? {
            real += 1;
            wmax = wmax.max(f.width);
            hmax = hmax.max(f.height);
        }
    }
    println!(
        "{}: 帧位 {}, 实帧 {}, 最大帧 {}x{}",
        path.display(),
        lib.len(),
        real,
        wmax,
        hmax
    );
    Ok(())
}

/// 拼图预览: 从 start 起取 n 个实帧, 每帧一格横排 (8 帧换行)
fn preview(path: &Path, out: &Path, start: usize, n: usize) -> Result<(), AnyErr> {
    let lib = MflLib::parse(std::fs::read(path)?)?;
    let mut frames = Vec::new();
    let mut i = start;
    while frames.len() < n && i < lib.len() {
        if let Some(f) = lib.image(i)? {
            frames.push((i, f));
        }
        i += 1;
    }
    if frames.is_empty() {
        return Err("该区间没有实帧".into());
    }
    let cell_w = frames.iter().map(|(_, f)| f.width as u32).max().unwrap() + 4;
    let cell_h = frames.iter().map(|(_, f)| f.height as u32).max().unwrap() + 4;
    let cols = frames.len().min(8) as u32;
    let rows = frames.len().div_ceil(8) as u32;
    let mut canvas = image::RgbaImage::from_pixel(
        cols * cell_w,
        rows * cell_h,
        image::Rgba([40, 34, 28, 255]),
    );
    for (k, (_, f)) in frames.iter().enumerate() {
        let (cx, cy) = ((k as u32 % 8) * cell_w + 2, (k as u32 / 8) * cell_h + 2);
        for yy in 0..f.height as u32 {
            for xx in 0..f.width as u32 {
                let o = ((yy * f.width as u32 + xx) * 4) as usize;
                let px = image::Rgba([f.rgba[o], f.rgba[o + 1], f.rgba[o + 2], f.rgba[o + 3]]);
                if px.0[3] > 0 {
                    canvas.put_pixel(cx + xx, cy + yy, px);
                }
            }
        }
    }
    canvas.save(out)?;
    println!(
        "预览: 帧 {}..{} 共 {} 实帧 → {}",
        start,
        i,
        frames.len(),
        out.display()
    );
    Ok(())
}
