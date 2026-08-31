//! MirForge 自有图库格式 `.mfl` (MirForge Library)。
//!
//! 与 Crystal .Lib 划清界限的自有容器: packs/ 体系下的武器/衣甲/图标等
//! 自购或自制资源一律用它, Crystal 原版资源保持 .Lib 只读兜底, 逐步淘汰。
//!
//! 布局 (小端):
//! - 头: 魔数 `MFL1` 4 字节、`count u32`;
//! - 索引表: `count` 个 `u32` 文件内偏移 (0 = 空帧);
//! - 每帧: `w u16, h u16, x i16, y i16, len u32` 共 12 字节,
//!   随后 `len` 字节 GZip 压缩的 RGBA8 像素 (w*h*4)。
//!
//! 与 .Lib 的差异: 有魔数可靠识别; 像素直接存 RGBA (不再背 BGRA 历史包袱);
//! 帧头去掉不用的阴影字段。

use std::io::{Read, Write};

use crate::{Bytes, DecodedImage, FormatError, Result};

pub const MAGIC: &[u8; 4] = b"MFL1";

/// 已打开的 .mfl 图库 (持有原始字节, 按需解帧)。
#[derive(Debug)]
pub struct MflLib {
    data: Bytes,
    offsets: Vec<u32>,
}

impl MflLib {
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        Self::parse_bytes(Bytes::Vec(data))
    }

    /// 与 parse 同, 但可用 mmap 字节 (运行时路径)
    pub fn parse_bytes(data: Bytes) -> Result<Self> {
        if data.len() < 8 {
            return Err(FormatError::Truncated {
                need: 8,
                got: data.len(),
            });
        }
        if &data[0..4] != MAGIC {
            return Err(FormatError::Unrecognized("mfl magic"));
        }
        let count = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        if count > 2_000_000 {
            return Err(FormatError::Unrecognized("mfl count"));
        }
        let need = 8 + count * 4;
        if data.len() < need {
            return Err(FormatError::Truncated {
                need,
                got: data.len(),
            });
        }
        let offsets = (0..count)
            .map(|i| {
                let o = 8 + i * 4;
                u32::from_le_bytes(data[o..o + 4].try_into().unwrap())
            })
            .collect();
        Ok(Self { data, offsets })
    }

    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// 只读帧头尺寸, 不解压像素 (帧表探测用, 零成本)。空帧/越界返回 None。
    pub fn dims(&self, index: usize) -> Option<(u16, u16)> {
        let &off = self.offsets.get(index)?;
        let o = off as usize;
        if o == 0 || o + 12 > self.data.len() {
            return None;
        }
        let w = u16::from_le_bytes(self.data[o..o + 2].try_into().unwrap());
        let h = u16::from_le_bytes(self.data[o + 2..o + 4].try_into().unwrap());
        (w > 0 && h > 0).then_some((w, h))
    }

    /// 解码第 `index` 帧。空帧/越界/坏条目返回 Ok(None), 与 CrystalLib 同约定。
    pub fn image(&self, index: usize) -> Result<Option<DecodedImage>> {
        let Some(&off) = self.offsets.get(index) else {
            return Ok(None);
        };
        let o = off as usize;
        if o == 0 || o + 12 > self.data.len() {
            return Ok(None);
        }
        let ru = |i: usize| u16::from_le_bytes(self.data[o + i..o + i + 2].try_into().unwrap());
        let ri = |i: usize| i16::from_le_bytes(self.data[o + i..o + i + 2].try_into().unwrap());
        let (w, h) = (ru(0), ru(2));
        let (x, y) = (ri(4), ri(6));
        let len = u32::from_le_bytes(self.data[o + 8..o + 12].try_into().unwrap()) as usize;
        if w == 0 || h == 0 || len == 0 {
            return Ok(None);
        }
        let start = o + 12;
        let Some(end) = start.checked_add(len) else {
            return Ok(None);
        };
        if end > self.data.len() {
            return Ok(None);
        }
        let expect = w as usize * h as usize * 4;
        let mut rgba = Vec::with_capacity(expect);
        if flate2::read::GzDecoder::new(&self.data[start..end])
            .read_to_end(&mut rgba)
            .is_err()
            || rgba.len() < expect
        {
            return Ok(None);
        }
        rgba.truncate(expect);
        Ok(Some(DecodedImage {
            width: w,
            height: h,
            offset_x: x,
            offset_y: y,
            rgba,
        }))
    }
}

/// 图库句柄: 按魔数自动识别 .mfl (自有) 或 Crystal .Lib (兜底), 读帧同接口。
/// 客户端渲染与服务端管理台预览共用, 覆盖规则只需写一遍。
#[derive(Debug)]
pub enum AnyLib {
    Mfl(MflLib),
    Crystal(crate::crystal_lib::CrystalLib),
}

impl AnyLib {
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        Self::parse_bytes(Bytes::Vec(data))
    }

    /// mmap 打开库文件: 原始字节不进堆, 大库常开由 OS 页缓存管驻留
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Self::parse_bytes(Bytes::map_file(path)?)
    }

    fn parse_bytes(data: Bytes) -> Result<Self> {
        if data.starts_with(MAGIC) {
            Ok(AnyLib::Mfl(MflLib::parse_bytes(data)?))
        } else {
            Ok(AnyLib::Crystal(
                crate::crystal_lib::CrystalLib::parse_bytes(data)?,
            ))
        }
    }

    pub fn image(&self, index: usize) -> Result<Option<DecodedImage>> {
        match self {
            AnyLib::Mfl(l) => l.image(index),
            AnyLib::Crystal(l) => l.image(index),
        }
    }

    /// 只读帧头尺寸, 不解压像素 (帧表探测用)
    pub fn dims(&self, index: usize) -> Option<(u16, u16)> {
        match self {
            AnyLib::Mfl(l) => l.dims(index),
            AnyLib::Crystal(l) => l.dims(index),
        }
    }

    pub fn len(&self) -> usize {
        match self {
            AnyLib::Mfl(l) => l.len(),
            AnyLib::Crystal(l) => l.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 待写入的一帧 (None = 空帧占位)
pub struct MflFrame {
    pub width: u16,
    pub height: u16,
    pub offset_x: i16,
    pub offset_y: i16,
    /// RGBA8, 长度 = width * height * 4
    pub rgba: Vec<u8>,
}

/// 流式写入器: 先定帧位数, 逐帧追加 (空帧 push_empty), 末尾 finish。
/// 大库转码不用把全部解码帧攒在内存里。
pub struct MflWriter {
    buf: Vec<u8>,
    count: usize,
    next: usize,
}

impl MflWriter {
    pub fn new(count: usize) -> Self {
        let mut buf = Vec::with_capacity(8 + count * 4);
        buf.extend_from_slice(MAGIC);
        buf.extend_from_slice(&(count as u32).to_le_bytes());
        buf.resize(8 + count * 4, 0);
        Self {
            buf,
            count,
            next: 0,
        }
    }

    pub fn push_empty(&mut self) -> Result<()> {
        if self.next >= self.count {
            return Err(FormatError::Unrecognized("mfl writer overflow"));
        }
        self.next += 1;
        Ok(())
    }

    pub fn push(&mut self, f: &MflFrame) -> Result<()> {
        if self.next >= self.count {
            return Err(FormatError::Unrecognized("mfl writer overflow"));
        }
        if f.rgba.len() != f.width as usize * f.height as usize * 4 {
            return Err(FormatError::Unrecognized("mfl frame rgba size"));
        }
        if f.width == 0 || f.height == 0 {
            return self.push_empty(); // 空图当空帧
        }
        let i = self.next;
        self.next += 1;
        let off = self.buf.len() as u32;
        self.buf[8 + i * 4..8 + i * 4 + 4].copy_from_slice(&off.to_le_bytes());
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let z = gz
            .write_all(&f.rgba)
            .and_then(|_| gz.finish())
            .map_err(|_| FormatError::Unrecognized("mfl gzip"))?;
        self.buf.extend_from_slice(&f.width.to_le_bytes());
        self.buf.extend_from_slice(&f.height.to_le_bytes());
        self.buf.extend_from_slice(&f.offset_x.to_le_bytes());
        self.buf.extend_from_slice(&f.offset_y.to_le_bytes());
        self.buf.extend_from_slice(&(z.len() as u32).to_le_bytes());
        self.buf.extend_from_slice(&z);
        Ok(())
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

/// 打包成 .mfl 字节流。索引即帧号, 打包工具与运行时读取共用此真源。
pub fn write(frames: &[Option<MflFrame>]) -> Result<Vec<u8>> {
    let mut w = MflWriter::new(frames.len());
    for f in frames {
        match f {
            Some(f) => w.push(f)?,
            None => w.push_empty()?,
        }
    }
    Ok(w.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let px = vec![255u8; 2 * 3 * 4];
        let frames = vec![
            None,
            Some(MflFrame {
                width: 2,
                height: 3,
                offset_x: -5,
                offset_y: 7,
                rgba: px.clone(),
            }),
        ];
        let bytes = write(&frames).unwrap();
        let lib = MflLib::parse(bytes).unwrap();
        assert_eq!(lib.len(), 2);
        assert!(lib.image(0).unwrap().is_none());
        let f = lib.image(1).unwrap().unwrap();
        assert_eq!((f.width, f.height, f.offset_x, f.offset_y), (2, 3, -5, 7));
        assert_eq!(f.rgba, px);
        assert!(lib.image(9).unwrap().is_none());
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(MflLib::parse(b"XXXX\0\0\0\0".to_vec()).is_err());
    }
}
