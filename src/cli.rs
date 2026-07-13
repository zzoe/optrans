//! CLI 参数定义
//!
//! 用户仅需指定 send/receive + 文件路径。
//! 所有调优参数（模块像素、帧间隔、灰度级数等）均为程序内部常量。

use clap::{Parser, Subcommand};

/// optrans（Optical Transfer）— 通过屏幕视觉编码传输文件
#[derive(Parser, Debug)]
#[command(name = "optrans", about = "通过屏幕视觉编码在远程桌面两端传输文件")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// 发送文件：将文件编码为视觉帧并通过全屏窗口显示
    Send {
        /// 待发送的文件路径
        path: std::path::PathBuf,
    },
    /// 接收文件：从全屏截取视觉帧并解码复原文件
    Receive {
        /// 输出文件路径（可选；不指定时从发送端元数据解析文件名）
        output: Option<std::path::PathBuf>,
    },
}

/// 解析 CLI 并返回子命令枚举
pub fn parse_command() -> Command {
    Cli::parse().command
}
