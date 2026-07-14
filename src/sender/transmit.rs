//! 发送编排模块
//!
//! 发送主控逻辑：读取文件 → 块合并 → 帧构建 → 灰度渲染 → 循环显示 → EOT。
//! v0.9: 多块合并发送，每帧填满 payload 容量。
//! v0.19.3: render 线程异步预渲染，display loop 严格串行计时。

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use crate::common::constants;
use crate::common::error::{OptransError, OptransResult};
use crate::common::types::{BlockData, GrayEncodingConfig};
use crate::encoder::frame::Frame;
use crate::encoder::render::render_frame_default;
use crate::sender::window::SenderWindow;

use log::{debug, info, warn};

/// 首帧 / 末帧驻留时间（ms）
const BOUNDARY_DWELL_MS: u64 = 100;

/// 将字节数组格式化为十六进制字符串
fn hex_str(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join("")
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
    use crate::common::rs_utils::rs_payload_capacity;

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
        8 => (available * 3) / 8,
        16 => available / 2,
        _ => available / 8,
    };

    let max_payload = rs_payload_capacity(max_rs_encoded);

    (modules_w, modules_h, max_payload)
}

/// 执行发送操作的主入口
pub fn run_send(path: PathBuf) -> OptransResult<()> {
    let module_pixels = constants::MODULE_PIXELS_DEFAULT;
    let interval_ms = constants::FRAME_INTERVAL_MS;
    let gray_levels = constants::GRAY_LEVELS_DEFAULT;
    let frame_margin = constants::FRAME_MARGIN_PX_DEFAULT;
    let enable_heartbeat = true;
    let i_frame_interval = constants::I_FRAME_INTERVAL_DEFAULT;

    // 验证灰度级数
    let valid_levels = crate::common::constants::VALID_GRAY_LEVELS;
    if !valid_levels.contains(&gray_levels) {
        warn!(
            "警告: 灰度级数 {} 不在推荐范围 {:?}，继续使用",
            gray_levels, valid_levels
        );
    }

    // 灰度编码配置
    let gray_config = GrayEncodingConfig::new(gray_levels);
    // encoding_mode_bit: 2级灰度=二值模式=bit0, 其他=灰度模式=bit1
    let encoding_mode_bit: u8 = if gray_levels == 2 { 0 } else { 1 };

    debug!(
        "灰度配置: {} 级, 编码模式位={}",
        gray_levels, encoding_mode_bit
    );

    // 1. 根据屏幕尺寸计算每帧最大容量
    let (modules_w, modules_h, max_payload_per_frame) =
        calculate_frame_capacity(module_pixels, &gray_config, frame_margin);

    if max_payload_per_frame <= 4 {
        return Err(OptransError::InvalidArgument(
            "屏幕尺寸不足以容纳任何数据帧，请增大窗口或减小 module_pixels".to_string(),
        ));
    }

    // 多块合并格式每块 overhead：2B count + (4B index + 4B len) × N
    // 最少 1 块 → 10B overhead → block_size = max_payload - 10
    let block_size = max_payload_per_frame.saturating_sub(10);

    // 2. 读取文件并分块
    let manifest =
        crate::sender::chunker::read_file_with_block_size(Path::new(&path), block_size)?;

    info!(
        "文件: {} ({} bytes, {} 块, 每块≤{}B)",
        manifest.file_name,
        manifest.file_size,
        manifest.blocks.len(),
        block_size
    );
    info!("SHA-256: {}", hex_str(&manifest.sha256));

    let total_blocks = manifest.blocks.len() as u32;

    // 3. 预构建元数据帧和 EOT 帧（始终 I 帧、二值模式）
    let meta_frame = Frame::new_metadata(&manifest, 0)?;

    let eot_seq = 1u16 + total_blocks as u16; // meta(seq=0) + data blocks
    let eot_frame = Frame::new_eot(eot_seq)?;

    // 4. 创建全屏最大化窗口
    let mut window =
        SenderWindow::create_sized(module_pixels, modules_w, modules_h, frame_margin)?;

    info!(
        "帧尺寸: {}w×{}h modules ({}×{}px, max_payload={}B)",
        modules_w,
        modules_h,
        modules_w * module_pixels,
        modules_h * module_pixels,
        max_payload_per_frame
    );
    info!("开始发送...");

    let mut global_seq: u16 = 0;

    // 元数据 payload 大小
    let meta_payload = {
        let mut p = Vec::new();
        p.push(manifest.file_name.len() as u8);
        p.extend_from_slice(manifest.file_name.as_bytes());
        p.extend_from_slice(&manifest.file_size.to_le_bytes());
        p.extend_from_slice(&manifest.sha256);
        p.extend_from_slice(&(manifest.blocks.len() as u32).to_le_bytes());
        p
    };
    let meta_payload_len = meta_payload.len();

    // 检查是否可以将元数据合并到第一个数据帧（小文件优化）
    let can_merge_meta = total_blocks == 1
        && 2 + 4 + 4 + meta_payload_len + 4 + 4 + manifest.blocks[0].data.len()
            <= max_payload_per_frame;

    // 单轮发送：不重传，靠单帧高可靠解码（参考二维码识别的容错思路：
    // 数据块自带 RS 纠错，格式信息解码失败时接收端降级重试平移/变半径）
    {
        if !window.process_messages() {
            info!("用户中止发送");
            return Ok(());
        }

        if can_merge_meta {
            // 小文件：元数据 + 数据合并到一个帧，直接进入数据发送
            info!("▶ 小文件模式：元数据+数据合并为单帧，开始发送...");
        } else {
            // 先发送元数据帧（始终 I 帧、二值渲染）
            {
                if !window.process_messages() {
                    info!("用户中止发送");
                    return Ok(());
                }
                let meta_gray_config = GrayEncodingConfig::new(2); // 元数据帧始终二值
                let pixels = render_frame_default(
                    &meta_frame,
                    module_pixels,
                    modules_w,
                    modules_h,
                    &meta_gray_config,
                    false,
                    None,
                );
                window.update_bitmap(&pixels);
                debug!("  [META] seq={}", meta_frame.sequence());
                global_seq += 1;

                // 元数据帧已显示，短暂驻留后开始发送数据
                info!(
                    "□ 确认帧已显示，{}ms 后开始发送...",
                    BOUNDARY_DWELL_MS
                );
                if !window.sleep_with_messages(BOUNDARY_DWELL_MS) {
                    info!("用户中止发送");
                    return Ok(());
                }
                info!("▶ 开始发送数据帧");
            }
        }

        // ── 阶段 1: 收集所有数据帧 (Frame 构建，不渲染) ──
        let mut block_iter = manifest.blocks.iter().peekable();
        let mut frame_idx: usize = 0;
        let mut data_frames: Vec<Frame> = Vec::new();

        while block_iter.peek().is_some() || (can_merge_meta && frame_idx == 0) {
            let mut merged_blocks: Vec<&BlockData> = Vec::new();
            let mut accumulated = 2usize;

            if can_merge_meta && frame_idx == 0 {
                accumulated += 4 + 4 + meta_payload_len;
            }

            while let Some(block) = block_iter.peek() {
                let entry_size = 4 + 4 + block.data.len();
                if accumulated + entry_size > max_payload_per_frame {
                    break;
                }
                accumulated += entry_size;
                merged_blocks.push(block);
                block_iter.next();
            }

            if merged_blocks.is_empty() && !(can_merge_meta && frame_idx == 0) {
                break;
            }

            let frame = if can_merge_meta && frame_idx == 0 {
                Frame::new_merged_with_meta(
                    global_seq,
                    total_blocks,
                    encoding_mode_bit,
                    &meta_payload,
                    &merged_blocks,
                )?
            } else {
                Frame::new_data_merged(&merged_blocks, global_seq, total_blocks, encoding_mode_bit)?
            };

            data_frames.push(frame);
            frame_idx += 1;
            global_seq += 1;
        }

        // ── P 帧差分优化 ──
        // 对多帧数据传输，只有首帧作为 I 帧（全量），后续帧通过 diff 转为 P 帧。
        // P 帧只渲染变化模块，payload 极小 → 渲染快 → 降低漏帧风险。
        let gray_config_p = gray_config.clone();
        let n_w = modules_w as usize;
        let n_h = modules_h as usize;
        let mut i_frame_cache: Vec<u8> = Vec::new(); // 缓存最近 I 帧的灰度序列
        let mut p_since_last_i: u32 = 0;

        // 预计算每帧的灰度序列
        let gray_seqs: Vec<Vec<u8>> = data_frames.iter().map(|f| {
            if f.encoded_payload.is_empty() {
                Vec::new()
            } else {
                crate::encoder::render::build_data_gray_sequence(
                    &f.encoded_payload, n_w, n_h, &gray_config_p,
                )
            }
        }).collect();

        // 重建帧列表，决策 I/P
        let mut render_frames: Vec<(Frame, Option<Vec<crate::common::types::DiffEntry>>)> = Vec::new();

        for (i, frame) in data_frames.into_iter().enumerate() {
            if p_since_last_i == 0 || p_since_last_i >= i_frame_interval as u32
                || i_frame_cache.is_empty()
            {
                // 强制 I 帧
                i_frame_cache = gray_seqs[i].clone();
                p_since_last_i = 0;
                render_frames.push((frame, None));
            } else {
                // 尝试 P 帧：比较当前灰度序列与 I 帧缓存的差异
                let cur = &gray_seqs[i];
                let base = &i_frame_cache;
                let min_len = cur.len().min(base.len());
                let mut diff_count: usize = 0;
                let mut diff_entries: Vec<crate::common::types::DiffEntry> = Vec::new();
                for j in 0..min_len {
                    if cur[j] != base[j] {
                        diff_count += 1;
                        diff_entries.push(crate::common::types::DiffEntry {
                            module_index: j as u16,
                            module_value: cur[j],
                        });
                    }
                }
                // 超过 min_len 的部分全部作为 diff
                for j in min_len..cur.len() {
                    diff_entries.push(crate::common::types::DiffEntry {
                        module_index: j as u16,
                        module_value: cur[j],
                    });
                }

                // 如果 diff 超过总模块的 70%，退化为 I 帧
                if diff_count > (min_len * 70 / 100) {
                    i_frame_cache = cur.clone();
                    p_since_last_i = 0;
                    render_frames.push((frame, None));
                } else {
                    p_since_last_i += 1;
                    render_frames.push((frame, Some(diff_entries)));
                }
            }
        }

        let total_frames = render_frames.len();
        let p_count = render_frames.iter().filter(|(_, d)| d.is_some()).count();
        let i_count = total_frames - p_count;
        info!("▶ 已构建 {} 帧（{}I + {}P），启动渲染线程...", total_frames, i_count, p_count);

        // v0.19.4: 提前释放 manifest（含原始文件数据拷贝），渲染阶段仅保留 RS 编码的帧数据
        drop(manifest);
        drop(meta_payload);

        // ── 阶段 2: 渲染线程 (异步) + display loop (严格串行) ──
        let (render_tx, render_rx) = mpsc::channel::<Vec<u8>>();

        let render_gray = gray_config.clone();
        thread::spawn(move || {
            for (frame, diff_entries) in &render_frames {
                let p_entries: Option<&[crate::common::types::DiffEntry]> =
                    diff_entries.as_deref();
                let pixels = render_frame_default(
                    frame,
                    module_pixels,
                    modules_w,
                    modules_h,
                    &render_gray,
                    enable_heartbeat,
                    p_entries,
                );
                if render_tx.send(pixels).is_err() {
                    break;
                }
            }
            // render_tx dropped → display loop 的 rx.recv() 返回 Err
        });

        // display loop: 严格串行，每帧 interval_ms
        let mut last_frame_time = unsafe { crate::win32::ffi::GetTickCount() };
        for (i, pixels) in render_rx.into_iter().enumerate() {
            if !window.process_messages() {
                info!("用户中止发送");
                return Ok(());
            }

            // 诊断：记录帧间隔（v0.19.4）
            let now = unsafe { crate::win32::ffi::GetTickCount() };
            let actual_interval = now.wrapping_sub(last_frame_time);
            if actual_interval > (interval_ms as u32) * 2 {
                debug!(
                    "  [帧间隔] 第 {} 帧实际间隔 {}ms（期望 {}ms），可能丢帧！",
                    i + 1,
                    actual_interval,
                    interval_ms
                );
            }
            last_frame_time = now;

            window.update_bitmap(&pixels);
            if !window.sleep_with_messages(interval_ms) {
                info!("用户中止发送");
                return Ok(());
            }
        }
    }

    // 6. 发送 EOT 帧
    info!("发送 EOT 信号...");
    if !window.process_messages() {
        info!("用户中止发送");
        return Ok(());
    }

    let pixels = render_frame_default(
        &eot_frame,
        module_pixels,
        modules_w,
        modules_h,
        &gray_config,
        false,
        None,
    );
    window.update_bitmap(&pixels);
    debug!("  [EOT] seq={}", eot_frame.sequence());

    // 7. EOT 帧驻留，确保接收端有充足时间检测
    info!(
        "EOT 帧驻留 {}ms，等待接收端确认...",
        BOUNDARY_DWELL_MS
    );
    let dwell_start = unsafe { crate::win32::ffi::GetTickCount() };
    loop {
        if !window.process_messages() {
            break;
        }
        if (unsafe { crate::win32::ffi::GetTickCount() } - dwell_start) >= BOUNDARY_DWELL_MS as u32
        {
            break;
        }
        unsafe { crate::win32::ffi::Sleep(50) };
    }
    info!("发送完成，窗口即将关闭");

    // window 的 Drop 实现会自动调用 DestroyWindow 关闭窗口
    Ok(())
}
