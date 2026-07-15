//! 编码器模块
//!
//! 负责将数据编码为视觉帧：
//! - **pattern**: Finder Pattern 生成（7×7 QR-like 定位图案）、格式信息区标记
//! - **reed_solomon**: RS(255,217) 帧内纠错编码，217 数据码字 + 38 校验码字
//! - **frame**: 帧组装，支持元数据帧、数据帧（多块合并）、EOT 帧、FEC 校验帧
//! - **render**: 帧到 RGBA 像素缓冲区渲染，支持 2/4/8/16 级灰度编码

pub mod pattern;
pub mod reed_solomon;
pub mod frame;
pub mod render;
