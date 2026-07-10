//! optrans 程序入口
//!
//! 解析 CLI 参数并分派到发送端或接收端逻辑。

use optrans::cli::{self, Command, SendArgs, ReceiveArgs, TestMode};
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
            eprintln!("[optrans] 发送模式");
            eprintln!("[optrans]   文件: {:?}", args.path);
            eprintln!("[optrans]   模块像素: {}", args.module_pixels);
            eprintln!("[optrans]   帧间隔: {}ms", args.interval_ms);
            eprintln!("[optrans]   帧边距: {}px", args.frame_margin);
            eprintln!("[optrans]   灰度级数: {}", args.gray_levels);
            eprintln!("[optrans]   差分编码: {}", if args.enable_diff { "启用" } else { "禁用" });
            eprintln!("[optrans]   心跳: {}", if args.enable_heartbeat { "启用" } else { "禁用" });
            eprintln!("[optrans]   I帧间隔: {}", args.i_frame_interval);
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
            eprintln!("[optrans] 接收模式");
            match &args.output {
                Some(p) => eprintln!("[optrans]   输出: {:?}", p),
                None => eprintln!("[optrans]   输出: <从发送端元数据解析>"),
            }
            eprintln!("[optrans]   模块像素: {}", args.module_pixels);
            eprintln!("[optrans]   编码模式: {:?}", args.encoding_mode);
            if args.debug {
                eprintln!("[optrans]   调试: 已启用（失败截图保存到 optrans_debug/）");
            }
            optrans::receiver::mod_impl::run_receive(args)
        }
        Command::Test { mode } => match mode {
            TestMode::Send {
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
                    std::path::PathBuf::new(),
                    module_pixels,
                    interval_ms,
                    frame_margin,
                    gray_levels,
                    no_diff,
                    no_heartbeat,
                    i_frame_interval,
                );
                eprintln!("[optrans] test send — 发送固定测试内容（复用 send 完整流程）");
                optrans::test_mode::test_send(args)
            }
            TestMode::Receive {
                output,
                module_pixels,
                encoding_mode,
                debug,
            } => {
                validate_module_pixels(module_pixels)?;
                let args = ReceiveArgs::from_cli(output, module_pixels, &encoding_mode, debug);
                eprintln!("[optrans] test receive — 接收测试帧（复用 receive 完整流程）");
                optrans::test_mode::test_receive(args)
            }
            TestMode::DecodeImage { path } => {
                eprintln!("[optrans] test decode-image — 从图片解码测试帧");
                optrans::test_mode::test_decode_image(&path.to_string_lossy())
            }
        },
    }
}
