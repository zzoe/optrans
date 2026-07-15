//! 发送编排模块
//!
//! 发送主控逻辑：读取文件 → 块级 FEC 编码 → 帧构建 → 灰度渲染 → 循环显示 → EOT。
//!
//! v0.26: 块级 FEC（前向纠错）。数据块分组后生成 RS 校验分片，
//! 数据帧与校验帧交错发送，接收端收到任意 k 个分片即可恢复该组。

use std::path::{Path, PathBuf};
use std::thread;

use crate::common::constants;
use crate::common::error::{OptransError, OptransResult};
use crate::common::fec::{FecEncoder, fec_group_count};
use crate::common::types::{BlockData, FileManifest, GrayEncodingConfig, MetaPayload};
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
    use crate::common::reed_solomon_utils::rs_payload_capacity;

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
    let manifest = crate::sender::chunker::read_file_with_block_size(Path::new(&path), block_size)?;

    let total_blocks = manifest.blocks.len() as u32;
    let fec_k = constants::FEC_DATA_SHARDS as u16;
    let fec_m = constants::FEC_PARITY_SHARDS as u16;
    let fec_groups = fec_group_count(manifest.blocks.len());
    // FEC 分片长度 = block_size（最后一块数据补零到此长度）
    let fec_shard_size = block_size as u32;

    info!(
        "文件: {} ({} bytes, {} 块, 每块≤{}B)",
        manifest.file_name, manifest.file_size, total_blocks, block_size
    );
    info!("SHA-256: {}", hex_str(&manifest.sha256));
    info!(
        "FEC: {} 组, k={} m={} (容错 {:.0}%), 分片长度={}B",
        fec_groups,
        fec_k,
        fec_m,
        fec_m as f64 / (fec_k + fec_m) as f64 * 100.0,
        fec_shard_size
    );

    // 3. 块级 FEC 编码：数据块 → 等长分片 → 校验分片
    let parity_shards = build_parity_shards(&manifest, block_size)?;

    // 4. 构建 MetaPayload（含 FEC 参数）
    let meta = MetaPayload {
        file_name: manifest.file_name.clone(),
        file_size: manifest.file_size,
        sha256: manifest.sha256,
        total_blocks,
        fec_k,
        fec_m,
        fec_groups,
        fec_shard_size,
    };

    // 5. 预构建元数据帧和 EOT 帧
    let meta_frame = Frame::new_metadata(&meta, 0)?;
    // EOT seq = 1(meta) + 数据帧数 + 校验帧数（wrapping 与 build_all_frames_interleaved 一致）
    let total_data_frames = count_data_frames(&manifest, max_payload_per_frame);
    let total_parity_frames = parity_shards.total_parity_count();
    let eot_seq = 1u16
        .wrapping_add(total_data_frames as u16)
        .wrapping_add(total_parity_frames as u16);
    let eot_frame = Frame::new_eot(eot_seq)?;

    // 6. 创建全屏最大化窗口
    let mut window = SenderWindow::create_sized(module_pixels, modules_w, modules_h, frame_margin)?;

    info!(
        "帧尺寸: {}w×{}h modules ({}×{}px, max_payload={}B)",
        modules_w,
        modules_h,
        modules_w * module_pixels,
        modules_h * module_pixels,
        max_payload_per_frame
    );
    info!(
        "开始发送: {} 数据帧 + {} 校验帧 = {} 总帧数",
        total_data_frames,
        total_parity_frames,
        total_data_frames + total_parity_frames
    );

    // ── 发送元数据帧（始终二值渲染） ──
    if !window.process_messages() {
        info!("用户中止发送");
        return Ok(());
    }
    {
        let meta_gray_config = GrayEncodingConfig::new(2);
        let pixels = render_frame_default(
            &meta_frame,
            module_pixels,
            modules_w,
            modules_h,
            &meta_gray_config,
            false,
        );
        window.update_bitmap(&pixels);
        debug!("  [META] seq={}", meta_frame.sequence());

        info!("□ 元数据帧已显示，{}ms 后开始发送...", BOUNDARY_DWELL_MS);
        if !window.wait_with_message_pump(BOUNDARY_DWELL_MS) {
            info!("用户中止发送");
            return Ok(());
        }
    }

    // ── 阶段 1: 构建所有帧（数据帧 + 校验帧，交错排列） ──
    let all_frames = build_all_frames_interleaved(
        &manifest,
        &parity_shards,
        total_blocks,
        encoding_mode_bit,
        max_payload_per_frame,
    )?;

    let total_frames = all_frames.len();
    info!("▶ 已构建 {} 帧（交错），启动渲染线程...", total_frames);

    // 提前释放 manifest（含原始文件数据拷贝）
    drop(manifest);

    // ── 阶段 2: 渲染线程 (异步) + display loop (严格串行) ──
    // 有界 channel（容量 2）：防止 display loop 变慢时渲染线程堆积像素缓冲区导致 OOM。
    // 容量 2 保证渲染线程在 display loop 处理当前帧时能并行渲染下一帧，同时背压阻止堆积。
    let (render_tx, render_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(2);

    let render_gray = gray_config.clone();
    thread::spawn(move || {
        for frame in &all_frames {
            let pixels = render_frame_default(
                frame,
                module_pixels,
                modules_w,
                modules_h,
                &render_gray,
                enable_heartbeat,
            );
            if render_tx.send(pixels).is_err() {
                break;
            }
        }
        // render_tx dropped → display loop 的 rx.recv() 返回 Err
    });

    // display loop: 严格串行，每帧 interval_ms
    for pixels in render_rx.into_iter() {
        if !window.process_messages() {
            info!("用户中止发送");
            return Ok(());
        }

        window.update_bitmap(&pixels);
        if !window.wait_with_message_pump(interval_ms) {
            info!("用户中止发送");
            return Ok(());
        }
    }

    // ── 发送 EOT 帧 ──
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
    );
    window.update_bitmap(&pixels);
    debug!("  [EOT] seq={}", eot_frame.sequence());

    // EOT 帧驻留，确保接收端有充足时间检测
    info!("EOT 帧驻留 {}ms，等待接收端确认...", BOUNDARY_DWELL_MS);
    window.wait_with_message_pump(BOUNDARY_DWELL_MS);
    info!("发送完成，窗口即将关闭");

    Ok(())
}

/// 计算数据块需要的帧数（多块合并）
fn count_data_frames(manifest: &FileManifest, max_payload_per_frame: usize) -> usize {
    let mut count = 0usize;
    let mut accumulated = 2usize; // 2B count header
    for block in &manifest.blocks {
        let entry_size = 4 + 4 + block.data.len();
        if accumulated + entry_size > max_payload_per_frame {
            count += 1;
            accumulated = 2 + entry_size;
        } else {
            accumulated += entry_size;
        }
    }
    if accumulated > 2 {
        count += 1;
    }
    count
}

/// FEC 校验分片集合：每组 m 个校验分片
struct ParityShards {
    /// 按 group 组织的校验分片：groups[g] = m 个校验分片
    groups: Vec<Vec<Vec<u8>>>,
}

impl ParityShards {
    /// 所有校验分片总数
    fn total_parity_count(&self) -> usize {
        self.groups.iter().map(|g| g.len()).sum()
    }
}

/// 对文件数据块进行 FEC 编码，生成校验分片
///
/// 每组 k 个数据块（最后一组不足 k 个时补零分片），生成 m 个校验分片。
fn build_parity_shards(manifest: &FileManifest, block_size: usize) -> OptransResult<ParityShards> {
    let k = constants::FEC_DATA_SHARDS;
    let encoder = FecEncoder::new()?;

    let total_blocks = manifest.blocks.len();
    let num_groups = fec_group_count(total_blocks) as usize;
    let mut groups: Vec<Vec<Vec<u8>>> = Vec::with_capacity(num_groups);

    // 复用的零分片（不足 k 个的组用零补齐，避免重复分配）
    let zero_shard = vec![0u8; block_size];

    for g in 0..num_groups {
        let start = g * k;
        let end = std::cmp::min(start + k, total_blocks);

        let mut data_shards: Vec<Vec<u8>> = Vec::with_capacity(k);
        for i in 0..k {
            if start + i < end {
                let mut shard = manifest.blocks[start + i].data.clone();
                shard.resize(block_size, 0u8);
                data_shards.push(shard);
            } else {
                data_shards.push(zero_shard.clone());
            }
        }

        let parity = encoder.encode(&data_shards)?;
        groups.push(parity);
    }

    Ok(ParityShards { groups })
}

/// 构建所有帧（数据帧 + 校验帧），交错排列以分散丢帧风险
///
/// 交错策略：
/// - 数据帧按原始块顺序连续发送（多块合并）
/// - 校验帧按组轮转插入：组0分片0, 组1分片0, ..., 组0分片1, 组1分片1, ...
///
/// 这样单组内的校验帧在时间上分散，避免 Horizon 连续丢帧
/// 导致整组校验帧全丢。FEC 容错能力确保任意 k 个分片即可恢复。
fn build_all_frames_interleaved(
    manifest: &FileManifest,
    parity: &ParityShards,
    total_blocks: u32,
    encoding_mode_bit: u8,
    max_payload_per_frame: usize,
) -> OptransResult<Vec<Frame>> {
    let m = constants::FEC_PARITY_SHARDS;

    let mut result: Vec<Frame> = Vec::new();
    let mut global_seq: u16 = 1; // meta=0，数据帧从 seq=1 开始

    // 1. 构建数据帧（多块合并）
    let mut block_iter = manifest.blocks.iter().peekable();
    while block_iter.peek().is_some() {
        let mut merged_blocks: Vec<&BlockData> = Vec::new();
        let mut accumulated = 2usize;

        while let Some(block) = block_iter.peek() {
            let entry_size = 4 + 4 + block.data.len();
            if accumulated + entry_size > max_payload_per_frame {
                break;
            }
            accumulated += entry_size;
            merged_blocks.push(block);
            block_iter.next();
        }

        if merged_blocks.is_empty() {
            break;
        }

        let frame =
            Frame::new_data_merged(&merged_blocks, global_seq, total_blocks, encoding_mode_bit)?;
        result.push(frame);
        global_seq = global_seq.wrapping_add(1);
    }

    // 2. 构建校验帧并按组轮转交错排列
    // 交错策略：所有组的第 0 个校验帧先发，再第 1 个，...
    // seq 按实际发送顺序分配，确保接收端漏帧检测（基于 seq 连续性）准确
    for p_idx in 0..m {
        for (g_idx, group_parity) in parity.groups.iter().enumerate() {
            if p_idx < group_parity.len() {
                let frame = Frame::new_parity(
                    global_seq,
                    g_idx as u16,
                    p_idx as u16,
                    total_blocks,
                    &group_parity[p_idx],
                    encoding_mode_bit,
                )?;
                result.push(frame);
                global_seq = global_seq.wrapping_add(1);
            }
        }
    }

    Ok(result)
}
