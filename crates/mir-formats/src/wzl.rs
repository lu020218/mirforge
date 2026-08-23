//! 盛大 `WZL/WZX` 图库解码。
//!
//! **状态：未实现（开发计划 M0 任务 0.5）。**
//! WZL 帧为 zlib 压缩，头部字段与位深随版本变化，对真实样本回归后落地。
//! 落地时格式参考: Crystal 项目 LibraryEditor (github.com/Suprcode/Crystal)。

use crate::{DecodedImage, FormatError, Result};

pub struct WzlLib;

impl WzlLib {
    pub fn parse(_wzl: Vec<u8>, _wzx: Vec<u8>) -> Result<Self> {
        Err(FormatError::NotImplemented("WZL/WZX"))
    }

    pub fn image(&self, _index: usize) -> Result<Option<DecodedImage>> {
        Err(FormatError::NotImplemented("WZL/WZX"))
    }
}
