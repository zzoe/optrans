//! 发送编排模块
//!
//! 发送主控逻辑：读取文件 → 块合并 → 帧构建 → 灰度渲染 → 循环显示 → EOT。
//! v0.9: 多块合并发送，每帧填满 payload 容量。
//! v0.19.2: 帧间隔从 render 起始计时，消除 render 耗时导致的漂移。

use std::path::Path;
use std::time::{Duration, Instant};

use crate::cli::SendArgs;
use crate::common::constants::EOT_REPEAT;
use crate::common::error::{OptransError, OptransResult};
use crate::common::types::{
    BlockData, GrayEncodingConfig,
};
use crate::encoder::frame::Frame;
use crate::encoder::render::render_frame_default;
use crate::sender::window::SenderWindow;

/// 首帧 / 末帧驻留时间（ms）
const BOUNDARY_DWELL_MS: u64 = 500;

/// 将字节数组格式化为十六进制字符串
fn hex_str(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect::<Vec<_>>().join("")
}

/// 根据屏幕尺寸和编码参数，计算每帧能容纳的最大载荷字节数
///
/// 从屏幕尺寸反推容量，而非从固定的 BLOCK_SIZE 正推帧尺寸。
fn calculate_frame_capacity(
    module_pixels: u32,
    gray_config: &GrayEncodingConfig,
    frame_margin: u32,
) -> (u32, u32, usize) {
    let screen_w = unsafe { crate::win32::ffi::GetSystemMetrics(crate::win32::ffi::SM_CXSCREEN) };
    let screen_h = unsafe { crate::win32::ffi::GetSystemMetrics(crate::win32::ffi::SM_CYSCREEN) };

    let modules_w = ((screen_w as u32).saturating_sub(frame_margin * 2)) / module_pixels;
    let modules_h = ((screen_h as u32).saturating_sub(frame_margin * 2)) / module_pixels;

    let (m_w, m_h, payload) = frame_capacity_for_modules(modules_w, modules_h, gray_config);
    (m_w, m_h, payload)
}

/// 核心容量计算：给定模块尺寸和编码参数，返回有效帧尺寸和最大 payload
fn frame_capacity_for_modules(
    modules_w: u32,
    modules_h: u32,
    gray_config: &GrayEncodingConfig,
) -> (u32, u32, usize) {
    use crate::common::constants::{RS_DATA_LEN, RS_TOTAL_LEN};

    // 确保最小尺寸（至少能容纳格式信息区：2×(w-14) ≥ 360，所以 w ≥ 194）
    let min_w = 194u32;
    let modules_w = modules_w.max(min_w);
    let modules_h = modules_h.max(50u32);

    let total = modules_w * modules_h;
    let finder_modules = 3 * 49u32;
    let format_modules = 2 * (modules_w.saturating_sub(14));
    let available_data_modules = total.saturating_sub(finder_modules + format_modules);

    let gray_levels = gray_config.levels;
    let available = available_data_modules as usize;
    let max_rs_encoded: usize = match gray_levels {
        2 => available / 8,
        4 => available / 4,
        5 => { let v = (available * 2) / 7; v.saturating_sub(1) }
        8 => (available * 3) / 8,
        16 => available / 2,
        _ => available / 8,
    };

    let header = 4usize;
    let max_payload = if max_rs_encoded <= header {
        0
    } else {
        let num_rs_blocks = (max_rs_encoded - header) / RS_TOTAL_LEN;
        num_rs_blocks * RS_DATA_LEN
    };

    (modules_w, modules_h, max_payload)
}

/// 执行发送操作的主入口
pub fn run_send(args: SendArgs) -> OptransResult<()> {
    let module_pixels = args.module_pixels;
    let interval_ms = args.interval_ms;
    let gray_levels = args.gray_levels;
    let _enable_diff = args.enable_diff;
    let enable_heartbeat = args.enable_heartbeat;
    let _i_frame_interval = args.i_frame_interval;

    // 验证灰度级数
    let valid_levels = crate::common::constants::VALID_GRAY_LEVELS;
    if !valid_levels.contains(&gray_levels) {
        eprintln!(
            "[optrans] 警告: 灰度级数 {} 不在推荐范围 {:?}，继续使用",
            gray_levels, valid_levels
        );
    }

    // 灰度编码配置
    let gray_config = GrayEncodingConfig::new(gray_levels);
    // encoding_mode_bit: 2级灰度=二值模式=bit0, 其他=灰度模式=bit1
    let encoding_mode_bit: u8 = if gray_levels == 2 { 0 } else { 1 };

    eprintln!(
        "[optrans] 灰度配置: {} 级, 编码模式位={}",
        gray_levels, encoding_mode_bit
    );

    // 1. 根据屏幕尺寸计算每帧最大容量
    let (modules_w, modules_h, max_payload_per_frame) = calculate_frame_capacity(
        module_pixels,
        &gray_config,
        args.frame_margin,
    );

    if max_payload_per_frame <= 4 {
        return Err(OptransError::InvalidArgument(
            "屏幕尺寸不足以容纳任何数据帧，请增大窗口或减小 module_pixels".to_string()
        ));
    }

    // 多块合并格式每块 overhead：2B count + (4B index + 2B len) × N
    // 最少 1 块 → 8B overhead → block_size = max_payload - 8
    let block_size = max_payload_per_frame.saturating_sub(8);

    // 2. 读取文件并分块
    let manifest = crate::sender::chunker::read_file_with_block_size(&Path::new(&args.path), block_size)?;

    eprintln!(
        "[optrans] 文件: {} ({} bytes, {} 块, 每块≤{}B)",
        manifest.file_name, manifest.file_size, manifest.blocks.len(), block_size
    );
    eprintln!("[optrans] SHA-256: {}", hex_str(&manifest.sha256));

    let total_blocks = manifest.blocks.len() as u32;

    // 3. 预构建元数据帧和 EOT 帧（始终 I 帧、二值模式）
    let meta_frame = Frame::new_metadata(&manifest, 0)?;

    let eot_seq = 1u16 + total_blocks as u16; // meta(seq=0) + data blocks
    let eot_frame = Frame::new_eot(eot_seq)?;

    // 4. 创建全屏最大化窗口
    let mut window = SenderWindow::create_sized(module_pixels, modules_w, modules_h, args.frame_margin)?;

    eprintln!(
        "[optrans] 帧尺寸: {}w×{}h modules ({}×{}px, max_payload={}B)",
        modules_w, modules_h,
        modules_w * module_pixels, modules_h * module_pixels,
        max_payload_per_frame
    );
    eprintln!("[optrans] 开始发送...");

    let mut global_seq: u16 = 0;

    // 元数据 payload 大小
    let meta_payload = {
        let mut p = Vec::new();
        p.push(manifest.file_name.as_bytes().len() as u8);
        p.extend_from_slice(manifest.file_name.as_bytes());
        p.extend_from_slice(&manifest.file_size.to_le_bytes());
        p.extend_from_slice(&manifest.sha256);
        p.extend_from_slice(&(manifest.blocks.len() as u32).to_le_bytes());
        p
    };
    let meta_payload_len = meta_payload.len();

    // 检查是否可以将元数据合并到第一个数据帧（小文件优化）
    let can_merge_meta = total_blocks == 1
        && 2 + 4 + 2 + meta_payload_len + 4 + 2 + manifest.blocks[0].data.len() <= max_payload_per_frame;

    // 单轮发送：不重传，靠单帧高可靠解码（参考二维码识别的容错思路：
    // 数据块自带 RS 纠错，格式信息解码失败时接收端降级重试平移/变半径）
    {
        if !window.process_messages() {
            eprintln!("[optrans] 用户中止发送");
            return Ok(());
        }

        if can_merge_meta {
            // 小文件：元数据 + 数据合并到一个帧，直接进入数据发送
            eprintln!("[optrans] ▶ 小文件模式：元数据+数据合并为单帧，开始发送...");
        } else {
            // 先发送元数据帧（始终 I 帧、二值渲染）
            {
                if !window.process_messages() {
                    eprintln!("[optrans] 用户中止发送");
                    return Ok(());
                }
                let meta_gray_config = GrayEncodingConfig::new(2); // 元数据帧始终二值
                let pixels = render_frame_default(&meta_frame, module_pixels, modules_w, modules_h, &meta_gray_config, false, None);
                window.update_bitmap(&pixels);
                eprintln!("  [META] seq={}", meta_frame.sequence());
                global_seq += 1;

                // 元数据帧已显示，短暂驻留后开始发送数据
                eprintln!("[optrans] □ 确认帧已显示，{}ms 后开始发送...", BOUNDARY_DWELL_MS);
                if !window.sleep_with_messages(BOUNDARY_DWELL_MS) {
                    eprintln!("[optrans] 用户中止发送");
                    return Ok(());
                }
                eprintln!("[optrans] ▶ 开始发送数据帧");
            }
        }

        // 合并块发送：将多个块打包到一帧，直到 payload 接近容量上限
        let mut block_iter = manifest.blocks.iter().peekable();
        let mut frame_idx: usize = 0;

        while block_iter.peek().is_some() || (can_merge_meta && frame_idx == 0) {
            let frame_start = Instant::now();

            if !window.process_messages() {
                eprintln!("[optrans] 用户中止发送");
                return Ok(());
            }

            // 收集本帧的块，直到 payload 填满或块用完
            let mut merged_blocks: Vec<&BlockData> = Vec::new();
            let mut accumulated = 2usize; // count prefix: 2B

            // 合并模式：第一个"块"是元数据
            if can_merge_meta && frame_idx == 0 {
                accumulated += 4 + 2 + meta_payload_len; // index(4) + len(2) + meta_data
            }

            while let Some(block) = block_iter.peek() {
                let entry_size = 4 + 2 + block.data.len(); // index(4) + len(2) + data
                if accumulated + entry_size > max_payload_per_frame {
                    break; // 当前帧已满
                }
                accumulated += entry_size;
                merged_blocks.push(block);
                block_iter.next();
            }

            if merged_blocks.is_empty() && !(can_merge_meta && frame_idx == 0) {
                break;
            }

            // 构建帧
            let frame = if can_merge_meta && frame_idx == 0 {
                // 元数据+数据合并帧：用特殊 block_index 标识元数据块
                Frame::new_merged_with_meta(global_seq, total_blocks, encoding_mode_bit,
                    &meta_payload, &merged_blocks)?
            } else {
                Frame::new_data_merged(
                    &merged_blocks, global_seq, total_blocks, encoding_mode_bit,
                )?
            };

            // 渲染帧
            let pixels = render_frame_default(
                &frame, module_pixels, modules_w, modules_h,
                &gray_config, enable_heartbeat, None,
            );

            window.update_bitmap(&pixels);

            let count = merged_blocks.len();
            let first_idx = merged_blocks[0].index;
            let last_idx = merged_blocks[count - 1].index;
            eprintln!(
                "  [{}/{}] seq={} blocks={}-{} payload={}B ({:.0}%)",
                frame_idx + 1, total_blocks as usize,
                global_seq, first_idx, last_idx,
                accumulated,
                accumulated as f64 / max_payload_per_frame as f64 * 100.0
            );

            frame_idx += 1;
            global_seq += 1;

            // v0.19.2: 从帧起始计时，确保严格的帧间隔
            // render耗时浮动 (5~100ms) 不再累积漂移
            let elapsed = frame_start.elapsed();
            let interval = Duration::from_millis(interval_ms);
            if elapsed < interval {
                let remaining = (interval - elapsed).as_millis() as u64;
                if !window.sleep_with_messages(remaining) {
                    eprintln!("[optrans] 用户中止发送");
                    return Ok(());
                }
            }
        }
    }

    // 6. 发送 EOT 帧
    eprintln!("[optrans] 发送 EOT 信号...");
    for i in 0..EOT_REPEAT {
        if !window.process_messages() {
            eprintln!("[optrans] 用户中止发送");
            return Ok(());
        }

        let pixels = render_frame_default(&eot_frame, module_pixels, modules_w, modules_h, &gray_config, false, None);
        window.update_bitmap(&pixels);
        eprintln!(
            "  [EOT {}/{}] seq={}",
            i + 1,
            EOT_REPEAT,
            eot_frame.sequence()
        );

        if !window.sleep_with_messages(interval_ms) {
            eprintln!("[optrans] 用户中止发送");
            return Ok(());
        }
    }

    // 7. EOT 帧驻留 500ms，确保接收端有充足时间检测
    eprintln!("[optrans] EOT 帧驻留 {}ms，等待接收端确认...", BOUNDARY_DWELL_MS);
    let dwell_start = unsafe { crate::win32::ffi::GetTickCount() };
    loop {
        if !window.process_messages() {
            break;
        }
        if (unsafe { crate::win32::ffi::GetTickCount() } - dwell_start) >= BOUNDARY_DWELL_MS as u32 {
            break;
        }
        unsafe { crate::win32::ffi::Sleep(50) };
    }
    eprintln!("[optrans] 发送完成，窗口即将关闭");

    // window 的 Drop 实现会自动调用 DestroyWindow 关闭窗口
    Ok(())
}
