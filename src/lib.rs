//! optrans（Optical Transfer）— 通过屏幕视觉编码在 Horizon 远程桌面两端传输文件
//!
//! 本 crate 实现了完整的文件光学传输协议：
//! - **发送端**：文件 → 分块 → RS 编码 → 帧构建 → 像素渲染 → 窗口显示
//! - **接收端**：截屏 → Finder 定位 → 模块提取 → RS 解码 → 去重 → 重组 → 校验

pub mod cli;
pub mod common;
pub mod decoder;
pub mod encoder;
pub mod receiver;
pub mod sender;
pub mod win32;
