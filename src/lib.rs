//! optrans（Optical Transfer）— 通过屏幕视觉编码在远程桌面两端传输文件
//!
//! 本 crate 实现了完整的文件光学传输协议：
//!
//! **发送端流程**：文件读取 → SHA-256 计算 → 分块 → 块级 FEC 编码 → 帧构建 → 灰度渲染 → 全屏窗口显示
//! - RS(255,217) 帧内纠错，可纠正单帧内最多 19 个字节错误
//! - RS(192,128) 块级 FEC，每组 128 数据块 + 64 校验块，容错率 33%
//! - 支持 2/4/8/16 级灰度编码，直映射比特到灰度值
//! - 异步渲染 + 串行显示，有界 channel 防止内存堆积
//!
//! **接收端流程**：全屏截屏 → Finder Pattern 定位 → 格式信息解码 → 模块提取 → RS 解码 → 去重 → FEC 恢复 → SHA-256 校验 → 写文件
//! - 流水线架构：capture 线程 → decode 线程池 → aggregator 线程
//! - 格式信息 N 路多数投票 + CRC-16 校验，抵抗视频压缩噪声
//! - 支持长方形帧和帧锁定模式，首次定位后复用位置跳过全屏扫描

pub mod cli;
pub mod common;
pub mod decoder;
pub mod encoder;
pub mod platform;
pub mod receiver;
pub mod sender;
