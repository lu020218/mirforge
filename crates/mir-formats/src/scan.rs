//! 资源目录扫描器（开发计划 M0 任务 0.6）。
//!
//! 给定用户的传奇资源根目录，产出归一化清单：地图（格式/尺寸）与图库（格式/帧数）。
//! 原则：**能识别多少读多少**——不认识或损坏的文件记入 `unknown`，绝不让扫描崩溃。
//! 只读文件头，不做全量解码，大目录也能秒级完成。

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::map::{self, MapKind};

/// 图库格式类别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibKind {
    CrystalLib,
    /// WIL/WIX（解码器未实现，仍会被清单识别）
    Wil,
    /// WZL/WZX（解码器未实现，仍会被清单识别）
    Wzl,
}

#[derive(Debug, Clone)]
pub struct MapEntry {
    pub path: PathBuf,
    pub kind: MapKind,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone)]
pub struct LibEntry {
    pub path: PathBuf,
    pub kind: LibKind,
    /// 帧数（WIL/WZL 在解码器落地前为 0）
    pub frames: u32,
    /// 对应解码器是否已实现
    pub supported: bool,
}

/// 扫描产出的归一化资源清单
#[derive(Debug, Default)]
pub struct ResourceIndex {
    pub maps: Vec<MapEntry>,
    pub libs: Vec<LibEntry>,
    /// 后缀匹配但无法识别/读取失败的文件（路径 + 原因）
    pub unknown: Vec<(PathBuf, String)>,
}

impl ResourceIndex {
    /// 递归扫描目录。深度与文件数不设上限，但每文件只读头部。
    pub fn scan(root: &Path) -> Self {
        let mut out = Self::default();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                out.classify(&p);
            }
        }
        // 稳定输出顺序 (与文件系统遍历顺序解耦)
        out.maps.sort_by(|a, b| a.path.cmp(&b.path));
        out.libs.sort_by(|a, b| a.path.cmp(&b.path));
        out.unknown.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn classify(&mut self, p: &Path) {
        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "map" => match read_head(p, 64) {
                Ok(head) => match map::detect(&head) {
                    Some(kind) => {
                        let (w, h) = head_dims(&head, kind);
                        self.maps.push(MapEntry {
                            path: p.to_path_buf(),
                            kind,
                            width: w,
                            height: h,
                        });
                    }
                    None => self
                        .unknown
                        .push((p.to_path_buf(), "无法识别的地图头".into())),
                },
                Err(e) => self.unknown.push((p.to_path_buf(), e)),
            },
            "lib" => match read_head(p, 12) {
                Ok(head) if head.len() >= 8 => {
                    let version = i32::from_le_bytes(head[0..4].try_into().unwrap());
                    let count = i32::from_le_bytes(head[4..8].try_into().unwrap());
                    if (1..=10).contains(&version) && (0..=2_000_000).contains(&count) {
                        self.libs.push(LibEntry {
                            path: p.to_path_buf(),
                            kind: LibKind::CrystalLib,
                            frames: count as u32,
                            supported: true,
                        });
                    } else {
                        self.unknown
                            .push((p.to_path_buf(), "无法识别的 .Lib 头".into()));
                    }
                }
                Ok(_) => self.unknown.push((p.to_path_buf(), "文件过短".into())),
                Err(e) => self.unknown.push((p.to_path_buf(), e)),
            },
            "wil" => self.libs.push(LibEntry {
                path: p.to_path_buf(),
                kind: LibKind::Wil,
                frames: 0,
                supported: false, // M0 任务 0.4 落地后翻转
            }),
            "wzl" => self.libs.push(LibEntry {
                path: p.to_path_buf(),
                kind: LibKind::Wzl,
                frames: 0,
                supported: false, // M0 任务 0.5 落地后翻转
            }),
            _ => {} // 其余文件 (wix/wzx 索引随主文件处理; 无关文件忽略)
        }
    }
}

fn read_head(p: &Path, n: usize) -> Result<Vec<u8>, String> {
    let mut f = fs::File::open(p).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; n];
    let mut read = 0;
    while read < n {
        match f.read(&mut buf[read..]) {
            Ok(0) => break,
            Ok(k) => read += k,
            Err(e) => return Err(e.to_string()),
        }
    }
    buf.truncate(read);
    Ok(buf)
}

fn head_dims(head: &[u8], kind: MapKind) -> (u32, u32) {
    let u16at = |i: usize| u16::from_le_bytes([head[i], head[i + 1]]);
    match kind {
        MapKind::Type0 | MapKind::Type2 | MapKind::Type3 => (u16at(0) as u32, u16at(2) as u32),
        MapKind::Type1 => {
            let xor = u16at(23);
            ((u16at(21) ^ xor) as u32, (u16at(25) ^ xor) as u32)
        }
        MapKind::Type4 => {
            let xor = u16at(33);
            ((u16at(31) ^ xor) as u32, (u16at(35) ^ xor) as u32)
        }
        MapKind::Type5 => (u16at(22) as u32, u16at(24) as u32),
        MapKind::Type6 => (u16at(16) as u32, u16at(18) as u32),
        MapKind::Type7 => (u16at(21) as u32, u16at(25) as u32),
        MapKind::Type100 => (u16at(4) as u32, u16at(6) as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, data: &[u8]) {
        fs::write(dir.join(name), data).unwrap();
    }

    #[test]
    fn scan_mixed_dir() {
        let tmp = std::env::temp_dir().join(format!("mirforge-scan-{}", std::process::id()));
        let sub = tmp.join("Data");
        fs::create_dir_all(&sub).unwrap();

        // type0 地图 2×3
        let mut m0 = vec![0u8; 52 + 6 * 12];
        m0[0..2].copy_from_slice(&2u16.to_le_bytes());
        m0[2..4].copy_from_slice(&3u16.to_le_bytes());
        write(&tmp, "a.map", &m0);
        // 损坏地图
        write(&tmp, "bad.map", &[0xFF; 8]);
        // Crystal lib v2, 5 帧
        let mut lib = Vec::new();
        lib.extend_from_slice(&2i32.to_le_bytes());
        lib.extend_from_slice(&5i32.to_le_bytes());
        lib.extend_from_slice(&[0u8; 20]);
        write(&sub, "Tiles.Lib", &lib);
        // WIL (未实现仍入清单)
        write(&sub, "Hum.wil", &[0u8; 32]);
        // 无关文件
        write(&tmp, "readme.txt", b"hi");

        let idx = ResourceIndex::scan(&tmp);
        assert_eq!(idx.maps.len(), 1);
        assert_eq!((idx.maps[0].width, idx.maps[0].height), (2, 3));
        assert_eq!(idx.libs.len(), 2);
        let cl = idx
            .libs
            .iter()
            .find(|l| l.kind == LibKind::CrystalLib)
            .unwrap();
        assert_eq!(cl.frames, 5);
        assert!(cl.supported);
        let wil = idx.libs.iter().find(|l| l.kind == LibKind::Wil).unwrap();
        assert!(!wil.supported);
        assert_eq!(idx.unknown.len(), 1); // bad.map

        fs::remove_dir_all(&tmp).ok();
    }
}
