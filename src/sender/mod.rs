//! 发送端模块
//!
//! 负责文件读取、编码和窗口显示：
//! - **chunker**: 文件读取、SHA-256 计算、按帧容量分块
//! - **window**: Win32 无边框全屏窗口，GDI 位图渲染
//! - **transmit**: 发送编排，包括块级 FEC 编码、帧构建、异步渲染、串行显示

pub mod chunker;
pub mod window;
pub mod transmit;
