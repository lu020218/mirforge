//! Wemade `WIL/WIX` 图库解码 (`#ILIB v1.0` 16bpp 变体)。
//!
//! 对市售素材实际样本 (沃玛森林 Tiles55/smTiles55/Objects55, 毒蛇山谷
//! Tiles187) 逐字节回归后落地的布局:
//! - WIX: 44 字节标题 (`#INDX v1.0-WEMADE ...`)、`count i32`、
//!   `count` 个 `i32` 帧在 WIL 内的偏移;
//! - WIL: 44 字节标题 (`#ILIB v1.0-WEMADE ...`)、`count i32`、
//!   `color_count i32` (65536 = 16bpp RGB565)、保留 8 字节;
//! - 每帧: `w i16, h i16, x i16, y i16` + `w*h*2` 字节 RGB565 像素
//!   (行序自上而下, 小端; 纯黑 0x0000 = 透明 — 老图库的约定)。
//!
//! 8bpp 调色板变体样本未见, 遇到时报 NotImplemented 而不是猜。

use crate::{DecodedImage, FormatError, Result};

pub struct WilLib {
    wil: Vec<u8>,
    offsets: Vec<i32>,
}

impl WilLib {
    pub fn parse(wil: Vec<u8>, wix: Vec<u8>) -> Result<Self> {
        if wil.len() < 60 || wix.len() < 48 {
            return Err(FormatError::Truncated {
                need: 60,
                got: wil.len().min(wix.len()),
            });
        }
        if !wil.starts_with(b"#ILIB") || !wix.starts_with(b"#INDX") {
            return Err(FormatError::Unrecognized("wil/wix magic"));
        }
        let color_count = i32::from_le_bytes(wil[48..52].try_into().unwrap());
        if color_count != 65536 {
            return Err(FormatError::NotImplemented("WIL 8bpp 调色板变体"));
        }
        let count = i32::from_le_bytes(wix[44..48].try_into().unwrap());
        if !(0..=2_000_000).contains(&count) {
            return Err(FormatError::Unrecognized("wix count"));
        }
        let need = 48 + count as usize * 4;
        if wix.len() < need {
            return Err(FormatError::Truncated {
                need,
                got: wix.len(),
            });
        }
        let offsets = (0..count as usize)
            .map(|i| {
                let o = 48 + i * 4;
                i32::from_le_bytes(wix[o..o + 4].try_into().unwrap())
            })
            .collect();
        Ok(Self { wil, offsets })
    }

    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// 解码第 `index` 帧。空帧/越界/坏条目返回 Ok(None), 与其它图库同约定。
    pub fn image(&self, index: usize) -> Result<Option<DecodedImage>> {
        let Some(&off) = self.offsets.get(index) else {
            return Ok(None);
        };
        if off <= 0 {
            return Ok(None);
        }
        let o = off as usize;
        if o + 8 > self.wil.len() {
            return Ok(None);
        }
        let rd = |i: usize| i16::from_le_bytes(self.wil[o + i..o + i + 2].try_into().unwrap());
        let (w, h, x, y) = (rd(0), rd(2), rd(4), rd(6));
        if w <= 0 || h <= 0 {
            return Ok(None);
        }
        let n = w as usize * h as usize;
        let start = o + 8;
        let Some(end) = start.checked_add(n * 2) else {
            return Ok(None);
        };
        if end > self.wil.len() {
            return Ok(None);
        }
        let mut rgba = Vec::with_capacity(n * 4);
        for px in self.wil[start..end].chunks_exact(2) {
            let v = u16::from_le_bytes([px[0], px[1]]);
            if v == 0 {
                rgba.extend_from_slice(&[0, 0, 0, 0]); // 纯黑 = 透明
            } else {
                let r = ((v >> 11) & 0x1f) as u8;
                let g = ((v >> 5) & 0x3f) as u8;
                let b = (v & 0x1f) as u8;
                // 5/6 位扩到 8 位: 左移后用高位补低位, 避免整体偏暗
                rgba.extend_from_slice(&[r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2, 255]);
            }
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
