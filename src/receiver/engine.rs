//! 接收端主循环实现
//!
//! v0.19: 流水线架构 — capture 线程 + decode 线程池 + aggregator 线程
//! capture 全速截屏，快速 decode_format_info 拿帧标识做去重预判，
//! 新帧 spawn decode 线程处理 payload，最终由 aggregator 去重排序写入。
//! v0.18: 帧位置锁定 — 首次后复用 FrameLocation，跳过全屏扫描。
//! v0.8: 高频解码错误日志增加节流。

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use log::{debug, info, warn};

use crate::common::error::{OptransError, OptransResult};
use crate::common::types::EncodingMode;
use crate::common::types::{DecodedFrame, FrameKind, FrameLocation, FrameType, IngestResult};
use crate::decoder::decode::{decode_format_info, decode_frame};
use crate::decoder::locate::{LocateDiagnostics, locate_frame_all_with_diag};
use crate::receiver::capture::capture_full_screen;
use crate::receiver::collect::FrameCollector;
use crate::receiver::reassemble::FileAssembler;

/// 执行接收操作的主入口（v0.19: 流水线架构）
pub fn run_receive(
    output: Option<std::path::PathBuf>,
    encoding_mode: EncodingMode,
) -> OptransResult<()> {
    info!("开始接收...");
    info!("截屏模式: 全屏（Ctrl+C 退出）");
    info!("定位模式: 全屏扫描 → 帧锁定");
    info!("编码模式: {:?}", encoding_mode);
    info!("流水线: capture → decode线程池 → aggregator");
    info!("正在扫描屏幕，等待发送端信号...");

    // ── channel ──
    let (tx, rx) = mpsc::channel::<DecodedFrame>();

    // ── shared state ──
    let shared_output = output.clone();
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
            result?; // propagate capture errors first
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
                let kind_str = match frame.frame_kind {
                    FrameKind::I => "I",
                    FrameKind::P => "P",
                };

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
                            kind_str,
                            idx,
                            received,
                            total,
                            if total > 0 {
                                received as f64 / total as f64 * 100.0
                            } else {
                                0.0
                            }
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
                                missing.len(),
                                missing
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
                    return Err(OptransError::FrameDecode(
                        "未收到元数据帧，无法重组文件".into(),
                    ));
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
    let meta = collector
        .meta()
        .cloned()
        .ok_or_else(|| OptransError::FrameDecode("未收到元数据帧，无法重组文件".into()))?;
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
//  Capture 循环 — 截屏 + 快速预判 + 线程池 decode
// ═══════════════════════════════════════════════════════════════════

fn run_capture_loop(
    tx: mpsc::Sender<DecodedFrame>,
    encoding_mode: &EncodingMode,
    done: &AtomicBool,
) -> OptransResult<()> {
    let mut no_frame_start = Instant::now();
    let mut last_heartbeat = Instant::now();
    let locked: Arc<std::sync::OnceLock<FrameLocation>> = Arc::new(std::sync::OnceLock::new());
    let mut locate_diag_printed = false;
    let mut loop_count: u64 = 0;
    let mut last_seq: u16 = u16::MAX;  // 检测帧切换（防首帧撕裂）
    let pool = threadpool::ThreadPool::new(2);

    loop {
        loop_count += 1;
        if done.load(Ordering::SeqCst) {
            drop(tx);
            return Ok(());
        }

        // 1. 截屏
        let (pixels, img_w, img_h) = match capture_full_screen() {
            Ok(r) => r,
            Err(e) => { warn!("截屏失败: {}，重试...", e); continue; }
        };

        // 2. 定位：锁定时直接用，否则全屏扫描
        let candidates = if let Some(loc) = locked.get() {
            vec![(loc.clone(), LocateDiagnostics { candidates_found: 0, candidates_verified: 0 })]
        } else {
            let raw = locate_frame_all_with_diag(&pixels, img_w, img_h);
            if raw.is_empty() {
                if no_frame_start.elapsed().as_secs() >= 3
                    && last_heartbeat.elapsed() >= Duration::from_secs(3)
                {
                    debug!("等待发送端...");
                    last_heartbeat = Instant::now();
                }
                continue;
            }
            // 位置过滤
            let margin = crate::common::constants::FRAME_MARGIN_PX_DEFAULT as f64;
            let mp = crate::common::constants::MODULE_PIXELS_DEFAULT as f64;
            let ex = margin + 3.5 * mp;
            let ey = margin + 3.5 * mp;
            let max_dist = 120.0;
            let before = raw.len();
            let filtered: Vec<_> = raw.into_iter()
                .filter(|(loc, _)| {
                    let dx = loc.tl.center_x - ex;
                    let dy = loc.tl.center_y - ey;
                    (dx*dx + dy*dy).sqrt() <= max_dist
                })
                .collect();
            debug!("[find] {}→{} candidates", before, filtered.len());

            if !locate_diag_printed && !filtered.is_empty() {
                locate_diag_printed = true;
                let d = &filtered[0].1;
                info!("定位: {}候选/{}验证 → {}三角组, TL=({:.0},{:.0}) frame={}×{}",
                    d.candidates_found, d.candidates_verified, filtered.len(),
                    filtered[0].0.tl.center_x, filtered[0].0.tl.center_y,
                    filtered[0].0.modules_w, filtered[0].0.modules_h);
            }
            filtered
        };

        no_frame_start = Instant::now();

        // 3. 预判：format_info 快速 CRC 校验 + 帧切换检测
        //    format info 在帧底部（最后渲染），CRC 通过 = 全帧完整 → 可安全解码
        let mut best_candidate: Option<(usize, crate::common::types::FormatInfo)> = None;
        for (ci, (loc, _)) in candidates.iter().enumerate() {
            match decode_format_info(&pixels, img_w, img_h, loc) {
                Ok(info) => {
                    if info.frame_type == FrameType::EOT {
                        info!("收到 EOT 信号");
                        let _ = tx.send(DecodedFrame {
                            frame_type: FrameType::EOT,
                            frame_sequence: info.frame_sequence,
                            block_index: 0, total_blocks: 0,
                            payload: Vec::new(),
                            frame_kind: FrameKind::I,
                            encoding_mode: EncodingMode::Binary,
                            base_frame_seq: None,
                            data_module_values: Vec::new(),
                        });
                        drop(tx);
                        return Ok(());
                    }
                    best_candidate = Some((ci, info));
                    break;
                }
                Err(_) => {}
            }
        }

        // format info 未通过 → 帧不完整（底部未渲染完成），跳过继续截下一屏
        let Some((ci, info)) = best_candidate else { continue; };

        // 帧切换检测：新帧首次截屏时底部可能未渲染完，跳过等下一屏
        if info.frame_sequence != last_seq && last_seq != u16::MAX {
            last_seq = info.frame_sequence;
            continue;
        }
        last_seq = info.frame_sequence;

        // 4. 解码（线程池：帧完整，安全提交）
        {
            let loc = candidates[ci].0.clone();
            let pixels_arc = Arc::new(pixels);
            let enc_mode = encoding_mode.clone();
            let tx_clone = tx.clone();
            let locked_clone = Arc::clone(&locked);
            let is_first = locked.get().is_none();
            pool.execute(move || {
                match decode_frame(&pixels_arc, img_w, img_h, &loc, &enc_mode) {
                    Ok(frame) => {
                        if matches!(frame.frame_type, FrameType::Meta | FrameType::Data)
                            && frame.payload.is_empty() { return; }
                        if is_first && matches!(frame.frame_type, FrameType::Data) {
                            let _ = locked_clone.set(loc);
                            info!("✓ 首个有效帧解码成功，已锁定位置");
                        }
                        let _ = tx_clone.send(frame);
                    }
                    Err(e) => { debug!("payload 解码失败: {}", e); }
                }
            });
        }

        // 时序
        if loop_count.is_multiple_of(10) {
            debug!("capture loop @{}fps", 0);
        }
    }
}
