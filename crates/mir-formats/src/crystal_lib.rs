//! Crystal 引擎 `.Lib` 图库解码。
//!
//! 布局（在旧项目对 WemadeMir2 全套图库验证过）：
//! - 头：`version i32@0`、`count i32@4`；version ≥ 3 时 `frame_seek i32@8`，
//!   索引表从 12 起；否则从 8 起。
//! - 索引表：`count` 个 `i32` 文件内偏移（≤0 = 空帧）。
//! - 每帧：`w i16, h i16, x i16, y i16, shadow_x i16, shadow_y i16, shadow u8, length i32`
//!   共 17 字节，随后 `length` 字节 GZip 压缩的 BGRA32 像素（w*h*4）。
//! - 像素：BGRA → RGBA，alpha 直通（库数据编码期已把背景像素置 alpha=0）。

use std::io::Read;

use crate::{DecodedImage, FormatError, Result};

/// 已打开的 .Lib 图库（持有原始字节，按需解帧）。
#[derive(Debug)]
pub struct CrystalLib {
    data: Vec<u8>,
    pub version: i32,
    offsets: Vec<i32>,
}

impl CrystalLib {
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        if data.len() < 8 {
            return Err(FormatError::Truncated {
                need: 8,
                got: data.len(),
            });
        }
        let version = i32::from_le_bytes(data[0..4].try_into().unwrap());
        let count = i32::from_le_bytes(data[4..8].try_into().unwrap());
        if !(0..=2_000_000).contains(&count) {
            return Err(FormatError::Unrecognized("crystal lib count"));
        }
        let table_at = if version >= 3 { 12 } else { 8 };
        let need = table_at + count as usize * 4;
        if data.len() < need {
            return Err(FormatError::Truncated {
                need,
                got: data.len(),
            });
        }
        let offsets = (0..count as usize)
            .map(|i| {
                let o = table_at + i * 4;
                i32::from_le_bytes(data[o..o + 4].try_into().unwrap())
            })
            .collect();
        Ok(Self {
            data,
            version,
            offsets,
        })
    }

    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// 解码第 `index` 帧。
    ///
    /// 空帧、越界、帧头非法、解压失败一律返回 Ok(None)（与传奇图库实况一致：
    /// 库里大量 1×1 占位帧与坏条目，读取方按"能识别多少读多少"降级，
    /// 行为对齐旧 JS 实现在真实全套图库上的验证结果）。
    pub fn image(&self, index: usize) -> Result<Option<DecodedImage>> {
        let Some(&off) = self.offsets.get(index) else {
            return Ok(None);
        };
        if off <= 0 || (off as usize) + 17 > self.data.len() {
            return Ok(None);
        }
        let o = off as usize;
        let rd = |i: usize| i16::from_le_bytes(self.data[o + i..o + i + 2].try_into().unwrap());
        let (w, h) = (rd(0), rd(2));
        let (x, y) = (rd(4), rd(6));
        let length = i32::from_le_bytes(self.data[o + 13..o + 17].try_into().unwrap());
        if w <= 0 || h <= 0 || length <= 0 {
            return Ok(None);
        }
        let start = o + 17;
        let Some(end) = start.checked_add(length as usize) else {
            return Ok(None);
        };
        if end > self.data.len() {
            return Ok(None);
        }
        let mut bgra = Vec::with_capacity((w as usize) * (h as usize) * 4);
        if flate2::read::GzDecoder::new(&self.data[start..end])
            .read_to_end(&mut bgra)
            .is_err()
        {
            return Ok(None);
        }
        let expect = w as usize * h as usize * 4;
        if bgra.len() < expect {
            return Ok(None);
        }
        let mut rgba = vec![0u8; expect];
        for i in (0..expect).step_by(4) {
            // BGRA → RGBA, alpha 直通: 库数据在编码期已把背景(含黑底)置 alpha=0,
            // 解码层不得再造 alpha (强制不透明会把透明背景画成垃圾色块 — 实测教训)
            rgba[i] = bgra[i + 2];
            rgba[i + 1] = bgra[i + 1];
            rgba[i + 2] = bgra[i];
            rgba[i + 3] = bgra[i + 3];
        }
        Ok(Some(DecodedImage {
            width: w as u16,
            height: h as u16,
            offset_x: x,
            offset_y: y,
            rgba,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn gz(data: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    /// 合成 v2 库: 2 帧, 第 0 帧空, 第 1 帧 2×1 (红色 + 黑色透明)
    #[test]
    fn v2_two_frames() {
        // BGRA: 像素0 = 红 (不透明), 像素1 = 黑色但 alpha=0 (库的透明背景约定)
        let pixels = [0u8, 0, 255, 255, 0, 0, 0, 0];
        let comp = gz(&pixels);
        let mut buf = Vec::new();
        buf.extend_from_slice(&2i32.to_le_bytes()); // version
        buf.extend_from_slice(&2i32.to_le_bytes()); // count
        let table_at = buf.len();
        buf.extend_from_slice(&0i32.to_le_bytes()); // frame0: 空
        buf.extend_from_slice(&0i32.to_le_bytes()); // frame1: 占位, 回填
        let img_at = buf.len() as i32;
        buf[table_at + 4..table_at + 8].copy_from_slice(&img_at.to_le_bytes());
        buf.extend_from_slice(&2i16.to_le_bytes()); // w
        buf.extend_from_slice(&1i16.to_le_bytes()); // h
        buf.extend_from_slice(&(-24i16).to_le_bytes()); // x
        buf.extend_from_slice(&(-16i16).to_le_bytes()); // y
        buf.extend_from_slice(&0i16.to_le_bytes()); // shadow_x
        buf.extend_from_slice(&0i16.to_le_bytes()); // shadow_y
        buf.push(0); // shadow
        buf.extend_from_slice(&(comp.len() as i32).to_le_bytes());
        buf.extend_from_slice(&comp);

        let lib = CrystalLib::parse(buf).unwrap();
        assert_eq!(lib.version, 2);
        assert_eq!(lib.len(), 2);
        assert!(lib.image(0).unwrap().is_none());
        let im = lib.image(1).unwrap().unwrap();
        assert_eq!((im.width, im.height), (2, 1));
        assert_eq!((im.offset_x, im.offset_y), (-24, -16));
        assert_eq!(&im.rgba[0..4], &[255, 0, 0, 255]); // BGRA→RGBA
        assert_eq!(im.rgba[7], 0); // alpha 直通: 背景 alpha=0 保持透明
        assert!(lib.image(9).unwrap().is_none());
    }

    /// v3 库索引表从 12 起
    #[test]
    fn v3_table_offset() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&3i32.to_le_bytes());
        buf.extend_from_slice(&1i32.to_le_bytes());
        buf.extend_from_slice(&0i32.to_le_bytes()); // frame_seek
        buf.extend_from_slice(&0i32.to_le_bytes()); // frame0 空
        let lib = CrystalLib::parse(buf).unwrap();
        assert_eq!(lib.len(), 1);
        assert!(lib.image(0).unwrap().is_none());
    }

    #[test]
    fn reject_garbage() {
        assert!(CrystalLib::parse(vec![1, 2, 3]).is_err());
        assert!(CrystalLib::parse(vec![0xFF; 16]).is_err());
    }
}
