//! 接收端模块
//!
//! 负责屏幕截取、帧收集去重和文件重组校验。

pub mod capture;
pub mod collect;
pub mod reassemble;

/// 接收端流水线引擎（capture → decode → aggregate → file）
pub mod engine;
