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

use log::{info, warn};

use crate::common::error::{OptransError, OptransResult};
use crate::common::types::EncodingMode;
use crate::common::types::{DecodedFrame, FrameLocation, FrameType, IngestResult};
use crate::decoder::decode::{decode_format_info, decode_frame};
use crate::decoder::locate::locate_frame_all_with_diag;
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
                            "数据块 #{}, 进度: {}/{} ({:.0}%)",
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
                        info!("收到 EOT 信号");
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
    let pool = threadpool::ThreadPool::new(2);
    let enc_mode = encoding_mode.clone();

    // ═══ Phase 1: 等待 Meta 帧 → 锁定位置 ═══
    let mut meta_loc: Option<FrameLocation> = None;
    loop {
        if done.load(Ordering::SeqCst) { drop(tx); return Ok(()); }

        let (pixels, img_w, img_h) = match capture_full_screen() {
            Ok(r) => r,
            Err(e) => { warn!("截屏失败: {}，重试...", e); continue; }
        };

        let candidates = locate_frame_all_with_diag(&pixels, img_w, img_h);
        if candidates.is_empty() { continue; }

        let pixels_arc = Arc::new(pixels);
        let mut found = false;
        for (loc, _) in &candidates {
            if let Ok(info) = decode_format_info(&pixels_arc, img_w, img_h, loc) {
                if info.frame_type != FrameType::Meta { continue; }
                if let Ok(frame) = decode_frame(&pixels_arc, img_w, img_h, loc, &enc_mode) {
                    if !frame.payload.is_empty() {
                        let _ = tx.send(frame);
                        meta_loc = Some(loc.clone());
                        found = true;
                        break;
                    }
                }
            }
        }
        if found {
            info!("✓ Meta 帧解析成功，位置已锁定");
            break;
        }
    }
    let loc = meta_loc.unwrap();

    // ═══ Phase 2: 捕获数据帧 + EOT（纯转发，aggregator 负责计数/漏帧检测） ═══
    // last_data_seq: 上一次截屏看到的 Data 帧序号（用于检测漏帧）
    //   - meta(seq=0) 之后第一个数据帧 seq=1
    //   - 同一帧重复截到（seq 相同）→ 正常，跳过
    //   - seq > last + 1 → 中间有帧未截到 → 漏帧告警
    let mut last_data_seq: Option<u16> = None;
    loop {
        if done.load(Ordering::SeqCst) { drop(tx); return Ok(()); }

        let (pixels, img_w, img_h) = match capture_full_screen() {
            Ok(r) => r,
            Err(e) => { warn!("截屏失败: {}，重试...", e); continue; }
        };

        let info = match decode_format_info(&pixels, img_w, img_h, &loc) {
            Ok(i) => i,
            Err(_) => continue,
        };

        // 帧序号连续性检测（在 capture loop 中，不受线程池并发影响）
        if info.frame_type == FrameType::Data || info.frame_type == FrameType::EOT {
            let seq = info.frame_sequence;
            if let Some(last) = last_data_seq {
                if seq == last {
                    // 同一帧重复截到，正常（sender 帧间隔内多次截屏）
                    continue;
                } else if seq > last.wrapping_add(1) {
                    // 中间有帧未截到 → 漏帧
                    let missed = last.wrapping_add(1);
                    warn!(
                        "⚠ 漏帧检测: 期望 seq={}，实际截到 seq={}（跳过 {} 帧）",
                        missed, seq, seq.wrapping_sub(missed)
                    );
                }
                // seq == last + 1: 正常递增
                // seq < last: 倒退（理论上不该发生），忽略
            }
            last_data_seq = Some(seq);
        }

        match info.frame_type {
            FrameType::EOT => {
                info!("收到 EOT 信号");
                let _ = tx.send(DecodedFrame {
                    frame_type: FrameType::EOT,
                    frame_sequence: info.frame_sequence,
                    block_index: 0, total_blocks: 0,
                    payload: Vec::new(),
                    encoding_mode: EncodingMode::Binary,
                });
                break;
            }
            FrameType::Data => {
                let pixels_arc = Arc::new(pixels);
                let enc = enc_mode.clone();
                let tx_c = tx.clone();
                let lloc = loc.clone();
                pool.execute(move || {
                    if let Ok(frame) = decode_frame(&pixels_arc, img_w, img_h, &lloc, &enc) {
                        if !frame.payload.is_empty() {
                            let _ = tx_c.send(frame);
                        }
                    }
                });
            }
            _ => {}
        }
    }

    drop(tx);
    Ok(())
}
