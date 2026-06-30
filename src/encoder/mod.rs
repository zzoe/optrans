//! 编码器模块
//!
//! 负责将数据结构编码为视觉帧：
//! - Finder pattern 生成与格式信息编码
//! - Reed-Solomon 前向纠错编码
//! - 帧组装
//! - 帧到像素缓冲区渲染

pub mod pattern;
pub mod reed_solomon;
pub mod frame;
pub mod render;
