//! 发送编排模块
//!
//! 发送主控逻辑：读取文件 → I/P 帧决策 → 动态帧构建 → 灰度渲染 → 循环显示 → EOT 信号。
//! v0.2 新增：I/P 帧交叉、灰度编码支持、心跳控制。
//! v0.5: 支持长方形全屏，传递 (modules_w, modules_h) 双维度。

use std::path::Path;

use crate::cli::SendArgs;
use crate::common::constants::{
    EOT_REPEAT,
    P_FRAME_DIFF_RATIO_THRESHOLD,
};
use crate::common::error::{OptransError, OptransResult};
use crate::common::types::{
    DiffEntry, FrameKind, GrayEncodingConfig,
};
use crate::encoder::frame::Frame;
use crate::encoder::render::{encode_gray_n, render_frame_default};
use crate::sender::window::SenderWindow;

/// 发送端 I 帧缓存
#[derive(Debug, Clone)]
struct SenderCache {
    /// 最近一个 I 帧的数据模块灰度值序列（行优先排列）
    i_frame_modules: Vec<u8>,
    /// 最近 I 帧的 frame_sequence
    i_frame_seq: u16,
}

/// I/P 帧决策
///
/// # 参数
/// - `seq`: 当前帧序号
/// - `frame_index_in_round`: 本轮中的帧索引（第一个数据帧为 0）
/// - `p_frame_count_since_last_i`: 自上次 I 帧以来的连续 P 帧数
/// - `i_frame_interval`: 强制 I 帧间隔
/// - `diff_ratio`: diff 模块占比
fn decide_frame_kind(
    seq: u16,
    frame_index_in_round: usize,
    p_frame_count_since_last_i: u32,
    i_frame_interval: u32,
    diff_ratio: f64,
) -> FrameKind {
    // 第 1 帧始终是 I 帧
    if seq == 0 {
        return FrameKind::I;
    }
    // 每轮第 1 个数据帧强制 I 帧
    if frame_index_in_round == 0 {
        return FrameKind::I;
    }
    // 超过 I 帧间隔强制刷新
    if p_frame_count_since_last_i >= i_frame_interval {
        return FrameKind::I;
    }
    // diff 比例过高时退化为 I 帧
    if diff_ratio > P_FRAME_DIFF_RATIO_THRESHOLD {
        return FrameKind::I;
    }
    // 否则发 P 帧
    FrameKind::P
}

/// 计算当前模块与缓存之间的差异
///
/// # 返回
/// diff 条目列表 + diff 比例
fn compute_diff(
    current_modules: &[u8],
    cache: &SenderCache,
) -> (Vec<DiffEntry>, f64) {
    let total = current_modules.len().min(cache.i_frame_modules.len());
    if total == 0 {
        return (Vec::new(), 0.0);
    }

    let mut entries: Vec<DiffEntry> = Vec::new();

    for i in 0..total {
        if cache.i_frame_modules[i] != current_modules[i] {
            entries.push(DiffEntry {
                module_index: i as u16,
                module_value: current_modules[i],
            });
        }
    }

    let ratio = entries.len() as f64 / total as f64;
    (entries, ratio)
}

/// 根据屏幕尺寸和编码参数，计算每帧能容纳的最大载荷字节数
///
/// 从屏幕尺寸反推容量，而非从固定的 BLOCK_SIZE 正推帧尺寸。
fn calculate_frame_capacity(
    module_pixels: u32,
    gray_config: &GrayEncodingConfig,
) -> (u32, u32, usize) {
    use crate::common::constants::FRAME_MARGIN_PX;

    let screen_w = unsafe { crate::win32::ffi::GetSystemMetrics(crate::win32::ffi::SM_CXSCREEN) };
    let screen_h = unsafe { crate::win32::ffi::GetSystemMetrics(crate::win32::ffi::SM_CYSCREEN) };

    let modules_w = ((screen_w as u32).saturating_sub(FRAME_MARGIN_PX * 2)) / module_pixels;
    let modules_h = ((screen_h as u32).saturating_sub(FRAME_MARGIN_PX * 2)) / module_pixels;

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

/// 将单个 block 的 RS 编码数据渲染为数据模块灰度值序列
fn block_to_gray_modules(
    payload: &[u8],
    modules_w: usize,
    modules_h: usize,
    gray_config: &GrayEncodingConfig,
) -> Vec<u8> {
    let total_modules = modules_w * modules_h;
    let finder_modules = 3 * (7 * 7);
    let format_modules = 2 * (modules_w - 14);
    let max_data_modules = total_modules - finder_modules - format_modules;

    // v0.6: 应用数据掩码（XOR），与 render.rs 的 build_data_gray_sequence 保持一致
    let masked_payload = crate::common::mask::apply_mask(payload);
    let (gray_values, _) = encode_gray_n(&masked_payload, gray_config);
    gray_values.into_iter().take(max_data_modules).collect()
}

/// 执行发送操作的主入口
pub fn run_send(args: SendArgs) -> OptransResult<()> {
    let module_pixels = args.module_pixels;
    let rounds = args.rounds;
    let interval_ms = args.interval_ms;
    let gray_levels = args.gray_levels;
    let enable_diff = args.enable_diff;
    let enable_heartbeat = args.enable_heartbeat;
    let i_frame_interval = args.i_frame_interval;

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
    );

    if max_payload_per_frame <= 4 {
        return Err(OptransError::InvalidArgument(
            "屏幕尺寸不足以容纳任何数据帧，请增大窗口或减小 module_pixels".to_string()
        ));
    }

    // max_payload 含 block_index(4B)，数据块大小 = max_payload - 4
    let block_size = max_payload_per_frame - 4;

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
    let mut window = SenderWindow::create_sized(module_pixels, modules_w, modules_h)?;

    let n_w = modules_w as usize;
    let n_h = modules_h as usize;

    eprintln!(
        "[optrans] 帧尺寸: {}w×{}h modules ({}×{}px, max_payload={}B)",
        modules_w, modules_h,
        modules_w * module_pixels, modules_h * module_pixels,
        max_payload_per_frame
    );
    eprintln!("[optrans] 开始发送...");

    // 4. 维护发送端缓存和状态
    let mut cache: Option<SenderCache> = None;
    let mut global_seq: u16 = 0; // 跨轮全局序号

    // 5. 循环发送
    for round in 0..rounds {
        if !window.process_messages() {
            eprintln!("[optrans] 用户中止发送");
            return Ok(());
        }

        eprintln!("[optrans] --- 第 {} 轮 (共 {} 轮) ---", round + 1, rounds);

        // 每轮先发送元数据帧（始终 I 帧、二值渲染）
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

            // 首轮：元数据帧已显示，3 秒倒计时后自动开始发送数据
            if round == 0 {
                eprintln!("[optrans] □ 确认帧已显示，3 秒后自动开始发送...");
                for i in (1..=3).rev() {
                    eprintln!("[optrans]   {}...", i);
                    if !window.sleep_with_messages(1000) {
                        eprintln!("[optrans] 用户中止发送");
                        return Ok(());
                    }
                }
                eprintln!("[optrans] ▶ 开始发送数据帧");
            } else {
                if !window.sleep_with_messages(interval_ms) {
                    eprintln!("[optrans] 用户中止发送");
                    return Ok(());
                }
            }
        }

        // 每轮开始时重置 P 帧计数（首数据帧强制 I）
        let mut p_frame_count_since_last_i: u32 = 0;

        for (frame_idx, block) in manifest.blocks.iter().enumerate() {
            if !window.process_messages() {
                eprintln!("[optrans] 用户中止发送");
                return Ok(());
            }

            // 当前块的 RS 编码数据
            let payload_len = 4 + block.data.len();
            let mut payload = Vec::with_capacity(payload_len);
            payload.extend_from_slice(&block.index.to_le_bytes());
            payload.extend_from_slice(&block.data);

            let rs_encoder = crate::encoder::reed_solomon::RsEncoder::new();
            let encoded_payload = rs_encoder.encode(&payload)?;

            // 渲染当前块的灰度模块值序列
            let current_modules = block_to_gray_modules(&encoded_payload, n_w, n_h, &gray_config);

            // I/P 帧决策
            let frame_kind = if enable_diff {
                if let Some(ref c) = cache {
                    let (_, diff_ratio) = compute_diff(&current_modules, c);
                    decide_frame_kind(
                        global_seq,
                        frame_idx,
                        p_frame_count_since_last_i,
                        i_frame_interval,
                        diff_ratio,
                    )
                } else {
                    FrameKind::I // 无缓存时首帧必须是 I
                }
            } else {
                FrameKind::I // 禁用差分，全 I 帧
            };

            // 按帧种类构建帧
            let (frame, p_entries) = match frame_kind {
                FrameKind::I => {
                    // I 帧：全量编码
                    let frame = Frame::new_data(block, global_seq, total_blocks, encoding_mode_bit)?;

                    // 更新 I 帧缓存
                    cache = Some(SenderCache {
                        i_frame_modules: current_modules,
                        i_frame_seq: global_seq,
                    });
                    p_frame_count_since_last_i = 0;

                    (frame, None)
                }
                FrameKind::P => {
                    // P 帧：差分编码
                    let (diff_entries, _) = if let Some(ref c) = cache {
                        compute_diff(&current_modules, c)
                    } else {
                        (Vec::new(), 0.0)
                    };

                    let frame = Frame::new_p_frame(
                        &diff_entries,
                        global_seq,
                        cache.as_ref().map(|c| c.i_frame_seq).unwrap_or(0),
                        block.index,
                        total_blocks,
                        encoding_mode_bit,
                    )?;

                    p_frame_count_since_last_i += 1;

                    let entries_clone = diff_entries.clone();
                    (frame, Some(entries_clone))
                }
            };

            // 渲染帧
            let pixels = render_frame_default(
                &frame,
                module_pixels,
                modules_w,
                modules_h,
                &gray_config,
                enable_heartbeat,
                p_entries.as_deref(),
            );

            // 更新窗口显示
            window.update_bitmap(&pixels);

            // 打印进度
            let frame_kind_str = match frame_kind {
                FrameKind::I => "I",
                FrameKind::P => "P",
            };
            eprintln!(
                "  [{}/{}] seq={} type={} kind={}",
                frame_idx + 1,
                manifest.blocks.len(),
                global_seq,
                match frame.frame_type() {
                    crate::common::types::FrameType::Meta => "META",
                    crate::common::types::FrameType::Data => "DATA",
                    crate::common::types::FrameType::EOT => "EOT",
                },
                frame_kind_str
            );

            global_seq += 1;

            // 等待帧间隔
            if !window.sleep_with_messages(interval_ms) {
                eprintln!("[optrans] 用户中止发送");
                return Ok(());
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

    // 7. EOT 帧驻留 3 秒，确保接收端有充足时间检测
    eprintln!("[optrans] EOT 帧驻留 3 秒，等待接收端确认...");
    let dwell_start = unsafe { crate::win32::ffi::GetTickCount() };
    loop {
        if !window.process_messages() {
            break;
        }
        if (unsafe { crate::win32::ffi::GetTickCount() } - dwell_start) >= 3000 {
            break;
        }
        unsafe { crate::win32::ffi::Sleep(50) };
    }
    eprintln!("[optrans] 发送完成，窗口即将关闭");

    // window 的 Drop 实现会自动调用 DestroyWindow 关闭窗口
    Ok(())
}

/// 将字节切片格式化为十六进制字符串
fn hex_str(data: &[u8]) -> String {
    data
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join("")
}
