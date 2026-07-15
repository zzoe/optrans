//! 公共模块
//!
//! 导出项目全局共享的类型、常量与工具函数：
//! - **constants**: 全局常量定义（模块像素数、灰度级数、FEC 参数等）
//! - **crc**: CRC-16/XMODEM 校验，用于格式信息验证
//! - **error**: 统一错误类型 OptransError
//! - **fec**: 块级 FEC（前向纠错）编解码，RS(192,128) 纠删码
//! - **mask**: 位级 XOR 掩码，用于数据混淆
//! - **reed_solomon_utils**: 帧内 RS(255,217) 编码辅助函数（块数计算、容量计算）
//! - **types**: 共享数据类型定义（帧类型、格式信息、元数据、数据块等）

pub mod constants;
pub mod crc;
pub mod error;
pub mod fec;
pub mod mask;
pub mod reed_solomon_utils;
pub mod types;
