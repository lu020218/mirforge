//! 传奇 `.map` 地图解析。
//!
//! 支持全部 9 种存量格式。格式布局参考 Crystal 项目（Suprcode/mir2-mapeditor
//! `MapCode.cs`，社区多年实战验证）重新实现；检测顺序与 Crystal 完全一致：
//!
//! | 类型 | 来源 | 检测特征 | 头 | 格字节 |
//! |---|---|---|---|---|
//! | 100 | Crystal 自有 | `bytes[2..4] == "C#"`（ver=1） | 8B | 26 |
//! | 5 | Wemade Mir3 | `bytes[0] == 0` | 28B+ | 分段 |
//! | 6 | Shanda Mir3 | `0x0F@0, 'S'@5, '3'@14` | 40B | 20 |
//! | 4 | Wemade 反外挂 | `0x15@0,'2'@4,'A'@6,'1'@19` | 64B | 12（XOR） |
//! | 1 | "Map 2010" | `0x10@0,'a'@2,'1'@7,'1'@14` | 54B | 15（XOR） |
//! | 2 | Shanda 旧 | `0x0F@4, 0D0A@18` 且文件短 | 52B | 14 |
//! | 3 | Shanda 2012 | 同上且文件长 | 52B | 36 |
//! | 7 | 3/4 Heroes | `0x0D@0,'L'@1,' '@7,'m'@11` | 54B | 15（无 XOR） |
//! | 0 | 经典 Wemade | 兜底 | 52B | 12 |
//!
//! 统一后的语义：
//! - 图号从 1 起（0 = 无图），本解析器输出 0 基帧索引（-1 = 无）；
//! - 阻挡 = `backRaw & 0x2000_0000` 或 `frontRaw & 0x8000`（各格式在解析期归一到该约定，
//!   含 Mir3 的 flag 位与 16 位格式的 0x8000 高位转换）；
//! - 库号（`*_lib`）沿用 Crystal 的跨格式登记：0=Tiles 1=SmTiles 2+=Objects{n-1}，
//!   Shanda 老图 +100/+110/+120，Wemade Mir3 +200，Shanda Mir3 +300。

use crate::{FormatError, Result};

/// 地图格式类别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapKind {
    Type0,
    Type1,
    Type2,
    Type3,
    Type4,
    Type5,
    Type6,
    Type7,
    Type100,
}

/// 解析后的单元格（行主序存放于 [`MirMap::cells`]）
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cell {
    /// 背景大砖（96×64，仅偶数格有效）帧索引；-1 = 无
    pub back: i32,
    pub back_lib: i16,
    /// 中间层帧索引；-1 = 无
    pub mid: i32,
    pub mid_lib: i16,
    /// 前景物件帧索引；-1 = 无
    pub front: i32,
    pub front_lib: i16,
    /// 该格是否阻挡（不可行走）
    pub blocked: bool,
    pub door_index: u8,
    pub door_offset: u8,
    pub ani_frame: u8,
    pub ani_tick: u8,
    pub light: u8,
}

/// 解析后的地图
#[derive(Debug, Clone)]
pub struct MirMap {
    pub kind: MapKind,
    pub width: u32,
    pub height: u32,
    /// 行主序：`cells[y * width + x]`
    pub cells: Vec<Cell>,
}

impl MirMap {
    #[inline]
    pub fn cell(&self, x: u32, y: u32) -> Option<&Cell> {
        if x < self.width && y < self.height {
            self.cells.get((y * self.width + x) as usize)
        } else {
            None
        }
    }
}

/// 探测地图格式（Crystal 检测顺序）。头部至少给 20 字节，64 字节更可靠。
pub fn detect(head: &[u8]) -> Option<MapKind> {
    if head.len() < 20 {
        return None;
    }
    if head[2] == 0x43 && head[3] == 0x23 {
        return Some(MapKind::Type100); // "C#" 标记
    }
    if head[0] == 0 {
        return Some(MapKind::Type5); // Wemade Mir3: 无标题, 起始空字节
    }
    if head[0] == 0x0F && head[5] == 0x53 && head[14] == 0x33 {
        return Some(MapKind::Type6); // "(C) SNDA, MIR3"
    }
    if head[0] == 0x15 && head[4] == 0x32 && head[6] == 0x41 && head[19] == 0x31 {
        return Some(MapKind::Type4); // "Mir2 AntiHack"
    }
    if head[0] == 0x10 && head[2] == 0x61 && head[7] == 0x31 && head[14] == 0x31 {
        return Some(MapKind::Type1); // "Map 2010 Ver 1.0"
    }
    if head[4] == 0x0F && head[18] == 0x0D && head[19] == 0x0A {
        return Some(MapKind::Type2); // Shanda 旧/2012 (2 与 3 按文件长度在 parse 时区分)
    }
    if head[0] == 0x0D && head[1] == 0x4C && head[7] == 0x20 && head[11] == 0x6D {
        return Some(MapKind::Type7); // 3/4 Heroes
    }
    // 兜底: 经典 type0 (宽高需合法)
    let w = i16::from_le_bytes([head[0], head[1]]);
    let h = i16::from_le_bytes([head[2], head[3]]);
    if w > 0 && h > 0 {
        return Some(MapKind::Type0);
    }
    None
}

/// 解析 `.map` 文件字节。
pub fn parse(data: &[u8]) -> Result<MirMap> {
    let kind = detect(data).ok_or(FormatError::Unrecognized("map header"))?;
    match kind {
        MapKind::Type0 => parse_type0(data),
        MapKind::Type1 => parse_type1(data),
        MapKind::Type2 | MapKind::Type3 => parse_type23(data),
        MapKind::Type4 => parse_type4(data),
        MapKind::Type5 => parse_type5(data),
        MapKind::Type6 => parse_type6(data),
        MapKind::Type7 => parse_type7(data),
        MapKind::Type100 => parse_type100(data),
    }
}

// ─────────── 工具 ───────────

fn dims_checked(w: i64, h: i64) -> Result<(u32, u32)> {
    if !(1..=4096).contains(&w) || !(1..=4096).contains(&h) {
        return Err(FormatError::BadDimensions(format!("{w}x{h}")));
    }
    Ok((w as u32, h as u32))
}

fn need(data: &[u8], n: usize) -> Result<()> {
    if data.len() < n {
        return Err(FormatError::Truncated {
            need: n,
            got: data.len(),
        });
    }
    Ok(())
}

#[inline]
fn u16le(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
#[inline]
fn i16le(d: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([d[o], d[o + 1]])
}
#[inline]
fn u32le(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

/// 16 位图号 → (帧索引, 阻挡位)。高位 0x8000 = 阻挡；图号 1 基 → 0 基
#[inline]
fn idx16(raw: u16) -> (i32, bool) {
    let blocked = raw & 0x8000 != 0;
    let v = (raw & 0x7FFF) as i32;
    (if v == 0 { -1 } else { v - 1 }, blocked)
}

/// 32 位图号 → (帧索引, 阻挡位)。0x2000_0000 = 阻挡；掩码低 29 位；1 基 → 0 基
#[inline]
fn idx32(raw: u32) -> (i32, bool) {
    let blocked = raw & 0x2000_0000 != 0;
    let v = (raw & 0x1FFF_FFFF) as i32;
    (if v == 0 { -1 } else { v - 1 }, blocked)
}

/// Mir3 系图号: 存储值 +1 后 0 表示无 → 帧索引
#[inline]
fn idx_mir3(stored_plus1: i32) -> i32 {
    if stored_plus1 <= 0 {
        -1
    } else {
        stored_plus1 - 1
    }
}

// ─────────── type0: 经典 Wemade (52B 头, 12B/格) ───────────

fn parse_type0(data: &[u8]) -> Result<MirMap> {
    need(data, 52)?;
    let (w, h) = dims_checked(i16le(data, 0) as i64, i16le(data, 2) as i64)?;
    let n = (w * h) as usize;
    need(data, 52 + n * 12)?;
    let mut cells = vec![Cell::default(); n];
    for x in 0..w {
        for y in 0..h {
            let o = 52 + ((x * h + y) as usize) * 12;
            let c = &mut cells[(y * w + x) as usize];
            let (back, bb) = idx16(u16le(data, o));
            let (mid, _) = idx16(u16le(data, o + 2));
            let (front, fb) = idx16(u16le(data, o + 4));
            c.back = back;
            c.mid = mid;
            c.mid_lib = 1;
            c.front = front;
            c.front_lib = data[o + 10] as i16 + 2;
            c.blocked = bb || fb;
            c.door_index = data[o + 6];
            c.door_offset = data[o + 7];
            c.ani_frame = data[o + 8];
            c.ani_tick = data[o + 9];
            c.light = data[o + 11];
        }
    }
    Ok(MirMap {
        kind: MapKind::Type0,
        width: w,
        height: h,
        cells,
    })
}

// ─────────── type1: "Map 2010" XOR 加密 (54B 头, 15B/格) ───────────

fn parse_type1(data: &[u8]) -> Result<MirMap> {
    need(data, 54)?;
    let xor = u16le(data, 23);
    let (w, h) = dims_checked(
        (u16le(data, 21) ^ xor) as i64,
        (u16le(data, 25) ^ xor) as i64,
    )?;
    let n = (w * h) as usize;
    need(data, 54 + n * 15)?;
    let mut cells = vec![Cell::default(); n];
    for x in 0..w {
        for y in 0..h {
            let o = 54 + ((x * h + y) as usize) * 15;
            let c = &mut cells[(y * w + x) as usize];
            let (back, bb) = idx32(u32le(data, o) ^ 0xAA38_AA38);
            let (mid, _) = idx16(u16le(data, o + 4) ^ xor);
            let (front, fb) = idx16(u16le(data, o + 6) ^ xor);
            c.back = back;
            c.mid = mid;
            c.mid_lib = 1;
            c.front = front;
            let mut fl = data[o + 12] as i16 + 2;
            if fl == 102 {
                fl = 90; // Crystal 实测怪癖: 库号 102 实为 90
            }
            c.front_lib = fl;
            c.blocked = bb || fb;
            c.door_index = data[o + 8];
            c.door_offset = data[o + 9];
            c.ani_frame = data[o + 10];
            c.ani_tick = data[o + 11];
            c.light = data[o + 13];
        }
    }
    Ok(MirMap {
        kind: MapKind::Type1,
        width: w,
        height: h,
        cells,
    })
}

// ─────────── type2/3: Shanda 旧 (14B/格) 与 2012 (36B/格), 52B 头 ───────────

fn parse_type23(data: &[u8]) -> Result<MirMap> {
    need(data, 52)?;
    let (w, h) = dims_checked(i16le(data, 0) as i64, i16le(data, 2) as i64)?;
    let n = (w * h) as usize;
    // 两格式头相同, 按文件长度区分 (Crystal 同法)
    let (kind, stride) = if data.len() > 52 + n * 14 {
        (MapKind::Type3, 36)
    } else {
        (MapKind::Type2, 14)
    };
    need(data, 52 + n * stride)?;
    let mut cells = vec![Cell::default(); n];
    for x in 0..w {
        for y in 0..h {
            let o = 52 + ((x * h + y) as usize) * stride;
            let c = &mut cells[(y * w + x) as usize];
            let (back, bb) = idx16(u16le(data, o));
            let (mid, _) = idx16(u16le(data, o + 2));
            let (front, fb) = idx16(u16le(data, o + 4));
            c.back = back;
            c.mid = mid;
            c.front = front;
            c.blocked = bb || fb;
            c.door_index = data[o + 6];
            c.door_offset = data[o + 7];
            c.ani_frame = data[o + 8];
            c.ani_tick = data[o + 9];
            // Shanda 库号登记: front +120, back +100, mid +110
            c.front_lib = data[o + 10] as i16 + 120;
            c.light = data[o + 11];
            c.back_lib = data[o + 12] as i16 + 100;
            c.mid_lib = data[o + 13] as i16 + 110;
            // type3 其余 22 字节为砖动画/光照混合参数, 暂不消费
        }
    }
    Ok(MirMap {
        kind,
        width: w,
        height: h,
        cells,
    })
}

// ─────────── type4: Wemade 反外挂 (64B 头, 12B/格, XOR) ───────────

fn parse_type4(data: &[u8]) -> Result<MirMap> {
    need(data, 64)?;
    let xor = u16le(data, 33);
    let (w, h) = dims_checked(
        (u16le(data, 31) ^ xor) as i64,
        (u16le(data, 35) ^ xor) as i64,
    )?;
    let n = (w * h) as usize;
    need(data, 64 + n * 12)?;
    let mut cells = vec![Cell::default(); n];
    for x in 0..w {
        for y in 0..h {
            let o = 64 + ((x * h + y) as usize) * 12;
            let c = &mut cells[(y * w + x) as usize];
            let (back, bb) = idx16(u16le(data, o) ^ xor);
            let (mid, _) = idx16(u16le(data, o + 2) ^ xor);
            let (front, fb) = idx16(u16le(data, o + 4) ^ xor);
            c.back = back;
            c.mid = mid;
            c.mid_lib = 1;
            c.front = front;
            c.front_lib = data[o + 10] as i16 + 2;
            c.blocked = bb || fb;
            c.door_index = data[o + 6];
            c.door_offset = data[o + 7];
            c.ani_frame = data[o + 8];
            c.ani_tick = data[o + 9];
            c.light = data[o + 11];
        }
    }
    Ok(MirMap {
        kind: MapKind::Type4,
        width: w,
        height: h,
        cells,
    })
}

// ─────────── type5: Wemade Mir3 (背景 2×2 分块段 + 逐格段) ───────────

fn parse_type5(data: &[u8]) -> Result<MirMap> {
    need(data, 28)?;
    let (w, h) = dims_checked(i16le(data, 22) as i64, i16le(data, 24) as i64)?;
    let n = (w * h) as usize;
    let blocks = ((w / 2 + w % 2) * (h / 2)) as usize;
    let cells_at = 28 + 3 * blocks;
    need(data, cells_at + n * 14)?;
    let mut cells = vec![Cell::default(); n];
    // 背景: 每 2×2 块 3 字节 (lib u8 + image i16), 值复制到块内 4 格
    for bx in 0..(w / 2) {
        for by in 0..(h / 2) {
            let o = 28 + ((bx * (h / 2) + by) as usize) * 3;
            let lib = data[o];
            let img = i16le(data, o + 1) as i32 + 1;
            for i in 0..4u32 {
                let (cx, cy) = (bx * 2 + i % 2, by * 2 + i / 2);
                let c = &mut cells[(cy * w + cx) as usize];
                c.back_lib = if lib != 255 { lib as i16 + 200 } else { -1 };
                c.back = idx_mir3(img);
            }
        }
    }
    for x in 0..w {
        for y in 0..h {
            let o = cells_at + ((x * h + y) as usize) * 14;
            let c = &mut cells[(y * w + x) as usize];
            let flag = data[o];
            c.ani_frame = if data[o + 2] == 255 {
                0
            } else {
                data[o + 2] & 0x8F
            };
            c.front_lib = if data[o + 3] != 255 {
                data[o + 3] as i16 + 200
            } else {
                -1
            };
            c.mid_lib = if data[o + 4] != 255 {
                data[o + 4] as i16 + 200
            } else {
                -1
            };
            c.mid = idx_mir3(i16le(data, o + 5) as i32 + 1);
            c.front = idx_mir3(i16le(data, o + 7) as i32 + 1);
            c.light = (data[o + 12] & 0x0F) * 4;
            // flag 位: bit0 未置 = 阻挡(背景), bit1 未置 = 阻挡(前景)
            c.blocked = (flag & 0x01) != 1 || (flag & 0x02) != 2;
        }
    }
    Ok(MirMap {
        kind: MapKind::Type5,
        width: w,
        height: h,
        cells,
    })
}

// ─────────── type6: Shanda Mir3 (40B 头, 20B/格) ───────────

fn parse_type6(data: &[u8]) -> Result<MirMap> {
    need(data, 40)?;
    let (w, h) = dims_checked(i16le(data, 16) as i64, i16le(data, 18) as i64)?;
    let n = (w * h) as usize;
    need(data, 40 + n * 20)?;
    let mut cells = vec![Cell::default(); n];
    for x in 0..w {
        for y in 0..h {
            let o = 40 + ((x * h + y) as usize) * 20;
            let c = &mut cells[(y * w + x) as usize];
            let flag = data[o];
            c.back_lib = if data[o + 1] != 255 {
                data[o + 1] as i16 + 300
            } else {
                -1
            };
            c.mid_lib = if data[o + 2] != 255 {
                data[o + 2] as i16 + 300
            } else {
                -1
            };
            c.front_lib = if data[o + 3] != 255 {
                data[o + 3] as i16 + 300
            } else {
                -1
            };
            c.back = idx_mir3(i16le(data, o + 4) as i32 + 1);
            c.mid = idx_mir3(i16le(data, o + 6) as i32 + 1);
            c.front = idx_mir3(i16le(data, o + 8) as i32 + 1);
            c.ani_frame = if data[o + 11] == 255 {
                0
            } else {
                data[o + 11] & 0x0F
            };
            c.light = (data[o + 12] & 0x0F) * 4;
            c.blocked = (flag & 0x01) != 1 || (flag & 0x02) != 2;
        }
    }
    Ok(MirMap {
        kind: MapKind::Type6,
        width: w,
        height: h,
        cells,
    })
}

// ─────────── type7: 3/4 Heroes (54B 头, 15B/格, 无 XOR) ───────────

fn parse_type7(data: &[u8]) -> Result<MirMap> {
    need(data, 54)?;
    let (w, h) = dims_checked(i16le(data, 21) as i64, i16le(data, 25) as i64)?;
    let n = (w * h) as usize;
    need(data, 54 + n * 15)?;
    let mut cells = vec![Cell::default(); n];
    for x in 0..w {
        for y in 0..h {
            let o = 54 + ((x * h + y) as usize) * 15;
            let c = &mut cells[(y * w + x) as usize];
            let (back, bb) = idx32(u32le(data, o));
            let (mid, _) = idx16(u16le(data, o + 4));
            let (front, fb) = idx16(u16le(data, o + 6));
            c.back = back;
            c.mid = mid;
            c.mid_lib = 1;
            c.front = front;
            c.front_lib = data[o + 12] as i16 + 2;
            c.blocked = bb || fb;
            c.door_index = data[o + 8];
            c.door_offset = data[o + 9];
            c.ani_frame = data[o + 10];
            c.ani_tick = data[o + 11];
            c.light = data[o + 13];
        }
    }
    Ok(MirMap {
        kind: MapKind::Type7,
        width: w,
        height: h,
        cells,
    })
}

// ─────────── type100: Crystal ("ver,C#,w,h" 8B 头, 26B/格) ───────────

fn parse_type100(data: &[u8]) -> Result<MirMap> {
    need(data, 8)?;
    if data[0] != 1 || data[1] != 0 {
        return Err(FormatError::Unrecognized("type100 版本 (仅支持 v1)"));
    }
    let (w, h) = dims_checked(i16le(data, 4) as i64, i16le(data, 6) as i64)?;
    let n = (w * h) as usize;
    need(data, 8 + n * 26)?;
    let mut cells = vec![Cell::default(); n];
    for x in 0..w {
        for y in 0..h {
            let o = 8 + ((x * h + y) as usize) * 26;
            let c = &mut cells[(y * w + x) as usize];
            c.back_lib = i16le(data, o);
            let (back, bb) = idx32(u32le(data, o + 2));
            c.mid_lib = i16le(data, o + 6);
            let (mid, _) = idx16(u16le(data, o + 8));
            c.front_lib = i16le(data, o + 10);
            let (front, fb) = idx16(u16le(data, o + 12));
            c.back = back;
            c.mid = mid;
            c.front = front;
            c.blocked = bb || fb;
            c.door_index = data[o + 14];
            c.door_offset = data[o + 15];
            c.ani_frame = data[o + 16];
            c.ani_tick = data[o + 17];
            c.light = data[o + 25];
        }
    }
    Ok(MirMap {
        kind: MapKind::Type100,
        width: w,
        height: h,
        cells,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成 type0 地图: 2×2, (1,0) 放前景且阻挡
    #[test]
    fn type0_roundtrip() {
        let (w, h) = (2u16, 2u16);
        let mut buf = vec![0u8; 52 + (w * h) as usize * 12];
        buf[0..2].copy_from_slice(&w.to_le_bytes());
        buf[2..4].copy_from_slice(&h.to_le_bytes());
        let o = 52 + 2 * 12; // 列主序 (x=1,y=0)
        buf[o..o + 2].copy_from_slice(&(0x8000u16 | 7).to_le_bytes());
        buf[o + 4..o + 6].copy_from_slice(&42u16.to_le_bytes());
        buf[o + 10] = 1;

        let m = parse(&buf).unwrap();
        assert_eq!(m.kind, MapKind::Type0);
        let c = m.cell(1, 0).unwrap();
        assert_eq!(c.back, 6); // 图号 7 → 帧 6
        assert_eq!(c.front, 41);
        assert_eq!(c.front_lib, 3);
        assert!(c.blocked);
        assert!(!m.cell(0, 0).unwrap().blocked);
        assert_eq!(m.cell(0, 0).unwrap().back, -1);
    }

    /// 合成 type100: "C#" 标记 + 阻挡位 + 索引掩码
    #[test]
    fn type100_blocked_and_mask() {
        let (w, h) = (3u16, 1u16);
        let mut buf = vec![0u8; 8 + (w * h) as usize * 26];
        buf[0] = 1;
        buf[2] = 0x43;
        buf[3] = 0x23;
        buf[4..6].copy_from_slice(&w.to_le_bytes());
        buf[6..8].copy_from_slice(&h.to_le_bytes());
        let o = 8 + 26; // (x=1,y=0)
        buf[o..o + 2].copy_from_slice(&5i16.to_le_bytes());
        buf[o + 2..o + 6].copy_from_slice(&(0x2000_0000u32 | 551).to_le_bytes());

        let m = parse(&buf).unwrap();
        assert_eq!(m.kind, MapKind::Type100);
        let c = m.cell(1, 0).unwrap();
        assert_eq!(c.back, 550);
        assert_eq!(c.back_lib, 5);
        assert!(c.blocked);
    }

    /// 合成 type1: 检测特征字节 + XOR 解密 + frontLib+2
    #[test]
    fn type1_xor_decrypt() {
        let (w, h, xor) = (2u16, 1u16, 0x5A5Au16);
        let mut buf = vec![0u8; 54 + (w * h) as usize * 15];
        // 检测特征: "Map 2010 Ver 1.0" pascal 串 (0x10@0 'a'@2 '1'@7 '1'@14)
        let title = b"Map 2010 Ver 1.0";
        buf[0] = title.len() as u8;
        buf[1..1 + title.len()].copy_from_slice(title);
        buf[23..25].copy_from_slice(&xor.to_le_bytes());
        buf[21..23].copy_from_slice(&(w ^ xor).to_le_bytes());
        buf[25..27].copy_from_slice(&(h ^ xor).to_le_bytes());
        let o = 54;
        buf[o..o + 4].copy_from_slice(&((0x2000_0000u32 | 100) ^ 0xAA38_AA38).to_le_bytes());
        buf[o + 6..o + 8].copy_from_slice(&(9u16 ^ xor).to_le_bytes());
        buf[o + 12] = 1;

        let m = parse(&buf).unwrap();
        assert_eq!(m.kind, MapKind::Type1);
        let c = m.cell(0, 0).unwrap();
        assert_eq!(c.back, 99);
        assert_eq!(c.front, 8);
        assert_eq!(c.front_lib, 3);
        assert!(c.blocked);
    }

    /// 合成 type2: Shanda 库号登记 (+100/+110/+120)
    #[test]
    fn type2_shanda_lib_registry() {
        let (w, h) = (1u16, 1u16);
        let mut buf = vec![0u8; 52 + 14];
        buf[0..2].copy_from_slice(&w.to_le_bytes());
        buf[2..4].copy_from_slice(&h.to_le_bytes());
        buf[4] = 0x0F;
        buf[18] = 0x0D;
        buf[19] = 0x0A;
        let o = 52;
        buf[o..o + 2].copy_from_slice(&3u16.to_le_bytes()); // back 图号 3
        buf[o + 10] = 5; // front lib 字节
        buf[o + 12] = 2; // back lib 字节
        buf[o + 13] = 1; // mid lib 字节

        let m = parse(&buf).unwrap();
        assert_eq!(m.kind, MapKind::Type2);
        let c = m.cell(0, 0).unwrap();
        assert_eq!(c.back, 2);
        assert_eq!(c.front_lib, 125);
        assert_eq!(c.back_lib, 102);
        assert_eq!(c.mid_lib, 111);
    }

    /// 合成 type6: Shanda Mir3 flag 位阻挡语义
    #[test]
    fn type6_flag_blocking() {
        let (w, h) = (2u16, 1u16);
        let mut buf = vec![0u8; 40 + 2 * 20];
        buf[0] = 0x0F;
        buf[5] = 0x53;
        buf[14] = 0x33;
        buf[16..18].copy_from_slice(&w.to_le_bytes());
        buf[18..20].copy_from_slice(&h.to_le_bytes());
        // (0,0): flag=0x03 可走, back_lib=4(+300), back 图号 8
        let o = 40;
        buf[o] = 0x03;
        buf[o + 1] = 4;
        buf[o + 2] = 255;
        buf[o + 3] = 255;
        buf[o + 4..o + 6].copy_from_slice(&7i16.to_le_bytes()); // +1 → 8 → 帧 7
                                                                // (1,0): flag=0x02 (bit0 未置 = 阻挡)
        let o2 = 40 + 20;
        buf[o2] = 0x02;
        buf[o2 + 1] = 255;
        buf[o2 + 2] = 255;
        buf[o2 + 3] = 255;

        let m = parse(&buf).unwrap();
        assert_eq!(m.kind, MapKind::Type6);
        let c = m.cell(0, 0).unwrap();
        assert_eq!(c.back_lib, 304);
        assert_eq!(c.back, 7);
        assert!(!c.blocked);
        assert!(m.cell(1, 0).unwrap().blocked);
        assert_eq!(m.cell(1, 0).unwrap().back_lib, -1);
    }

    #[test]
    fn reject_garbage() {
        assert!(parse(&[0xFFu8; 8]).is_err());
    }
}
