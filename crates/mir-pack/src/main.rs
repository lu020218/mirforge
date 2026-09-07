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
        Some("mapinfo") if args.len() == 2 => mapinfo(Path::new(&args[1])),
        Some("import-maps") if args.len() == 3 => {
            import_maps(Path::new(&args[1]), Path::new(&args[2]))
        }
        Some("pack-wil") if args.len() == 3 => pack_wil(Path::new(&args[1]), Path::new(&args[2])),
        Some("pack-wzl") if args.len() == 3 => pack_wzl(Path::new(&args[1]), Path::new(&args[2])),
        Some("make-manifest") if args.len() >= 2 => make_manifest(
            Path::new(&args[1]),
            args.get(2).map(|s| s.as_str()).unwrap_or("0.1.0"),
        ),
        Some("remap") if args.len() >= 4 => remap(
            Path::new(&args[1]),
            Path::new(&args[2]),
            &args[3..].iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        ),
        Some("extract") if args.len() == 4 => extract(
            Path::new(&args[1]),
            args[2].parse().unwrap_or(0),
            Path::new(&args[3]),
        ),
        Some("pack-split") if args.len() == 4 => {
            let stride: usize = args[3].parse().map_err(|_| "跨度须为整数").unwrap_or(60);
            pack_split(Path::new(&args[1]), Path::new(&args[2]), stride)
        }
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

/// 收集目录下的 NNNNN.PNG (大小写/位数不限), 返回 (帧号, 原文件名主干, 路径)
fn scan_frames(dir: &Path) -> Result<Vec<(usize, String, PathBuf)>, AnyErr> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let p = entry?.path();
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let is_img = p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("png") || e.eq_ignore_ascii_case("bmp"));
        if !is_img {
            continue;
        }
        if let Ok(idx) = stem.parse::<usize>() {
            out.push((idx, stem.to_string(), p));
        }
    }
    out.sort_by_key(|(i, _, _)| *i);
    Ok(out)
}

/// 读 Placements/<主干>.txt: 两行整数 = X/Y 偏移; 缺失按 (0,0)
/// (不同素材包文件名位数不同 — 5 位/6 位都有, 按原主干找)
fn placement(dir: &Path, stem: &str) -> (i16, i16) {
    let p = dir.join("Placements").join(format!("{stem}.txt"));
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
    for (idx, stem, path) in files {
        // 素材包里空帧常是 0 字节占位 PNG, 解不开的一律按空帧
        let Ok(img) = image::open(&path) else {
            bad += 1;
            continue;
        };
        let mut rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 {
            bad += 1;
            continue;
        }
        // BMP 无 alpha 通道, 老图库约定纯黑为背景 → 抠成透明
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("bmp"))
        {
            for px in rgba.pixels_mut() {
                if px.0[0] == 0 && px.0[1] == 0 && px.0[2] == 0 {
                    px.0[3] = 0;
                }
            }
        }
        let (x, y) = placement(src, &stem);
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

/// 整库帧目录按固定跨度切成编号库: 帧 n → 第 n/stride 号库的第 n%stride 帧。
/// 市售 NPC 合集常见形态 (每个 NPC 60 帧一段) → npc/000.mfl 起
fn pack_split(src: &Path, outdir: &Path, stride: usize) -> Result<(), AnyErr> {
    let files = scan_frames(src)?;
    if files.is_empty() || stride == 0 {
        return Err("没有帧或跨度为 0".into());
    }
    let blocks = files.last().unwrap().0 / stride + 1;
    let mut per: Vec<Vec<Option<MflFrame>>> = (0..blocks)
        .map(|_| (0..stride).map(|_| None).collect())
        .collect();
    for (idx, stem, path) in files {
        let Ok(img) = image::open(&path) else {
            continue;
        };
        let mut rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 {
            continue;
        }
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("bmp"))
        {
            for px in rgba.pixels_mut() {
                if px.0[0] == 0 && px.0[1] == 0 && px.0[2] == 0 {
                    px.0[3] = 0;
                }
            }
        }
        let (x, y) = placement(src, &stem);
        per[idx / stride][idx % stride] = Some(MflFrame {
            width: w as u16,
            height: h as u16,
            offset_x: x,
            offset_y: y,
            rgba: rgba.into_raw(),
        });
    }
    std::fs::create_dir_all(outdir)?;
    let mut written = 0usize;
    for (n, frames) in per.iter().enumerate() {
        if frames.iter().all(|f| f.is_none()) {
            continue;
        }
        std::fs::write(outdir.join(format!("{n:03}.mfl")), mfl::write(frames)?)?;
        written += 1;
    }
    println!(
        "切分完成: {blocks} 段 × {stride} 帧, 写出 {written} 个库 → {}",
        outdir.display()
    );
    Ok(())
}

/// 生成游戏更新清单: 扫描发布目录, 逐文件记 路径/大小/SHA256 →
/// <目录>/manifest.json。登录器对照该清单做按文件增量更新
fn make_manifest(dir: &Path, version: &str) -> Result<(), AnyErr> {
    use sha2::{Digest, Sha256};
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, u64, String)>) -> Result<(), AnyErr> {
        for e in std::fs::read_dir(dir)? {
            let p = e?.path();
            if p.is_dir() {
                walk(&p, base, out)?;
                continue;
            }
            let rel = p.strip_prefix(base)?.to_string_lossy().replace('\\', "/");
            if rel == "manifest.json" {
                continue;
            }
            let bytes = std::fs::read(&p)?;
            let hash = format!("{:x}", Sha256::digest(&bytes));
            out.push((rel, bytes.len() as u64, hash));
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files)?;
    files.sort();
    let entries: Vec<String> = files
        .iter()
        .map(|(p, sz, h)| {
            format!("    {{ \"path\": \"{p}\", \"size\": {sz}, \"sha256\": \"{h}\" }}")
        })
        .collect();
    let json = format!(
        "{{
  \"version\": \"{version}\",
  \"files\": [
{}
  ]
}}
",
        entries.join(
            ",
"
        )
    );
    std::fs::write(dir.join("manifest.json"), &json)?;
    println!(
        "清单生成: {} 个文件, 版本 {version} → {}",
        files.len(),
        dir.join("manifest.json").display()
    );
    Ok(())
}

/// 段搬运: 从源库抽帧段拼新库 (单技能标准文件等重组用)。
/// spec 形如 `dst=src:count`, 帧原样转存 (含锚点), 目标帧位自动撑到最大
fn remap(src: &Path, out: &Path, specs: &[&str]) -> Result<(), AnyErr> {
    let lib = mfl::AnyLib::open(src)?;
    let mut moves: Vec<(usize, usize, usize)> = Vec::new();
    let mut pad = 0usize;
    for sp in specs {
        // pad=N: 帧位数至少撑到 N (统一规格用)
        if let Some(n) = sp.strip_prefix("pad=") {
            pad = n.parse()?;
            continue;
        }
        let (dst, rest) = sp
            .split_once('=')
            .ok_or("spec 应为 dst=src:count 或 pad=N")?;
        let (sb, cnt) = rest
            .split_once(':')
            .ok_or("spec 应为 dst=src:count 或 pad=N")?;
        moves.push((dst.parse()?, sb.parse()?, cnt.parse()?));
    }
    let total = moves
        .iter()
        .map(|(d, _, c)| d + c)
        .max()
        .unwrap_or(0)
        .max(pad);
    let mut frames: Vec<Option<MflFrame>> = Vec::new();
    frames.resize_with(total, || None);
    let mut ok = 0usize;
    for (dst, sb, cnt) in moves {
        for k in 0..cnt {
            if let Some(img) = lib.image(sb + k).ok().flatten() {
                frames[dst + k] = Some(MflFrame {
                    width: img.width,
                    height: img.height,
                    offset_x: img.offset_x,
                    offset_y: img.offset_y,
                    rgba: img.rgba,
                });
                ok += 1;
            }
        }
    }
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(out, mfl::write(&frames)?)?;
    println!("重组完成: {} 帧位, 实帧 {ok} → {}", total, out.display());
    Ok(())
}

/// WZL/WZX → .mfl (同名 .wzx 自动定位, 大小写不限)
fn pack_wzl(src: &Path, out: &Path) -> Result<(), AnyErr> {
    let wzx = ["wzx", "WZX", "Wzx"]
        .iter()
        .map(|e| src.with_extension(e))
        .find(|p| p.exists())
        .ok_or_else(|| format!("找不到 {} 的 .wzx 索引", src.display()))?;
    let lib = mir_formats::wzl::WzlLib::parse(std::fs::read(src)?, std::fs::read(wzx)?)?;
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
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let bytes = w.finish();
    std::fs::write(out, &bytes)?;
    println!(
        "打包完成: {} 帧位, 实帧 {ok}, 空帧 {empty}, {} KB → {}",
        lib.len(),
        bytes.len() / 1024,
        out.display()
    );
    Ok(())
}

/// 抽单帧存 PNG (做图标/核对帧号用)
fn extract(path: &Path, frame: usize, out: &Path) -> Result<(), AnyErr> {
    let lib = MflLib::parse(std::fs::read(path)?)?;
    let img = lib
        .image(frame)?
        .ok_or_else(|| format!("帧 {frame} 为空"))?;
    let buf = image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.rgba)
        .ok_or("尺寸不符")?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    buf.save(out)?;
    println!(
        "帧 {frame} ({}x{}) → {}",
        img.width,
        img.height,
        out.display()
    );
    Ok(())
}

/// WIL/WIX → .mfl (同名 .wix 自动定位, 大小写不限)
fn pack_wil(src: &Path, out: &Path) -> Result<(), AnyErr> {
    let wix = ["wix", "WIX", "Wix"]
        .iter()
        .map(|e| src.with_extension(e))
        .find(|p| p.exists())
        .ok_or_else(|| format!("找不到 {} 的 .wix 索引", src.display()))?;
    let lib = mir_formats::wil::WilLib::parse(std::fs::read(src)?, std::fs::read(wix)?)?;
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
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let bytes = w.finish();
    std::fs::write(out, &bytes)?;
    println!(
        "打包完成: {} 帧位, 实帧 {ok}, 空帧 {empty}, {} KB → {}",
        lib.len(),
        bytes.len() / 1024,
        out.display()
    );
    Ok(())
}

/// 把源目录里的全部 .map 地图文件收进 packs/map/ (引擎唯一的地图来源)。
/// 同名只收第一份并告警; 跳过 Placements 目录
fn import_maps(src: &Path, packs: &Path) -> Result<(), AnyErr> {
    let dst_dir = packs.join("map");
    std::fs::create_dir_all(&dst_dir)?;
    let mut seen = std::collections::HashSet::new();
    let (mut copied, mut skipped) = (0usize, 0usize);
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if e.file_name().to_str() != Some("Placements") {
                    stack.push(p);
                }
                continue;
            }
            let is_map = p
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| x.eq_ignore_ascii_case("map"));
            if !is_map {
                continue;
            }
            let name = p.file_name().unwrap().to_string_lossy().to_lowercase();
            if !seen.insert(name.clone()) {
                eprintln!("重名跳过: {} (已收过同名地图)", p.display());
                skipped += 1;
                continue;
            }
            std::fs::copy(&p, dst_dir.join(p.file_name().unwrap()))?;
            println!("收入 {} ← {}", name, p.display());
            copied += 1;
        }
    }
    println!("完成: 收入 {copied} 张地图, 重名跳过 {skipped}");
    Ok(())
}

/// 地图诊断: 尺寸 + 三层库号直方图 (接入新地图时先看引用了哪些图库)
fn mapinfo(path: &Path) -> Result<(), AnyErr> {
    let map = mir_formats::map::parse(&std::fs::read(path)?)?;
    println!("{}: {}x{} 格", path.display(), map.width, map.height);
    let mut hist: std::collections::BTreeMap<(&str, i16), usize> = Default::default();
    for y in 0..map.height {
        for x in 0..map.width {
            let Some(c) = map.cell(x, y) else { continue };
            if c.back >= 0 {
                *hist.entry(("back", c.back_lib)).or_default() += 1;
            }
            if c.mid >= 0 {
                *hist.entry(("mid", c.mid_lib)).or_default() += 1;
            }
            if c.front >= 0 {
                *hist.entry(("front", c.front_lib)).or_default() += 1;
            }
        }
    }
    for ((layer, lib), n) in hist {
        println!("  {layer} 库 {lib}: {n} 格");
    }
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
