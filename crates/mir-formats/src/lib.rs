//! # mir-formats
//!
//! 传奇（Legend of Mir 2）原版资源格式的纯逻辑解码器。
//! 引擎运行时直读市面已有资源，无预转换步骤。
//!
//! - [`map`]：地图 `.map`（type0 经典 / type1 "Map 2010" 加密 / type100 Crystal）
//! - [`crystal_lib`]：Crystal 引擎 `.Lib` 图库（GZip ARGB32）
//! - [`wil`]：Wemade `WIL/WIX` 图库（M0 任务 0.4，未实现）
//! - [`wzl`]：盛大 `WZL/WZX` 图库（M0 任务 0.5，未实现）
//!
//! 本 crate 不做任何文件 IO 之外的系统交互，输入为字节切片，便于测试与 WASM 复用。

pub mod crystal_lib;
pub mod map;
pub mod wil;
pub mod wzl;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("数据过短: 需要 {need} 字节, 实际 {got}")]
    Truncated { need: usize, got: usize },
    #[error("无法识别的格式: {0}")]
    Unrecognized(&'static str),
    #[error("尺寸非法: {0}")]
    BadDimensions(String),
    #[error("解压失败: {0}")]
    Decompress(String),
    #[error("尚未实现: {0} (见开发计划 M0)")]
    NotImplemented(&'static str),
}

pub type Result<T> = std::result::Result<T, FormatError>;

/// 解码后的单帧图像：RGBA8 + 绘制偏移（传奇帧自带锚点偏移）。
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedImage {
    pub width: u16,
    pub height: u16,
    /// 绘制偏移（相对放置格的像素偏移，可为负）
    pub offset_x: i16,
    pub offset_y: i16,
    /// RGBA8，长度 = width * height * 4
    pub rgba: Vec<u8>,
}
