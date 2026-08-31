//! 盛大 `WZL/WZX` 图库解码 (`www.shandagames.com` 头变体)。
//!
//! 对市售技能特效包 8 个真实样本逐字节回归后落地的布局:
//! - WZX: 44 字节标题、`count i32`、`count` 个 `i32` 帧在 WZL 内的偏移;
//! - WZL: 44 字节标题、`count i32`、保留 16 字节, 数据自 64 起;
//! - 每帧 16 字节头: `enc u8, pad u8, 保留 u16, w i16, h i16, x i16, y i16,
//!   len i32`, 随后 `len` 字节 zlib 压缩像素;
//! - 像素按 enc: 5 = RGB565、6 = BGR888; 行 4 字节对齐, 自上而下;
//!   纯黑 = 透明 (老图库约定)。
//! - enc 3 (8bpp 调色板, 板不随文件) 与 enc 7 (非 zlib) 样本占比 ~5%,
//!   当前按空帧跳过, 不盲猜。

use std::io::Read;

use crate::{DecodedImage, FormatError, Result};

pub struct WzlLib {
    wzl: Vec<u8>,
    offsets: Vec<i32>,
}

impl WzlLib {
    pub fn parse(wzl: Vec<u8>, wzx: Vec<u8>) -> Result<Self> {
        if wzl.len() < 64 || wzx.len() < 48 {
            return Err(FormatError::Truncated {
                need: 64,
                got: wzl.len().min(wzx.len()),
            });
        }
        // 标题区三种变体并存于同一素材包: 裸标题 / `\x13` 长度前缀标题 /
        // 全零。只要求"像标题", 真正的校验靠 count 与偏移的结构合法性
        let title_ok = |d: &[u8]| {
            d.starts_with(b"www.shandagames.com")
                || d.starts_with(b"\x13www.shandagames.com")
                || d[..19].iter().all(|&b| b == 0)
        };
        if !title_ok(&wzl) || !title_ok(&wzx) {
            return Err(FormatError::Unrecognized("wzl/wzx magic"));
        }
        let count = i32::from_le_bytes(wzx[44..48].try_into().unwrap());
        if !(0..=2_000_000).contains(&count) {
            return Err(FormatError::Unrecognized("wzx count"));
        }
        let need = 48 + count as usize * 4;
        if wzx.len() < need {
            return Err(FormatError::Truncated {
                need,
                got: wzx.len(),
            });
        }
        let offsets = (0..count as usize)
            .map(|i| {
                let o = 48 + i * 4;
                i32::from_le_bytes(wzx[o..o + 4].try_into().unwrap())
            })
            .collect();
        Ok(Self { wzl, offsets })
    }

    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// 解码第 `index` 帧。空帧/越界/坏条目/未支持编码返回 Ok(None)。
    pub fn image(&self, index: usize) -> Result<Option<DecodedImage>> {
        let Some(&off) = self.offsets.get(index) else {
            return Ok(None);
        };
        if off <= 0 {
            return Ok(None);
        }
        let o = off as usize;
        if o + 16 > self.wzl.len() {
            return Ok(None);
        }
        let enc = self.wzl[o];
        let rd = |i: usize| i16::from_le_bytes(self.wzl[o + i..o + i + 2].try_into().unwrap());
        let (w, h, x, y) = (rd(4), rd(6), rd(8), rd(10));
        let len = i32::from_le_bytes(self.wzl[o + 12..o + 16].try_into().unwrap());
        if w <= 0 || h <= 0 || len <= 0 {
            return Ok(None);
        }
        let bpp = match enc {
            5 => 2usize, // RGB565
            6 => 3,      // BGR888
            _ => return Ok(None),
        };
        let start = o + 16;
        let Some(end) = start.checked_add(len as usize) else {
            return Ok(None);
        };
        if end > self.wzl.len() {
            return Ok(None);
        }
        let (w, h) = (w as usize, h as usize);
        let stride = (w * bpp + 3) & !3;
        let mut raw = Vec::with_capacity(stride * h);
        if flate2::read::ZlibDecoder::new(&self.wzl[start..end])
            .read_to_end(&mut raw)
            .is_err()
            || raw.len() < stride * h
        {
            return Ok(None);
        }
        let mut rgba = Vec::with_capacity(w * h * 4);
        for row in 0..h {
            let line = &raw[row * stride..];
            for px in 0..w {
                let (r, g, b) = match enc {
                    5 => {
                        let v = u16::from_le_bytes([line[px * 2], line[px * 2 + 1]]);
                        let r = ((v >> 11) & 0x1f) as u8;
                        let g = ((v >> 5) & 0x3f) as u8;
                        let b = (v & 0x1f) as u8;
                        (r << 3 | r >> 2, g << 2 | g >> 4, b << 3 | b >> 2)
                    }
                    _ => (line[px * 3 + 2], line[px * 3 + 1], line[px * 3]),
                };
                if r == 0 && g == 0 && b == 0 {
                    rgba.extend_from_slice(&[0, 0, 0, 0]); // 纯黑 = 透明
                } else {
                    rgba.extend_from_slice(&[r, g, b, 255]);
                }
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
