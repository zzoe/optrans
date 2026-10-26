//! 接收端模块
//!
//! 负责屏幕截取、帧解码和文件重组：
//! - **capture**: Win32 GDI BitBlt 全屏截屏，支持三级回退策略兼容各种 RDP 实现
//! - **collect**: 帧收集器，去重、分类存储、FEC 恢复
//! - **reassemble**: 文件组装器，按块索引排序拼接、SHA-256 校验、写文件
//! - **engine**: 接收端流水线引擎，协调 capture、decode、aggregator 线程

pub mod capture;
pub mod collect;
pub mod reassemble;

/// 接收端流水线引擎（capture → decode → aggregate → file）
pub mod engine;
