//! 接收端主循环实现
//!
//! v0.19: 流水线架构 — capture 线程 + decode 线程池 + aggregator 线程
//! capture 全速截屏，快速 decode_format_info 拿帧标识做去重预判，
//! 新帧 spawn decode 线程处理 payload，最终由 aggregator 去重排序写入。
//! v0.18: 帧位置锁定 — 首次后复用 FrameLocation，跳过全屏扫描。
//! v0.8: 高频解码错误日志增加节流。

use std::sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use log::{debug, info, warn};

use crate::cli::ReceiveArgs;
use crate::common::error::{OptransError, OptransResult};
use crate::common::types::{DecodedFrame, EncodingMode, FrameKind, FrameLocation, FrameType, IngestResult};
use crate::decoder::decode::{decode_format_info, decode_frame};
use crate::decoder::locate::{locate_frame_all_with_diag, LocateDiagnostics};
use crate::receiver::capture::capture_full_screen;
use crate::receiver::collect::FrameCollector;
use crate::receiver::reassemble::FileAssembler;

/// 执行接收操作的主入口（v0.19: 流水线架构）
pub fn run_receive(args: ReceiveArgs) -> OptransResult<()> {
    let encoding_mode = args.encoding_mode.clone();

    info!("开始接收...");
    info!("截屏模式: 全屏（Ctrl+C 退出）");
    info!("定位模式: 全屏扫描 → 帧锁定");
    info!("编码模式: {:?}", encoding_mode);
    info!("流水线: capture → decode线程池 → aggregator");
    info!("正在扫描屏幕，等待发送端信号...");

    // ── channel ──
    let (tx, rx) = mpsc::channel::<DecodedFrame>();

    // ── shared state ──
    let output_path = args.output.clone();
    let shared_output = output_path.clone();
    let done = Arc::new(AtomicBool::new(false));

    // ── aggregator 线程 ──
    let done_clone = Arc::clone(&done);
    let aggregator = thread::spawn(move || {
        let result = run_aggregator(rx, shared_output);
        done_clone.store(true, Ordering::SeqCst);
        result
    });

    // ── capture 循环 ──
    let result = run_capture_loop(tx, &encoding_mode, &done);

    // ── 等 aggregator 结束 ──
    match aggregator.join() {
        Ok(agg_result) => {
            result?;  // propagate capture errors first
            agg_result
        }
        Err(_) => Err(OptransError::FrameDecode("aggregator 线程 panic".into())),
    }
}

// ═══════════════════════════════════════════════════════════════════
//  Aggregator — 去重、排序、重组、写文件
// ═══════════════════════════════════════════════════════════════════

fn run_aggregator(
    rx: mpsc::Receiver<DecodedFrame>,
    output_path: Option<std::path::PathBuf>,
) -> OptransResult<()> {
    let mut collector = FrameCollector::new();

    loop {
        match rx.recv() {
            Ok(frame) => {
                let kind_str = match frame.frame_kind { FrameKind::I => "I", FrameKind::P => "P" };

                match collector.ingest(frame) {
                    Ok(IngestResult::NewMeta(meta)) => {
                        info!(
                            "✅ 检测到发送端！文件: {} ({} bytes, {} 块)",
                            meta.file_name, meta.file_size, meta.total_blocks
                        );
                    }
                    Ok(IngestResult::NewBlock(idx)) => {
                        let total = collector.meta().map(|m| m.total_blocks).unwrap_or(0);
                        let received = collector.blocks_received();
                        info!(
                            "数据块({}) #{}, 进度: {}/{} ({:.0}%)",
                            kind_str, idx, received, total,
                            if total > 0 { received as f64 / total as f64 * 100.0 } else { 0.0 }
                        );
                    }
                    Ok(IngestResult::EotReceived) => {
                        info!("收到 EOT 信号({})", kind_str);
                        if collector.is_complete() {
                            info!("所有数据块已收到，开始重组...");
                            return flush_and_write(collector, output_path);
                        } else {
                            let missing = collector.missing_blocks();
                            return Err(OptransError::FrameDecode(format!(
                                "收到 EOT 但缺失 {} 块: {:?}，传输失败",
                                missing.len(), missing
                            )));
                        }
                    }
                    Ok(IngestResult::Duplicate) => { /* skip */ }
                    Ok(IngestResult::Invalid) => { /* skip */ }
                    Err(e) => {
                        warn!("帧处理失败: {}", e);
                    }
                }
            }
            Err(mpsc::RecvError) => {
                warn!("sender 通道关闭，flush 缓冲区...");
                if collector.meta().is_none() {
                    return Err(OptransError::FrameDecode("未收到元数据帧，无法重组文件".into()));
                }
                return flush_and_write(collector, output_path);
            }
        }
    }
}

fn flush_and_write(
    collector: FrameCollector,
    output_path: Option<std::path::PathBuf>,
) -> OptransResult<()> {
    let meta = collector.meta().cloned().ok_or_else(|| {
        OptransError::FrameDecode("未收到元数据帧，无法重组文件".into())
    })?;
    let expected_file_size = meta.file_size;
    let blocks = collector.take_blocks();

    let total_blocks = meta.total_blocks as usize;
    if blocks.len() < total_blocks {
        let missing: Vec<u32> = (0..total_blocks as u32)
            .filter(|i| !blocks.contains_key(i))
            .collect();
        warn!("警告: 缺少 {} 个数据块，文件可能不完整", missing.len());
    }

    let path = match output_path {
        Some(p) => p,
        None => {
            info!("未指定输出文件名，使用元数据中的: {}", meta.file_name);
            std::path::PathBuf::from(&meta.file_name)
        }
    };

    let mut assembler = FileAssembler::new(meta);
    let mut indices: Vec<u32> = blocks.keys().cloned().collect();
    indices.sort();
    for idx in &indices {
        if let Some(data) = blocks.get(idx) {
            assembler.add_block(*idx, data.clone());
        }
    }
    assembler.finalize_to_file(&path)?;
    info!("文件已保存: {:?} ({} bytes)", path, expected_file_size);
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════
//  Capture 循环 — 截屏 + 快速预判 + spawn decode
// ═══════════════════════════════════════════════════════════════════

fn run_capture_loop(
    tx: mpsc::Sender<DecodedFrame>,
    encoding_mode: &EncodingMode,
    done: &AtomicBool,
) -> OptransResult<()> {
    let mut no_frame_start = Instant::now();
    let mut last_heartbeat = Instant::now();
    let locked = Arc::new(std::sync::OnceLock::<FrameLocation>::new());
    let mut locate_diag_printed = false;
    let mut loop_count: u64 = 0;

    loop {
        loop_count += 1;
        let loop_start = Instant::now();

        // 1. 截屏
        let t0 = Instant::now();
        let (pixels, img_w, img_h) = match capture_full_screen() {
            Ok(r) => r,
            Err(e) => {
                warn!("截屏失败: {}，重试...", e);
                continue;
            }
        };
        let capture_ms = t0.elapsed().as_millis();

        // 2. 定位
        let t1 = Instant::now();
        let candidates: Vec<(FrameLocation, LocateDiagnostics)>;
        let mut locate_cost = 0u128;

        let locked_loc = locked.get().cloned();

        if let Some(ref loc) = locked_loc {
            candidates = vec![(loc.clone(), LocateDiagnostics { candidates_found: 0, candidates_verified: 0 })];
        } else {
            candidates = locate_frame_all_with_diag(&pixels, img_w, img_h);
            locate_cost = t1.elapsed().as_millis();

            if candidates.is_empty() {
                let waited = no_frame_start.elapsed();
                if waited.as_secs() >= 3 && last_heartbeat.elapsed() >= Duration::from_secs(3) {
                    debug!("等待发送端... (已等待 {}s)", waited.as_secs());
                    last_heartbeat = Instant::now();
                }
                continue;
            }

            if !locate_diag_printed {
                locate_diag_printed = true;
                let diag = &candidates[0].1;
                debug!("定位诊断: 候选{}个/验证{}个, 有效三角组{}个（显示前10）",
                    diag.candidates_found, diag.candidates_verified, candidates.len());
                let show_n = candidates.len().min(10);
                for (i, (loc, _)) in candidates.iter().enumerate().take(show_n) {
                    let is_frame_like = loc.modules_w >= 550 && loc.modules_w <= 710
                        && loc.modules_h >= 300 && loc.modules_h <= 460;
                    debug!("[#{}]{} TL=({:.1},{:.1}) TR=({:.1},{:.1}) BL=({:.1},{:.1}) frame={}w×{}h",
                        i, if is_frame_like { " ★" } else { "" },
                        loc.tl.center_x, loc.tl.center_y,
                        loc.tr.center_x, loc.tr.center_y,
                        loc.bl.center_x, loc.bl.center_y,
                        loc.modules_w, loc.modules_h);
                }
                let rank = candidates.iter().position(|(loc, _)| {
                    loc.modules_w >= 550 && loc.modules_w <= 710
                        && loc.modules_h >= 300 && loc.modules_h <= 460
                });
                if let Some(r) = rank {
                    debug!("  >>> 帧型首位排名 #{}, 共{}候选", r, candidates.len());
                } else {
                    debug!("  >>> 未找到帧型！共{}候选", candidates.len());
                }
            }
        }

        no_frame_start = Instant::now();
        let locate_ms = if locked_loc.is_some() { 0u128 } else { locate_cost };

        // 3. 快速预判 — decode_format_info 拿 {frame_type, frame_sequence}
        let t2 = Instant::now();
        let mut first_error: Option<(OptransError, FrameLocation)> = None;
        let mut best_candidate: Option<usize> = None; // candidate index

        for (ci, (loc, _)) in candidates.iter().enumerate() {
            match decode_format_info(&pixels, img_w, img_h, loc) {
                Ok(info) => {
                    // EOT 检测
                    if info.frame_type == FrameType::EOT {
                        info!("收到 EOT 信号");
                        let eot_frame = DecodedFrame {
                            frame_type: FrameType::EOT,
                            frame_sequence: info.frame_sequence,
                            block_index: 0, total_blocks: 0,
                            payload: Vec::new(),
                            frame_kind: FrameKind::I,
                            encoding_mode: EncodingMode::Binary,
                            base_frame_seq: None,
                            data_module_values: Vec::new(),
                        };
                        let _ = tx.send(eot_frame);
                        drop(tx);
                        return Ok(());
                    }

                    best_candidate = Some(ci);
                    break;
                }
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some((e, loc.clone()));
                    }
                }
            }
        }

        let precheck_ms = t2.elapsed().as_millis();

        // 4. 如果有有效候选，spawn decode 线程
        if let Some(ci) = best_candidate {
            let loc = candidates[ci].0.clone();
            // v0.19.4: Arc 共享截屏数据，避免每线程 8MB clone
            let pixels_arc = Arc::new(pixels);
            let enc_mode = encoding_mode.clone();
            let tx_clone = tx.clone();
            let locked_clone = Arc::clone(&locked);
            let is_first = locked_loc.is_none();

            thread::spawn(move || {
                match decode_frame(&pixels_arc, img_w, img_h, &loc, &enc_mode) {
                    Ok(frame) => {
                        let is_meta_or_data = matches!(frame.frame_type,
                            FrameType::Meta | FrameType::Data);
                        if is_meta_or_data && frame.payload.is_empty() {
                            return;
                        }
                        // v0.19: 仅 decode 成功后锁定，避免 CRC 碰撞假阳性
                        if is_first {
                            let is_new = locked_clone.set(loc.clone()).is_ok();
                            if is_new {
                                info!("✓ 首个有效帧解码成功，已与发送端建立连接");
                                info!("🔒 帧位置已锁定（跳过全屏扫描）");
                            }
                        }
                        let _ = tx_clone.send(frame);
                    }
                    Err(e) => {
                        // decode failed → capture will retry next round
                        debug!("payload 解码失败: {}", e);
                    }
                }
            });
        } else {
            // format_info 全部失败 — 输出第一个错误
            if let Some((err, loc)) = first_error {
                match &err {
                    OptransError::InvalidFormatInfo(msg) => {
                        if !msg.starts_with("假帧") {
                            debug!("格式信息解码失败: {} (tl={:.0},{:.0})",
                                msg, loc.tl.center_x, loc.tl.center_y);
                        }
                    }
                    _ => {
                        warn!("帧解码失败: {}", err);
                    }
                }
            }

            // aggregator 已完成（文件已写入）且发送端已消失 → 退出
            if locked_loc.is_some() && done.load(Ordering::SeqCst) {
                info!("发送端已关闭，接收完成");
                drop(tx);
                return Ok(());
            }
        }

        // 时序日志
        let total_ms = loop_start.elapsed().as_millis();
        if loop_count.is_multiple_of(10) {
            debug!("时序: 截屏{}ms 定位{}ms 预判{}ms 总{}ms @{}fps",
                capture_ms, locate_ms, precheck_ms, total_ms,
                1000 / total_ms.max(1));
        }
    }
}
