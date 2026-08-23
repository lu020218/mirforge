//! Wemade `WIL/WIX` 图库解码。
//!
//! **状态：未实现（开发计划 M0 任务 0.4）。**
//! WIL 存在多个版本变体（8bpp 调色板 / 16bpp、有无 padding 字段），
//! 必须对真实样本回归后落地，不做盲写。样本回归在本地进行，样本文件不入库。

use crate::{DecodedImage, FormatError, Result};

pub struct WilLib;

impl WilLib {
    pub fn parse(_wil: Vec<u8>, _wix: Vec<u8>) -> Result<Self> {
        Err(FormatError::NotImplemented("WIL/WIX"))
    }

    pub fn image(&self, _index: usize) -> Result<Option<DecodedImage>> {
        Err(FormatError::NotImplemented("WIL/WIX"))
    }
}
