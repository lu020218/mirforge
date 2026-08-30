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
        Some("pack-list") if args.len() >= 3 => pack_list(
            Path::new(&args[1]),
            &args[2..].iter().map(PathBuf::from).collect::<Vec<_>>(),
        ),
        Some("info") if args.len() == 2 => info(Path::new(&args[1])),
        Some("convert") if args.len() == 3 => convert(Path::new(&args[1]), Path::new(&args[2])),
        Some("preview") if (3..=5).contains(&args.len()) => {
            let start = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let n = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(16);
            preview(Path::new(&args[1]), Path::new(&args[2]), start, n)
        }
        _ => {
            eprintln!("用法:");
            eprintln!(
                "  mir-pack pack <PNG目录> <输出.mfl>     打包 (目录含 NNNNN.PNG + Placements/)"
            );
            eprintln!(
                "  mir-pack pack-list <输出.mfl> <PNG>...  散图按参数顺序打成帧 (立绘等无锚点素材)"
            );
            eprintln!("  mir-pack convert <resources/Data> <packs>  引擎注册表内全部 Crystal 库转码为 .mfl");
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

/// 散图打包: 帧号 = 参数顺序。立绘/图标这类不吃帧表的素材用。
/// 路径可带 `@dx,dy` 后缀写入帧偏移 (立绘叠加层的对位微调数据)
fn pack_list(out: &Path, srcs: &[PathBuf]) -> Result<(), AnyErr> {
    let mut frames = Vec::new();
    for p in srcs {
        let raw = p.to_string_lossy();
        let (path_s, ox, oy) = match raw.rsplit_once('@') {
            Some((head, tail)) => {
                let mut it = tail.split(',').filter_map(|t| t.trim().parse::<i16>().ok());
                match (it.next(), it.next()) {
                    (Some(x), Some(y)) => (head.to_string(), x, y),
                    _ => (raw.to_string(), 0, 0), // @ 是路径一部分 (无合法偏移)
                }
            }
            None => (raw.to_string(), 0, 0),
        };
        let img = image::open(&path_s).map_err(|e| format!("{path_s}: {e}"))?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        frames.push(Some(MflFrame {
            width: w as u16,
            height: h as u16,
            offset_x: ox,
            offset_y: oy,
            rgba: rgba.into_raw(),
        }));
    }
    let bytes = mfl::write(&frames)?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(out, &bytes)?;
    println!("打包完成: {} 帧 → {}", frames.len(), out.display());
    Ok(())
}

/// 引擎注册表内的 Crystal 库 → packs 目标路径 (与客户端 pack_path 同一套映射)
///
/// 返回 (Data 下相对路径不带扩展名, packs 下相对路径)。地图库保持相对
/// 路径, 五大类型归各自目录, 单例库平铺。
fn convert_targets(data: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    // 编号目录: (Data 子目录, packs 类型目录)
    for (dir, kind) in [
        ("CArmour", "armor"),
        ("CWeapon", "weapon"),
        ("CHair", "hair"),
        ("Monster", "monster"),
        ("NPC", "npc"),
    ] {
        let Ok(rd) = std::fs::read_dir(data.join(dir)) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let (Some(stem), Some(ext)) = (
                p.file_stem().and_then(|s| s.to_str()),
                p.extension().and_then(|s| s.to_str()),
            ) else {
                continue;
            };
            if !ext.eq_ignore_ascii_case("lib") {
                continue;
            }
            if let Ok(n) = stem.parse::<u32>() {
                out.push((format!("{dir}/{stem}"), format!("{kind}/{n:03}.mfl")));
            }
        }
    }
    // 单例库
    for (name, dst) in [
        ("Items", "items.mfl"),
        ("MagIcon", "magicon.mfl"),
        ("mmap", "mmap.mfl"),
        ("Magic", "magic/000.mfl"),
        ("Magic2", "magic/001.mfl"),
    ] {
        if data.join(format!("{name}.Lib")).exists() || data.join(format!("{name}.lib")).exists() {
            out.push((name.to_string(), dst.to_string()));
        }
    }
    // 地图图库: Map/ 下全部 .Lib, 保持相对路径
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, String)>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if p
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("lib"))
            {
                if let Ok(rel) = p.strip_prefix(base) {
                    let rel = rel.with_extension("");
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    out.push((format!("Map/{rel}"), format!("map/{rel}.mfl")));
                }
            }
        }
    }
    walk(&data.join("Map"), &data.join("Map"), &mut out);
    out
}

/// 单库转码: Crystal 帧逐帧解码 → 流式写 .mfl
fn convert_one(src: &Path, dst: &Path) -> Result<(usize, usize), AnyErr> {
    let lib = mir_formats::crystal_lib::CrystalLib::parse(std::fs::read(src)?)?;
    let mut w = mfl::MflWriter::new(lib.len());
    let (mut ok, mut empty) = (0usize, 0usize);
    for i in 0..lib.len() {
        match lib.image(i).ok().flatten() {
            Some(img) => {
                w.push(&MflFrame {
                    width: img.width,
                    height: img.height,
                    offset_x: img.offset_x,
                    offset_y: img.offset_y,
                    rgba: img.rgba,
                })?;
                ok += 1;
            }
            None => {
                w.push_empty()?;
                empty += 1;
            }
        }
    }
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(dst, w.finish())?;
    Ok((ok, empty))
}

/// 全量转码 (多线程, 已存在的跳过 — 想重转先删目标文件)
fn convert(data: &Path, packs: &Path) -> Result<(), AnyErr> {
    let targets = convert_targets(data);
    let total = targets.len();
    let todo: Vec<_> = targets
        .into_iter()
        .filter(|(_, dst)| !packs.join(dst).exists())
        .collect();
    println!("注册表内库 {total} 个, 待转 {} 个 (已存在跳过)", todo.len());
    let jobs = std::sync::Mutex::new(todo.into_iter());
    let done = std::sync::atomic::AtomicUsize::new(0);
    let failed = std::sync::Mutex::new(Vec::<String>::new());
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| loop {
                let job = { jobs.lock().unwrap().next() };
                let Some((name, dst)) = job else { break };
                let src = ["Lib", "lib"]
                    .iter()
                    .map(|e| data.join(format!("{name}.{e}")))
                    .find(|p| p.exists());
                let Some(src) = src else {
                    failed.lock().unwrap().push(format!("{name} (缺源)"));
                    continue;
                };
                match convert_one(&src, &packs.join(&dst)) {
                    Ok((ok, _)) => {
                        let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        println!("[{n}] {name} → {dst} ({ok} 实帧)");
                    }
                    Err(e) => failed.lock().unwrap().push(format!("{name}: {e}")),
                }
            });
        }
    });
    let failed = failed.into_inner().unwrap();
    println!(
        "转码完成: 成功 {}, 失败 {}",
        done.load(std::sync::atomic::Ordering::Relaxed),
        failed.len()
    );
    for f in &failed {
        eprintln!("  失败: {f}");
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("{} 个库转码失败", failed.len()).into())
    }
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
    let mut canvas =
        image::RgbaImage::from_pixel(cols * cell_w, rows * cell_h, image::Rgba([40, 34, 28, 255]));
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
