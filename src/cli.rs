//! CLI 参数定义
//!
//! 用户仅需指定 send/receive + 文件路径。
//! 大多数调优参数（模块像素、帧间隔等）为程序内部常量；
//! 灰度级数可按显示链路质量调整（`send --gray-levels`）。

use clap::{Parser, Subcommand};

/// optrans（Optical Transfer）— 通过屏幕视觉编码传输文件
#[derive(Parser, Debug)]
#[command(name = "optrans", version, about = "通过屏幕视觉编码在远程桌面两端传输文件")]
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
        /// 灰度级数（2/4/8/16）。经远程桌面/视频编码传输时用 8：
        /// 实测级距 24 可稳定解码，16 级的级距仅 12，与视频编码的量化步长
        /// 同量级，会被抹平导致每帧都卡在 RS 纠错上限边缘。
        /// 本地显示器直连可用 16（帧数最少）。
        #[arg(long, default_value_t = crate::common::constants::GRAY_LEVELS_DEFAULT)]
        gray_levels: u32,
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
