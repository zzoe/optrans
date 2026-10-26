//! 发送编排模块
//!
//! 发送主控逻辑：读取文件 → 块级 FEC 编码 → 帧构建 → 灰度渲染 → 循环显示 → EOT。
//!
//! v0.26: 块级 FEC（前向纠错）。数据块分组后生成 RS 校验分片，
//! 数据帧与校验帧交错发送，接收端收到任意 k 个分片即可恢复该组。
//!
//! 发送端需要全屏置顶窗口，仅 Windows 支持；其他平台 `run_send` 返回
//! `UnsupportedPlatform`。

// 发送路径整体为 Windows-only（非 Windows 平台仅提供 run_send 存根），
// 这些函数/导入在非 Windows 平台不被引用。
#![cfg_attr(not(target_os = "windows"), allow(dead_code, unused_imports, unused_variables))]

use std::path::{Path, PathBuf};
use std::thread;

use crate::common::constants;
use crate::common::error::{OptransError, OptransResult};
use crate::common::fec::{FecEncoder, fec_group_count};
use crate::common::types::{BlockData, GrayEncodingConfig, MetaPayload};
use crate::encoder::frame::Frame;
use crate::encoder::render::render_frame_default;
#[cfg(target_os = "windows")]
use crate::platform::win32::helpers::DisplayGuard;
#[cfg(target_os = "windows")]
use crate::platform::win32::window::SenderWindow;

use log::{debug, info, warn};

/// 首帧 / 末帧驻留时间（ms）
const BOUNDARY_DWELL_MS: u64 = 100;

/// 有效的灰度级数（2 的幂，bit 直映射）
const VALID_GRAY_LEVELS: [u32; 4] = [2, 4, 8, 16];

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
    let (screen_w, screen_h) = crate::platform::screen_size();

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

/// 执行发送操作的主入口（仅 Windows：发送端需要全屏置顶窗口）
#[cfg(target_os = "windows")]
pub fn run_send(path: PathBuf) -> OptransResult<()> {
    let module_pixels = constants::MODULE_PIXELS_DEFAULT;
    let interval_ms = constants::FRAME_INTERVAL_MS;
    let gray_levels = constants::GRAY_LEVELS_DEFAULT;
    let frame_margin = constants::FRAME_MARGIN_PX_DEFAULT;
    let enable_heartbeat = true;

    // 验证灰度级数（M8：非法值直接报错，不再警告后继续——非法 levels 会
    // 在灰度表构建时下溢/除零）
    crate::common::types::validate_gray_levels(gray_levels)?;

    // 灰度编码配置
    let gray_config = GrayEncodingConfig::new(gray_levels)?;
    // encoding_mode_bit: 2级灰度=二值模式=bit0, 其他=灰度模式=bit1
    let encoding_mode_bit: u8 = if gray_levels == 2 { 0 } else { 1 };

    debug!(
        "灰度配置: {} 级, 编码模式位={}",
        gray_levels, encoding_mode_bit
    );

    // 阻止系统在传输期间睡眠/熄屏/锁屏（RAII，函数返回时自动恢复）
    let _display_guard = DisplayGuard::new();

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

    // 2. 流式扫描文件（M10 第一遍读：SHA-256 + 块长度表，不驻留文件内容）
    let scan = crate::sender::chunker::scan_file(Path::new(&path), block_size)?;

    let total_blocks = scan.total_blocks();
    let fec_k = constants::FEC_DATA_SHARDS as u16;
    let fec_m = constants::FEC_PARITY_SHARDS as u16;
    let fec_groups = fec_group_count(scan.block_sizes.len());
    // FEC 分片长度 = block_size（最后一块数据补零到此长度）
    let fec_shard_size = block_size as u32;

    info!(
        "文件: {} ({} bytes, {} 块, 每块≤{}B)",
        scan.file_name, scan.file_size, total_blocks, block_size
    );
    info!("SHA-256: {}", hex_str(&scan.sha256));
    info!(
        "FEC: {} 组, k={} m={} (容错 {:.0}%), 分片长度={}B",
        fec_groups,
        fec_k,
        fec_m,
        fec_m as f64 / (fec_k + fec_m) as f64 * 100.0,
        fec_shard_size
    );

    // 3. 块级 FEC 编码（M10：逐组第二遍读取块数据，仅校验分片常驻）
    let parity_shards = build_parity_shards_streaming(Path::new(&path), &scan, block_size)?;

    // 4. 构建 MetaPayload（含 FEC 参数）
    let meta = MetaPayload {
        file_name: scan.file_name.clone(),
        file_size: scan.file_size,
        sha256: scan.sha256,
        total_blocks,
        fec_k,
        fec_m,
        fec_groups,
        fec_shard_size,
    };

    // 5. 预构建元数据帧和 EOT 帧
    let meta_frame = Frame::new_metadata(&meta, 0)?;
    // 帧计划（M10：轻量索引，帧载荷在渲染线程中惰性生成）
    let data_plan = plan_data_frames(&scan.block_sizes, max_payload_per_frame);
    let total_data_frames = data_plan.len();
    let parity_plan = plan_parity_frames(
        &parity_shards,
        1u16.wrapping_add(total_data_frames as u16),
    );
    // EOT seq = 1(meta) + 数据帧数 + 校验帧数（wrapping 与帧计划分配一致）
    let total_parity_frames = parity_plan.len();
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
        let meta_gray_config = GrayEncodingConfig::new(2)?;
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

    // ── 阶段 1: 帧计划（数据帧 + 校验帧交错排列，M10 流式：不驻留帧/块数据） ──
    let total_frames = data_plan.len() + parity_plan.len();
    info!("▶ 帧计划就绪（{} 帧，交错），启动渲染线程...", total_frames);

    // ── 阶段 2: 流式渲染线程 (异步) + display loop (严格串行) ──
    // M10: 渲染线程按帧计划逐个第二遍读取块数据、惰性构帧并渲染，
    // 内存中常驻的只有 FEC 校验分片（1 个渲染在途帧 + channel 容量 2 的像素缓冲）。
    // 有界 channel（容量 2）：防止 display loop 变慢时渲染线程堆积像素缓冲区导致 OOM。
    let (render_tx, render_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(2);

    let render_gray = gray_config.clone();
    let render_path = path.clone();
    let render_parity = parity_shards;
    thread::spawn(move || {
        let emit = |frame: &Frame| -> bool {
            let pixels = render_frame_default(
                frame,
                module_pixels,
                modules_w,
                modules_h,
                &render_gray,
                enable_heartbeat,
            );
            render_tx.send(pixels).is_ok()
        };

        // 数据帧：逐帧第二遍读取所需块（每帧至多 max_payload_per_frame 字节）
        for entry in &data_plan {
            let blocks = match crate::sender::chunker::read_blocks_range(
                &render_path,
                block_size,
                entry.start_block,
                entry.block_count,
            ) {
                Ok(b) => b,
                Err(e) => {
                    warn!("读取数据块失败（起始块 {}）: {}", entry.start_block, e);
                    return;
                }
            };
            let refs: Vec<&BlockData> = blocks.iter().collect();
            let seq = entry.seq;
            let frame = match Frame::new_data_merged(
                &refs,
                seq,
                total_blocks,
                encoding_mode_bit,
            ) {
                Ok(f) => f,
                Err(e) => {
                    warn!("构建数据帧失败（seq={}）: {}", seq, e);
                    return;
                }
            };
            debug!("  [DATA] seq={} blocks={}-{}", seq, entry.start_block, entry.start_block + entry.block_count - 1);
            if !emit(&frame) {
                return;
            }
        }

        // 校验帧：按交错序逐个取常驻校验分片
        for entry in &parity_plan {
            let frame = match Frame::new_parity(
                entry.seq,
                entry.group_idx,
                entry.parity_idx,
                total_blocks,
                &render_parity.groups[entry.group_idx as usize][entry.parity_idx as usize],
                encoding_mode_bit,
            ) {
                Ok(f) => f,
                Err(e) => {
                    warn!("构建校验帧失败（seq={}）: {}", entry.seq, e);
                    return;
                }
            };
            debug!("  [PARITY] seq={} g={} p={}", entry.seq, entry.group_idx, entry.parity_idx);
            if !emit(&frame) {
                return;
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

/// 发送端存根：非 Windows 平台不支持发送（需要全屏置顶窗口）
#[cfg(not(target_os = "windows"))]
pub fn run_send(_path: PathBuf) -> OptransResult<()> {
    Err(OptransError::UnsupportedPlatform(
        "发送端需要全屏置顶窗口，仅支持 Windows；请在 Windows 主机上执行 send",
    ))
}

/// 数据帧计划条目（M10：轻量索引，帧载荷惰性生成）
struct DataFramePlan {
    /// 帧序号
    seq: u16,
    /// 起始块序号
    start_block: u32,
    /// 合并的块数
    block_count: u32,
}

/// 校验帧计划条目
struct ParityFramePlan {
    /// 帧序号
    seq: u16,
    /// FEC 组号
    group_idx: u16,
    /// 组内校验分片序号
    parity_idx: u16,
}

/// 制定数据帧计划（多块合并），与旧 build_all_frames_interleaved 的合并逻辑逐字节一致
///
/// 块长度取自第一遍扫描的 `block_sizes`，无需驻留块数据。
fn plan_data_frames(block_sizes: &[usize], max_payload_per_frame: usize) -> Vec<DataFramePlan> {
    let mut plan = Vec::new();
    let mut global_seq: u16 = 1; // meta=0，数据帧从 seq=1 开始
    let mut idx = 0usize;

    while idx < block_sizes.len() {
        let start = idx;
        let mut accumulated = 2usize; // 2B count header

        while idx < block_sizes.len() {
            let entry_size = 4 + 4 + block_sizes[idx];
            if accumulated + entry_size > max_payload_per_frame {
                break;
            }
            accumulated += entry_size;
            idx += 1;
        }

        if idx == start {
            break;
        }

        plan.push(DataFramePlan {
            seq: global_seq,
            start_block: start as u32,
            block_count: (idx - start) as u32,
        });
        global_seq = global_seq.wrapping_add(1);
    }

    plan
}

/// 制定校验帧计划：所有组的第 0 个校验分片先发，再第 1 个，...
///
/// 交错使单组内的校验帧在时间上分散，避免 Horizon 连续丢帧导致整组全丢。
/// seq 按实际发送顺序分配，确保接收端漏帧检测（基于 seq 连续性）准确。
fn plan_parity_frames(parity: &ParityShards, first_seq: u16) -> Vec<ParityFramePlan> {
    let m = constants::FEC_PARITY_SHARDS;
    let mut plan = Vec::new();
    let mut global_seq = first_seq;

    for p_idx in 0..m {
        for (g_idx, group_parity) in parity.groups.iter().enumerate() {
            if p_idx < group_parity.len() {
                plan.push(ParityFramePlan {
                    seq: global_seq,
                    group_idx: g_idx as u16,
                    parity_idx: p_idx as u16,
                });
                global_seq = global_seq.wrapping_add(1);
            }
        }
    }

    plan
}

/// FEC 校验分片集合：每组 m 个校验分片
struct ParityShards {
    /// 按 group 组织的校验分片：groups[g] = m 个校验分片
    groups: Vec<Vec<Vec<u8>>>,
}

/// 流式 FEC 编码（M10）：逐组第二遍读取块数据，仅校验分片常驻内存
///
/// 每组 k 个数据块（最后一组不足 k 个时补零分片），生成 m 个校验分片。
fn build_parity_shards_streaming(
    path: &Path,
    scan: &crate::sender::chunker::FileScan,
    block_size: usize,
) -> OptransResult<ParityShards> {
    let k = constants::FEC_DATA_SHARDS;
    let encoder = FecEncoder::new()?;

    let total_blocks = scan.block_sizes.len();
    let num_groups = fec_group_count(total_blocks) as usize;
    let mut groups: Vec<Vec<Vec<u8>>> = Vec::with_capacity(num_groups);

    // 复用的零分片（不足 k 个的组用零补齐，避免重复分配）
    let zero_shard = vec![0u8; block_size];

    for g in 0..num_groups {
        let start = g * k;
        let end = std::cmp::min(start + k, total_blocks);

        let blocks = crate::sender::chunker::read_blocks_range(
            path,
            block_size,
            start as u32,
            (end - start) as u32,
        )?;

        let mut data_shards: Vec<Vec<u8>> = Vec::with_capacity(k);
        for i in 0..k {
            if start + i < end {
                let mut shard = blocks[i].data.clone();
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

/// 测试护航（M10）：帧计划 + 流式构帧与旧"全量构帧"逐字节一致
#[cfg(test)]
fn build_frames_from_plan(
    path: &Path,
    block_size: usize,
    data_plan: &[DataFramePlan],
    parity_plan: &[ParityFramePlan],
    parity: &ParityShards,
    total_blocks: u32,
    encoding_mode_bit: u8,
) -> OptransResult<Vec<Frame>> {
    let mut result = Vec::with_capacity(data_plan.len() + parity_plan.len());

    for entry in data_plan {
        let blocks = crate::sender::chunker::read_blocks_range(
            path,
            block_size,
            entry.start_block,
            entry.block_count,
        )?;
        let refs: Vec<&BlockData> = blocks.iter().collect();
        result.push(Frame::new_data_merged(
            &refs,
            entry.seq,
            total_blocks,
            encoding_mode_bit,
        )?);
    }
    for entry in parity_plan {
        result.push(Frame::new_parity(
            entry.seq,
            entry.group_idx,
            entry.parity_idx,
            total_blocks,
            &parity.groups[entry.group_idx as usize][entry.parity_idx as usize],
            encoding_mode_bit,
        )?);
    }

    Ok(result)
}

/// M10 测试护航：流式扫描 / 帧计划 / 流式 FEC 与旧全量路径逐字节一致
#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::types::FileManifest;
    use std::io::Write;

    /// 写测试文件并返回路径
    fn write_test_file(name: &str, data: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(data).unwrap();
        p
    }

    /// 旧全量 FEC 编码（对照实现，与被删除的 build_parity_shards 逻辑一致）
    fn build_parity_shards_legacy(
        manifest: &FileManifest,
        block_size: usize,
    ) -> Vec<Vec<Vec<u8>>> {
        let k = constants::FEC_DATA_SHARDS;
        let encoder = FecEncoder::new().unwrap();
        let total_blocks = manifest.blocks.len();
        let num_groups = fec_group_count(total_blocks) as usize;
        let mut groups = Vec::with_capacity(num_groups);
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
            groups.push(encoder.encode(&data_shards).unwrap());
        }
        groups
    }

    #[test]
    fn scan_matches_read_file() {
        let data: Vec<u8> = (0..10000u32).map(|i| (i * 7) as u8).collect();
        let path = write_test_file("optrans_scan_match.dat", &data);
        let block_size = 3000;

        let manifest =
            crate::sender::chunker::read_file_with_block_size(&path, block_size).unwrap();
        let scan = crate::sender::chunker::scan_file(&path, block_size).unwrap();

        assert_eq!(scan.file_name, manifest.file_name);
        assert_eq!(scan.file_size, manifest.file_size);
        assert_eq!(scan.sha256, manifest.sha256);
        let sizes: Vec<usize> = manifest.blocks.iter().map(|b| b.data.len()).collect();
        assert_eq!(scan.block_sizes, sizes);

        // 流式读块与全量分块逐字节一致
        let streamed =
            crate::sender::chunker::read_blocks_range(&path, block_size, 0, scan.total_blocks())
                .unwrap();
        assert_eq!(streamed.len(), manifest.blocks.len());
        for (a, b) in streamed.iter().zip(manifest.blocks.iter()) {
            assert_eq!(a.index, b.index);
            assert_eq!(a.data, b.data);
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn streaming_parity_matches_legacy() {
        let data: Vec<u8> = (0..50000u32).map(|i| (i * 13) as u8).collect();
        let path = write_test_file("optrans_parity_match.dat", &data);
        let block_size = 4096;

        let manifest =
            crate::sender::chunker::read_file_with_block_size(&path, block_size).unwrap();
        let scan = crate::sender::chunker::scan_file(&path, block_size).unwrap();

        let legacy = build_parity_shards_legacy(&manifest, block_size);
        let streaming = build_parity_shards_streaming(&path, &scan, block_size).unwrap();

        assert_eq!(streaming.groups.len(), legacy.len());
        for (a, b) in streaming.groups.iter().zip(legacy.iter()) {
            assert_eq!(a, b);
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn frame_plan_matches_legacy_sequence() {
        let data: Vec<u8> = (0..30000u32).map(|i| (i * 31) as u8).collect();
        let path = write_test_file("optrans_frame_match.dat", &data);
        let block_size = 5000;
        let max_payload = 12000;
        let mode_bit = 1u8;

        let manifest =
            crate::sender::chunker::read_file_with_block_size(&path, block_size).unwrap();
        let scan = crate::sender::chunker::scan_file(&path, block_size).unwrap();
        let total_blocks = scan.total_blocks();

        // 新路径：帧计划 + 流式构帧
        let parity = build_parity_shards_streaming(&path, &scan, block_size).unwrap();
        let data_plan = plan_data_frames(&scan.block_sizes, max_payload);
        let parity_plan =
            plan_parity_frames(&parity, 1u16.wrapping_add(data_plan.len() as u16));
        let new_frames = build_frames_from_plan(
            &path,
            block_size,
            &data_plan,
            &parity_plan,
            &parity,
            total_blocks,
            mode_bit,
        )
        .unwrap();

        // 旧路径：全量构帧（数据帧按块顺序合并 + 校验帧组轮转交错）
        let legacy_parity = build_parity_shards_legacy(&manifest, block_size);
        let mut legacy_frames = Vec::new();
        let mut global_seq: u16 = 1;
        let mut block_iter = manifest.blocks.iter().peekable();
        while block_iter.peek().is_some() {
            let mut merged: Vec<&BlockData> = Vec::new();
            let mut accumulated = 2usize;
            while let Some(block) = block_iter.peek() {
                let entry_size = 4 + 4 + block.data.len();
                if accumulated + entry_size > max_payload {
                    break;
                }
                accumulated += entry_size;
                merged.push(block);
                block_iter.next();
            }
            if merged.is_empty() {
                break;
            }
            legacy_frames.push(
                Frame::new_data_merged(&merged, global_seq, total_blocks, mode_bit).unwrap(),
            );
            global_seq = global_seq.wrapping_add(1);
        }
        let m = constants::FEC_PARITY_SHARDS;
        for p_idx in 0..m {
            for (g_idx, group_parity) in legacy_parity.iter().enumerate() {
                if p_idx < group_parity.len() {
                    legacy_frames.push(
                        Frame::new_parity(
                            global_seq,
                            g_idx as u16,
                            p_idx as u16,
                            total_blocks,
                            &group_parity[p_idx],
                            mode_bit,
                        )
                        .unwrap(),
                    );
                    global_seq = global_seq.wrapping_add(1);
                }
            }
        }

        assert_eq!(new_frames.len(), legacy_frames.len());
        for (a, b) in new_frames.iter().zip(legacy_frames.iter()) {
            assert_eq!(a.format_info, b.format_info, "格式信息不一致");
            assert_eq!(a.encoded_payload, b.encoded_payload, "编码载荷不一致");
        }

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn empty_file_plans_empty_frames() {
        let path = write_test_file("optrans_empty_stream.dat", &[]);
        let block_size = 1024;
        let scan = crate::sender::chunker::scan_file(&path, block_size).unwrap();
        assert_eq!(scan.file_size, 0);
        assert!(scan.block_sizes.is_empty());
        assert_eq!(scan.total_blocks(), 0);

        let parity = build_parity_shards_streaming(&path, &scan, block_size).unwrap();
        assert!(parity.groups.is_empty());
        assert!(plan_data_frames(&scan.block_sizes, 100).is_empty());
        assert!(plan_parity_frames(&parity, 1).is_empty());

        let _ = std::fs::remove_file(&path);
    }
}
