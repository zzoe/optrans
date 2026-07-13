//! optrans 程序入口
//!
//! 解析 CLI 参数并分派到发送端或接收端逻辑。

use log::info;
use optrans::cli::{self, Command};
use optrans::common::constants;
use optrans::common::error::OptransResult;
use optrans::common::types::{validate_module_pixels, EncodingMode};
use optrans::win32::ffi;

/// 声明进程 DPI 感知，使 GetSystemMetrics 返回物理像素而非逻辑像素。
fn enable_dpi_awareness() {
    unsafe { ffi::SetProcessDPIAware() };
}

fn main() -> OptransResult<()> {
    pretty_env_logger::init_timed();
    enable_dpi_awareness();
    let cmd = cli::parse_command();

    match cmd {
        Command::Send { path } => {
            validate_module_pixels(constants::MODULE_PIXELS_DEFAULT)?;
            info!("发送模式");
            info!("  文件: {:?}", path);
            info!("  模块像素: {}", constants::MODULE_PIXELS_DEFAULT);
            info!("  帧间隔: {}ms", constants::FRAME_INTERVAL_MS);
            info!("  帧边距: {}px", constants::FRAME_MARGIN_PX_DEFAULT);
            info!("  灰度级数: {}", constants::GRAY_LEVELS_DEFAULT);
            info!("  心跳: 启用");
            info!("  I帧间隔: {}", constants::I_FRAME_INTERVAL_DEFAULT);
            optrans::sender::transmit::run_send(path)
        }
        Command::Receive { output } => {
            validate_module_pixels(constants::MODULE_PIXELS_DEFAULT)?;
            let encoding_mode = EncodingMode::Auto;
            info!("接收模式");
            match &output {
                Some(p) => info!("  输出: {:?}", p),
                None => info!("  输出: <从发送端元数据解析>"),
            }
            info!("  模块像素: {}", constants::MODULE_PIXELS_DEFAULT);
            info!("  编码模式: {:?}", encoding_mode);
            optrans::receiver::mod_impl::run_receive(output, encoding_mode)
        }
    }
}
