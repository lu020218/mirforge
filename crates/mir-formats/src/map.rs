//! 传奇 `.map` 地图解析。
//!
//! 支持三种存量格式（细节均在旧项目 JS 实现上对真实地图验证过，此处为 Rust 移植）：
//! - **type0**：经典 Wemade 格式。头 52B（宽 i16@0 / 高 i16@2），12B/格，列主序。
//! - **type1**："Map 2010" 加密格式。头带文本签名，宽=u16@21^u16@23、高=u16@25^u16@23，
//!   格子自 54B 起 15B/格：back i32^0xAA38AA38、mid/front i16^xor、frontLib=字节@12（值 +2）。
//! - **type100**：Crystal 格式（常见 "0.map"）。头 8B（ver i16@0=1 / 宽 i16@4 / 高 i16@6），
//!   26B/格，back 图号为 i32（高位 0x20000000 为阻挡标志，索引取 &0x1FFFFFFF）。
//!
//! 阻挡语义（游戏可走性）：
//! - type0：`back & 0x8000` 或 `front & 0x8000`
//! - type1/type100：`backRaw & 0x2000_0000` 或 `frontRaw & 0x8000`

use crate::{FormatError, Result};

/// 地图格式类别
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapKind {
    Type0,
    Type1,
    Type100,
}

/// 解析后的单元格（行主序存放于 [`MirMap::cells`]）
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cell {
    /// 背景大砖（96×64，仅偶数格有效）图号；-1 = 无
    pub back: i32,
    /// 背景库号（type100 有，其余 0）
    pub back_lib: i16,
    /// 中间层（48×32）图号；-1 = 无
    pub mid: i32,
    pub mid_lib: i16,
    /// 前景物件图号；-1 = 无
    pub front: i32,
    /// 前景库号（0=Tiles 1=SmTiles 2=Objects n=Objects{n-1}，与既有约定一致）
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

/// 探测地图格式（只需头部字节）。
pub fn detect(head: &[u8]) -> Option<MapKind> {
    if head.len() >= 8 && i16::from_le_bytes([head[0], head[1]]) == 1 {
        // type100 头: ver i16@0 == 1
        let w = i16::from_le_bytes([head[4], head[5]]);
        let h = i16::from_le_bytes([head[6], head[7]]);
        if w > 0 && h > 0 {
            return Some(MapKind::Type100);
        }
    }
    if head.len() >= 20 && head[..20].windows(8).any(|w| w == b"Map 2010") {
        return Some(MapKind::Type1);
    }
    if head.len() >= 4 {
        let w = i16::from_le_bytes([head[0], head[1]]);
        let h = i16::from_le_bytes([head[2], head[3]]);
        if w > 0 && h > 0 {
            return Some(MapKind::Type0);
        }
    }
    None
}

/// 解析 `.map` 文件字节。
pub fn parse(data: &[u8]) -> Result<MirMap> {
    let kind = detect(data).ok_or(FormatError::Unrecognized("map header"))?;
    match kind {
        MapKind::Type0 => parse_type0(data),
        MapKind::Type1 => parse_type1(data),
        MapKind::Type100 => parse_type100(data),
    }
}

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

fn parse_type0(data: &[u8]) -> Result<MirMap> {
    need(data, 52)?;
    let (w, h) = dims_checked(
        i16::from_le_bytes([data[0], data[1]]) as i64,
        i16::from_le_bytes([data[2], data[3]]) as i64,
    )?;
    let n = (w * h) as usize;
    need(data, 52 + n * 12)?;
    let mut cells = vec![Cell::default(); n];
    // 列主序: x 外层, y 内层
    for x in 0..w {
        for y in 0..h {
            let o = 52 + ((x * h + y) as usize) * 12;
            let back_raw = u16::from_le_bytes([data[o], data[o + 1]]);
            let mid_raw = u16::from_le_bytes([data[o + 2], data[o + 3]]);
            let front_raw = u16::from_le_bytes([data[o + 4], data[o + 5]]);
            let c = &mut cells[(y * w + x) as usize];
            c.back = idx16(back_raw);
            c.mid = idx16(mid_raw);
            c.front = idx16(front_raw);
            c.front_lib = 2 + data[o + 10] as i16; // Objects 库号: 字节值 + 基准 2
            c.blocked = back_raw & 0x8000 != 0 || front_raw & 0x8000 != 0;
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

fn parse_type1(data: &[u8]) -> Result<MirMap> {
    need(data, 54)?;
    let xor = u16::from_le_bytes([data[23], data[24]]);
    let (w, h) = dims_checked(
        (u16::from_le_bytes([data[21], data[22]]) ^ xor) as i64,
        (u16::from_le_bytes([data[25], data[26]]) ^ xor) as i64,
    )?;
    let n = (w * h) as usize;
    need(data, 54 + n * 15)?;
    let mut cells = vec![Cell::default(); n];
    for x in 0..w {
        for y in 0..h {
            let o = 54 + ((x * h + y) as usize) * 15;
            let back_raw =
                u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]) ^ 0xAA38_AA38;
            let mid_raw = u16::from_le_bytes([data[o + 4], data[o + 5]]) ^ xor;
            let front_raw = u16::from_le_bytes([data[o + 6], data[o + 7]]) ^ xor;
            let c = &mut cells[(y * w + x) as usize];
            c.back = idx32(back_raw);
            c.mid = idx16(mid_raw);
            c.front = idx16(front_raw);
            c.front_lib = data[o + 12] as i16 + 2; // 实测: 字节值需 +2 映射到 Objects 库
            c.blocked = back_raw & 0x2000_0000 != 0 || front_raw & 0x8000 != 0;
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

fn parse_type100(data: &[u8]) -> Result<MirMap> {
    need(data, 8)?;
    let (w, h) = dims_checked(
        i16::from_le_bytes([data[4], data[5]]) as i64,
        i16::from_le_bytes([data[6], data[7]]) as i64,
    )?;
    let n = (w * h) as usize;
    need(data, 8 + n * 26)?;
    let mut cells = vec![Cell::default(); n];
    for x in 0..w {
        for y in 0..h {
            let o = 8 + ((x * h + y) as usize) * 26;
            let back_lib = i16::from_le_bytes([data[o], data[o + 1]]);
            let back_raw = u32::from_le_bytes([data[o + 2], data[o + 3], data[o + 4], data[o + 5]]);
            let mid_lib = i16::from_le_bytes([data[o + 6], data[o + 7]]);
            let mid_raw = u16::from_le_bytes([data[o + 8], data[o + 9]]);
            let front_lib = i16::from_le_bytes([data[o + 10], data[o + 11]]);
            let front_raw = u16::from_le_bytes([data[o + 12], data[o + 13]]);
            let c = &mut cells[(y * w + x) as usize];
            c.back = idx32(back_raw);
            c.back_lib = back_lib;
            c.mid = idx16(mid_raw);
            c.mid_lib = mid_lib;
            c.front = idx16(front_raw);
            c.front_lib = front_lib;
            c.blocked = back_raw & 0x2000_0000 != 0 || front_raw & 0x8000 != 0;
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

/// u16 图号 → 索引（0x8000 为存在/阻挡标志位；0 = 无图）
#[inline]
fn idx16(raw: u16) -> i32 {
    let v = (raw & 0x7FFF) as i32;
    if v == 0 {
        -1
    } else {
        v - 1
    } // 图号从 1 起, 帧数组 0 基
}

/// u32 图号 → 索引（高位为标志位，索引取低 29 位；0 = 无图）
#[inline]
fn idx32(raw: u32) -> i32 {
    let v = (raw & 0x1FFF_FFFF) as i32;
    if v == 0 {
        -1
    } else {
        v - 1
    } // 同上: -1 到 0 基
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
        // 列主序: (x=1,y=0) 是第 (1*h+0)=2 个格
        let o = 52 + 2 * 12;
        buf[o..o + 2].copy_from_slice(&(0x8000u16 | 7).to_le_bytes()); // back=7 且阻挡
        buf[o + 4..o + 6].copy_from_slice(&42u16.to_le_bytes()); // front=42 不阻挡
        buf[o + 10] = 1; // front lib 字节

        let m = parse(&buf).unwrap();
        assert_eq!(m.kind, MapKind::Type0);
        assert_eq!((m.width, m.height), (2, 2));
        let c = m.cell(1, 0).unwrap();
        assert_eq!(c.back, 6); // 图号 7 → 帧 6
        assert_eq!(c.front, 41);
        assert_eq!(c.front_lib, 3);
        assert!(c.blocked);
        assert!(!m.cell(0, 0).unwrap().blocked);
        assert_eq!(m.cell(0, 0).unwrap().back, -1);
    }

    /// 合成 type100 地图: 阻挡位 0x20000000 + 索引掩码
    #[test]
    fn type100_blocked_and_mask() {
        let (w, h) = (3u16, 1u16);
        let mut buf = vec![0u8; 8 + (w * h) as usize * 26];
        buf[0..2].copy_from_slice(&1i16.to_le_bytes());
        buf[4..6].copy_from_slice(&w.to_le_bytes());
        buf[6..8].copy_from_slice(&h.to_le_bytes());
        // (x=1,y=0): back = 0x20000000 | 551 (阻挡 + 索引 551)
        let o = 8 + 26;
        buf[o..o + 2].copy_from_slice(&5i16.to_le_bytes()); // back_lib=5
        buf[o + 2..o + 6].copy_from_slice(&(0x2000_0000u32 | 551).to_le_bytes());

        let m = parse(&buf).unwrap();
        assert_eq!(m.kind, MapKind::Type100);
        let c = m.cell(1, 0).unwrap();
        assert_eq!(c.back, 550);
        assert_eq!(c.back_lib, 5);
        assert!(c.blocked);
        assert!(!m.cell(0, 0).unwrap().blocked);
    }

    /// 合成 type1 地图: XOR 解密 + frontLib +2
    #[test]
    fn type1_xor_decrypt() {
        let (w, h, xor) = (2u16, 1u16, 0x5A5Au16);
        let mut buf = vec![0u8; 54 + (w * h) as usize * 15];
        buf[0..8].copy_from_slice(b"Map 2010");
        buf[23..25].copy_from_slice(&xor.to_le_bytes());
        buf[21..23].copy_from_slice(&(w ^ xor).to_le_bytes());
        buf[25..27].copy_from_slice(&(h ^ xor).to_le_bytes());
        // (x=0,y=0): back=100(阻挡), front=9, front_lib 字节=1 → 3
        let o = 54;
        buf[o..o + 4].copy_from_slice(&((0x2000_0000u32 | 100) ^ 0xAA38_AA38).to_le_bytes());
        buf[o + 6..o + 8].copy_from_slice(&(9u16 ^ xor).to_le_bytes());
        buf[o + 12] = 1;

        let m = parse(&buf).unwrap();
        assert_eq!(m.kind, MapKind::Type1);
        assert_eq!((m.width, m.height), (2, 1));
        let c = m.cell(0, 0).unwrap();
        assert_eq!(c.back, 99);
        assert_eq!(c.front, 8);
        assert_eq!(c.front_lib, 3);
        assert!(c.blocked);
    }

    #[test]
    fn reject_garbage() {
        assert!(parse(&[0u8; 4]).is_err());
        assert!(parse(b"\xff\xff\xff\xff\xff\xff\xff\xff").is_err());
    }
}
