//! optrans 程序入口
//!
//! 解析 CLI 参数并分派到发送端或接收端逻辑。

use log::info;
use optrans::cli::{self, Command, ReceiveArgs, SendArgs};
use optrans::common::error::OptransResult;
use optrans::common::types::validate_module_pixels;
use optrans::win32::ffi;

/// 声明进程 DPI 感知，使 GetSystemMetrics 返回物理像素而非逻辑像素。
///
/// 使用 SetProcessDPIAware (Vista+，Win7 兼容)，必须在任何 UI/截屏操作前调用。
fn enable_dpi_awareness() {
    unsafe { ffi::SetProcessDPIAware() };
}

fn main() -> OptransResult<()> {
    pretty_env_logger::init_timed();
    enable_dpi_awareness();
    let cmd = cli::parse_command();

    match cmd {
        Command::Send {
            path,
            module_pixels,
            interval_ms,
            frame_margin,
            gray_levels,
            no_diff,
            no_heartbeat,
            i_frame_interval,
        } => {
            validate_module_pixels(module_pixels)?;
            let args = SendArgs::from_cli(
                path,
                module_pixels,
                interval_ms,
                frame_margin,
                gray_levels,
                no_diff,
                no_heartbeat,
                i_frame_interval,
            );
            info!("发送模式");
            info!("  文件: {:?}", args.path);
            info!("  模块像素: {}", args.module_pixels);
            info!("  帧间隔: {}ms", args.interval_ms);
            info!("  帧边距: {}px", args.frame_margin);
            info!("  灰度级数: {}", args.gray_levels);
            info!(
                "  差分编码: {}",
                if args.enable_diff { "启用" } else { "禁用" }
            );
            info!(
                "  心跳: {}",
                if args.enable_heartbeat {
                    "启用"
                } else {
                    "禁用"
                }
            );
            info!("  I帧间隔: {}", args.i_frame_interval);
            optrans::sender::transmit::run_send(args)
        }
        Command::Receive {
            output,
            module_pixels,
            encoding_mode,
            debug,
        } => {
            validate_module_pixels(module_pixels)?;
            let args = ReceiveArgs::from_cli(output, module_pixels, &encoding_mode, debug);
            info!("接收模式");
            match &args.output {
                Some(p) => info!("  输出: {:?}", p),
                None => info!("  输出: <从发送端元数据解析>"),
            }
            info!("  模块像素: {}", args.module_pixels);
            info!("  编码模式: {:?}", args.encoding_mode);
            if args.debug {
                info!("  调试: 已启用（失败截图保存到 optrans_debug/）");
            }
            optrans::receiver::mod_impl::run_receive(args)
        }
    }
}
