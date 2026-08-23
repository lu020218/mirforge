//! 本地冒烟工具: 对真实资源文件/目录跑解码器 (样本不入库)。
//! 用法: cargo run -p mir-formats --example scan -- <file.map | file.Lib | 资源目录>

use std::fs;
use std::path::Path;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("用法: scan <file.map|file.Lib|目录>");
    if Path::new(&path).is_dir() {
        let t = std::time::Instant::now();
        let idx = mir_formats::scan::ResourceIndex::scan(Path::new(&path));
        let sup = idx.libs.iter().filter(|l| l.supported).count();
        println!(
            "{path}: 地图 {} 张, 图库 {} 个 (可解码 {sup}), 未识别 {} — {:?}",
            idx.maps.len(),
            idx.libs.len(),
            idx.unknown.len(),
            t.elapsed()
        );
        for m in idx.maps.iter().take(5) {
            println!(
                "  例: {:?} {:?} {}x{}",
                m.path.file_name().unwrap(),
                m.kind,
                m.width,
                m.height
            );
        }
        for (p, why) in idx.unknown.iter().take(5) {
            println!("  未识别: {:?} ({why})", p.file_name().unwrap());
        }
        return;
    }
    let data = fs::read(&path).expect("读文件失败");
    let lower = path.to_lowercase();
    if lower.ends_with(".map") {
        match mir_formats::map::parse(&data) {
            Ok(m) => {
                let blocked = m.cells.iter().filter(|c| c.blocked).count();
                let back = m.cells.iter().filter(|c| c.back >= 0).count();
                let front = m.cells.iter().filter(|c| c.front >= 0).count();
                println!(
                    "{path}: {:?} {}x{} 阻挡 {blocked}/{} back {back} front {front}",
                    m.kind,
                    m.width,
                    m.height,
                    m.cells.len()
                );
            }
            Err(e) => println!("{path}: 解析失败: {e}"),
        }
    } else {
        match mir_formats::crystal_lib::CrystalLib::parse(data) {
            Ok(lib) => {
                let mut decoded = 0usize;
                let mut failed = 0usize;
                let mut empty = 0usize;
                let probe = lib.len().min(500);
                for i in 0..probe {
                    match lib.image(i) {
                        Ok(Some(_)) => decoded += 1,
                        Ok(None) => empty += 1,
                        Err(_) => failed += 1,
                    }
                }
                println!(
                    "{path}: v{} 共 {} 帧; 前 {probe} 帧: 解码 {decoded} 空 {empty} 失败 {failed}",
                    lib.version,
                    lib.len()
                );
            }
            Err(e) => println!("{path}: 解析失败: {e}"),
        }
    }
}
