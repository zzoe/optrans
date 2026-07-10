//! CLI 参数定义
//!
//! 使用 clap derive 宏定义 `send` 和 `receive` 子命令及其参数。

use clap::{Parser, Subcommand};
use crate::common::constants;
use crate::common::types::EncodingMode;

/// optrans（Optical Transfer）— 通过屏幕视觉编码传输文件
#[derive(Parser, Debug)]
#[command(name = "optrans", version = "0.4.0", about = "通过屏幕视觉编码在远程桌面两端传输文件")]
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

        /// 每模块像素数（2-8 之间的偶数，默认 4）
        #[arg(long, default_value_t = constants::MODULE_PIXELS_DEFAULT)]
        module_pixels: u32,

        /// 帧间隔毫秒数（默认 150ms）
        #[arg(long, default_value_t = constants::FRAME_INTERVAL_MS)]
        interval_ms: u64,

        /// 灰度级数：2/4/5/8/16（默认 5）
        #[arg(long, default_value_t = constants::GRAY_LEVELS_DEFAULT)]
        gray_levels: u32,

        /// 帧边距像素数（编码图案四周的黑边，远程桌面场景建议 120）
        #[arg(long, default_value_t = constants::FRAME_MARGIN_PX_DEFAULT)]
        frame_margin: u32,

        /// 禁用差分编码（P 帧），全发 I 帧
        #[arg(long, default_value_t = false)]
        no_diff: bool,

        /// 禁用心跳噪声注入
        #[arg(long, default_value_t = false)]
        no_heartbeat: bool,

        /// 强制 I 帧间隔（连续 P 帧数超过此值后发 I 帧，默认 10）
        #[arg(long, default_value_t = constants::I_FRAME_INTERVAL_DEFAULT)]
        i_frame_interval: u32,
    },
    /// 接收文件：从全屏截取视觉帧并解码复原文件
    Receive {
        /// 输出文件路径（可选；不指定时从发送端元数据解析文件名）
        output: Option<std::path::PathBuf>,

        /// 每模块像素数（必须与发送端一致，默认 4）
        #[arg(long, default_value_t = constants::MODULE_PIXELS_DEFAULT)]
        module_pixels: u32,

        /// 编码模式：auto / binary / gray[-N]（默认 auto）
        #[arg(long, default_value = "auto")]
        encoding_mode: String,

        /// 调试模式：把解码失败的截图保存到 optrans_debug/ 目录，便于排查定位/采样问题
        #[arg(long, default_value_t = false)]
        debug: bool,
    },
    /// 测试模式：发送端显示单帧硬编码数据，接收端截取解码
    Test {
        #[command(subcommand)]
        mode: TestMode,
    },
}

/// 测试模式子命令
#[derive(Subcommand, Debug, Clone)]
pub enum TestMode {
    /// 发送固定测试内容（复用 send 完整流程，仅文件内容固定为 "HELLO OPTRANS TEST"）
    Send {
        /// 每模块像素数（2-8 之间的偶数，默认 4）
        #[arg(long, default_value_t = constants::MODULE_PIXELS_DEFAULT)]
        module_pixels: u32,

        /// 帧间隔毫秒数（默认 300ms）
        #[arg(long, default_value_t = constants::FRAME_INTERVAL_MS)]
        interval_ms: u64,

        /// 灰度级数：2/4/5/8/16（默认 2，二值最抗远程桌面压缩）
        #[arg(long, default_value_t = constants::GRAY_LEVELS_DEFAULT)]
        gray_levels: u32,

        /// 帧边距像素数（编码图案四周的黑边，远程桌面场景建议 120）
        #[arg(long, default_value_t = constants::FRAME_MARGIN_PX_DEFAULT)]
        frame_margin: u32,

        /// 禁用差分编码（P 帧），全发 I 帧
        #[arg(long, default_value_t = false)]
        no_diff: bool,

        /// 禁用心跳噪声注入
        #[arg(long, default_value_t = false)]
        no_heartbeat: bool,

        /// 强制 I 帧间隔（连续 P 帧数超过此值后发 I 帧，默认 10）
        #[arg(long, default_value_t = constants::I_FRAME_INTERVAL_DEFAULT)]
        i_frame_interval: u32,
    },
    /// 接收测试帧（复用 receive 完整流程）
    Receive {
        /// 输出文件路径（可选；不指定时从发送端元数据解析文件名）
        output: Option<std::path::PathBuf>,

        /// 每模块像素数（必须与发送端一致，默认 4）
        #[arg(long, default_value_t = constants::MODULE_PIXELS_DEFAULT)]
        module_pixels: u32,

        /// 编码模式：auto / binary / gray[-N]（默认 auto）
        #[arg(long, default_value = "auto")]
        encoding_mode: String,

        /// 调试模式：把解码失败的截图保存到 optrans_debug/ 目录
        #[arg(long, default_value_t = false)]
        debug: bool,
    },
    /// 从指定 PNG 图片解码测试帧（用于离线调试）
    DecodeImage {
        /// PNG 图片路径
        path: std::path::PathBuf,
    },
}

/// 发送端参数（清理后的结构体）
#[derive(Debug, Clone)]
pub struct SendArgs {
    /// 待发送的文件路径
    pub path: std::path::PathBuf,
    /// 每模块像素数（必须是 2-8 之间的偶数）
    pub module_pixels: u32,
    /// 帧间隔（毫秒）
    pub interval_ms: u64,
    /// 帧边距像素数（编码图案四周的黑边）
    pub frame_margin: u32,
    /// v0.2: 灰度级数（2/4/5/8/16）
    pub gray_levels: u32,
    /// v0.2: 是否启用差分编码（P 帧）
    pub enable_diff: bool,
    /// v0.2: 是否启用心跳噪声
    pub enable_heartbeat: bool,
    /// v0.2: 强制 I 帧间隔
    pub i_frame_interval: u32,
}

/// 接收端参数（清理后的结构体）
#[derive(Debug, Clone)]
pub struct ReceiveArgs {
    /// 输出文件路径（None 时从元数据解析文件名）
    pub output: Option<std::path::PathBuf>,
    /// 每模块像素数（必须与发送端一致）
    pub module_pixels: u32,
    /// v0.2: 编码模式
    pub encoding_mode: EncodingMode,
    /// 调试模式：保存解码失败的截图
    pub debug: bool,
}

/// 解析 CLI 参数并返回清理后的结构体
pub fn parse() -> (SendArgs, ReceiveArgs) {
    panic!("请使用 parse_command() 函数");
}

/// 解析 CLI 并返回子命令枚举
pub fn parse_command() -> Command {
    Cli::parse().command
}

impl SendArgs {
    /// 从命令行参数构建发送端参数
    pub fn from_cli(
        path: std::path::PathBuf,
        module_pixels: u32,
        interval_ms: u64,
        frame_margin: u32,
        gray_levels: u32,
        no_diff: bool,
        no_heartbeat: bool,
        i_frame_interval: u32,
    ) -> Self {
        SendArgs {
            path,
            module_pixels,
            interval_ms,
            frame_margin,
            gray_levels,
            enable_diff: !no_diff,
            enable_heartbeat: !no_heartbeat,
            i_frame_interval,
        }
    }
}

impl ReceiveArgs {
    /// 从命令行参数构建接收端参数
    pub fn from_cli(
        output: Option<std::path::PathBuf>,
        module_pixels: u32,
        encoding_mode_str: &str,
        debug: bool,
    ) -> Self {
        let encoding_mode = EncodingMode::from_str(encoding_mode_str)
            .unwrap_or(EncodingMode::Auto);
        ReceiveArgs {
            output,
            module_pixels,
            encoding_mode,
            debug,
        }
    }
}
