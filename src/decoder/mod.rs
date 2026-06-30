//! 解码器模块
//!
//! 负责从截屏像素中提取和解码视觉帧：
//! - Finder pattern 检测与定位
//! - 模块提取（仿射变换 + 网格采样）
//! - Reed-Solomon 解码
//! - 帧格式信息与载荷解码

pub mod locate;
pub mod extract;
pub mod reed_solomon;
pub mod decode;
