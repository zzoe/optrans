//! optrans 程序入口
//!
//! 解析 CLI 参数并分派到发送端或接收端逻辑。

use log::{info, warn};
use optrans::cli::{self, Command};
use optrans::common::constants;
use optrans::common::error::OptransResult;
use optrans::common::types::validate_module_pixels;

fn main() -> OptransResult<()> {
    pretty_env_logger::formatted_builder()
        .filter_level(log::LevelFilter::Info)
        .init();
    // 声明进程 DPI 感知，使截屏/尺寸返回物理像素而非逻辑像素。
    // 失败时几何换算会错位（表现为接收端永远解不出帧），必须告警。
    if !optrans::platform::set_process_dpi_aware() {
        warn!("设置 DPI 感知失败，多显示器/高 DPI 环境下几何换算可能错位");
    }
    let cmd = cli::parse_command();

    match cmd {
        Command::Send { path, gray_levels } => {
            validate_module_pixels(constants::MODULE_PIXELS_DEFAULT)?;
            info!("发送模式");
            info!("  文件: {:?}", path);
            info!("  模块像素: {}", constants::MODULE_PIXELS_DEFAULT);
            info!("  帧间隔: {}ms", constants::FRAME_INTERVAL_MS);
            info!("  帧边距: {}px", constants::FRAME_MARGIN_PX_DEFAULT);
            info!("  灰度级数: {}", gray_levels);
            info!("  心跳: 启用");
            optrans::sender::transmit::run_send(path, gray_levels)
        }
        Command::Receive { output } => {
            validate_module_pixels(constants::MODULE_PIXELS_DEFAULT)?;
            info!("接收模式");
            match &output {
                Some(p) => info!("  输出: {:?}", p),
                None => info!("  输出: <从发送端元数据解析>"),
            }
            info!("  模块像素: {}", constants::MODULE_PIXELS_DEFAULT);
            optrans::receiver::engine::run_receive(output)
        }
    }
}
