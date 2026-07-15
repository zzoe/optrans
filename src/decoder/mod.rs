//! 解码器模块
//!
//! 负责从截屏像素中提取和解码视觉帧：
//! - **locate**: FrameLocation 定位，支持角落定位（推荐）和全屏扫描（降级）
//! - **extract**: 模块提取，通过仿射变换从模块坐标映射到像素坐标，支持二值/灰度采样
//! - **reed_solomon**: RS(255,217) 帧内纠错解码
//! - **decode**: 帧解码，包括格式信息解码（多数投票 + CRC 校验）和载荷解码

pub mod locate;
pub mod extract;
pub mod reed_solomon;
pub mod decode;
