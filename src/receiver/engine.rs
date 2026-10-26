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
use crate::common::types::{DecodedFrame, EncodingMode, FrameLocation, FrameType, IngestResult};
use crate::decoder::decode::{
    decode_format_info, decode_format_info_with_geometry, decode_frame, refine_frame_geometry,
    MIN_REFINE_PAYLOAD,
};
use crate::decoder::locate::{locate_frame_all_with_diag, LocateDiagnostics};
use crate::platform::{self, capture_full_screen, capture_virtual_desktop};
use crate::receiver::collect::FrameCollector;
use crate::receiver::reassemble::FileAssembler;

/// 解码任务队列积压上限（M10 背压）：超过后暂停提交新截屏帧，
/// 防止 display loop 重绘风暴期间任务与像素缓冲堆积。
const MAX_DECODE_QUEUE: usize = 8;

/// 执行接收操作的主入口（v0.19: 流水线架构）
pub fn run_receive(output: Option<std::path::PathBuf>) -> OptransResult<()> {
    info!("开始接收...");
    info!("截屏模式: 全屏（Ctrl+C 退出）");
    info!("定位模式: 全屏扫描 → 帧锁定");
    info!("流水线: capture → decode线程池 → aggregator");
    info!("正在扫描屏幕，等待发送端信号...");

    // ── channel ──
    let (tx, rx) = mpsc::channel::<DecodedFrame>();

    // ── shared state ──
    let shared_output = output.clone();
    let done = Arc::new(AtomicBool::new(false));

    // ── aggregator 线程 ──
    // M2: DoneGuard 保证 aggregator 无论正常返回还是 panic，done 都会被置位，
    // 否则 capture loop 会在 aggregator 消失后无限截屏。
    let done_clone = Arc::clone(&done);
    let aggregator = thread::spawn(move || {
        let _guard = DoneGuard(done_clone);
        run_aggregator(rx, shared_output)
    });

    // ── capture 循环 ──
    let result = run_capture_loop(tx, &done);

    // ── 等 aggregator 结束 ──
    match aggregator.join() {
        Ok(agg_result) => {
            result?; // propagate capture errors first
            agg_result
        }
        Err(_) => Err(OptransError::FrameDecode("aggregator 线程 panic".into())),
    }
}

/// M2: aggregator 退出守卫——无论正常返回还是 panic 展开，
/// 都将 done 置位，否则 capture loop 会在 aggregator 消失后无限截屏。
struct DoneGuard(Arc<AtomicBool>);

impl Drop for DoneGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
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
                        if meta.fec_enabled() {
                            info!(
                                "✅ 检测到发送端！文件: {} ({} bytes, {} 块, FEC k={} m={})",
                                meta.file_name,
                                meta.file_size,
                                meta.total_blocks,
                                meta.fec_k,
                                meta.fec_m
                            );
                        } else {
                            info!(
                                "✅ 检测到发送端！文件: {} ({} bytes, {} 块)",
                                meta.file_name, meta.file_size, meta.total_blocks
                            );
                        }
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
                    Ok(IngestResult::NewParity(g, p)) => {
                        info!("FEC 校验分片 组={} 序号={}", g, p);
                    }
                    Ok(IngestResult::EotReceived) => {
                        info!("收到 EOT 信号");
                        return finalize_transfer(collector, output_path);
                    }
                    // Duplicate：该块/分片早已摄入（重复截取或重传帧），幂等跳过
                    Ok(IngestResult::Duplicate) => { /* skip */ }
                    // Invalid：帧格式损坏，整帧丢弃（由 FEC 兜底）
                    Ok(IngestResult::Invalid) => {
                        warn!("帧格式损坏，已丢弃（交由 FEC 恢复）");
                    }
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
                return finalize_transfer(collector, output_path);
            }
        }
    }
}

/// 收到 EOT 或通道关闭后，尝试 FEC 恢复并写文件
fn finalize_transfer(
    mut collector: FrameCollector,
    output_path: Option<std::path::PathBuf>,
) -> OptransResult<()> {
    if collector.is_complete() {
        info!("所有数据块已收到，开始重组...");
        return flush_and_write(collector, output_path);
    }

    // 尝试 FEC 恢复
    let missing = collector.missing_blocks();
    info!(
        "缺失 {} 块，尝试 FEC 恢复...（已收到 {} 校验分片）",
        missing.len(),
        collector.parity_received()
    );

    match collector.try_fec_recover() {
        Ok(recovered) => {
            if collector.is_complete() {
                info!("✅ FEC 恢复成功，恢复 {} 块，开始重组...", recovered);
                flush_and_write(collector, output_path)
            } else {
                let still_missing = collector.missing_blocks();
                Err(OptransError::FrameDecode(format!(
                    "FEC 恢复后仍缺失 {} 块: {:?}，传输失败",
                    still_missing.len(),
                    still_missing
                )))
            }
        }
        Err(e) => Err(OptransError::FrameDecode(format!(
            "FEC 恢复失败: {}（缺失 {} 块）",
            e,
            missing.len()
        ))),
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

/// 截屏并定位帧候选。
///
/// 优先主显示器（快）；主屏无候选且存在多显示器时，回退扫描整个虚拟桌面
/// （发送端帧可能在副屏显示，如远程桌面窗口拖到副屏全屏）。
fn scan_frame() -> Option<(Vec<u8>, i32, i32, Vec<(FrameLocation, LocateDiagnostics)>, bool)> {
    if let Ok((pixels, w, h)) = capture_full_screen() {
        let cands = locate_frame_all_with_diag(&pixels, w, h);
        if !cands.is_empty() {
            return Some((pixels, w, h, cands, false));
        }
        if platform::has_multiple_monitors() {
            if let Ok((vp, vw, vh)) = capture_virtual_desktop() {
                let cands = locate_frame_all_with_diag(&vp, vw, vh);
                if !cands.is_empty() {
                    return Some((vp, vw, vh, cands, true));
                }
            }
        }
    }
    None
}

/// 长时间未检测到发送端信号时输出诊断提示（节流：每 10 秒一次）
fn warn_no_signal(start: std::time::Instant, last_warn: &mut Option<std::time::Instant>) {
    let elapsed = start.elapsed();
    if elapsed.as_secs() < 10 {
        return;
    }
    let should_warn = match last_warn {
        Some(t) => t.elapsed().as_secs() >= 10,
        None => true,
    };
    if !should_warn {
        return;
    }
    *last_warn = Some(std::time::Instant::now());
    warn!(
        "已等待 {}s 未检测到发送端信号。请确认: ① 发送端窗口可见且未被遮挡; \
         ② 多显示器环境下发送端帧所在屏幕在接收端可见; \
         ③ 远程桌面窗口缩放比例未导致图案严重失真",
        elapsed.as_secs()
    );
}

fn run_capture_loop(
    tx: mpsc::Sender<DecodedFrame>,
    done: &AtomicBool,
) -> OptransResult<()> {
    let pool = threadpool::ThreadPool::new(2);

    // ═══ Phase 1: 等待 Meta 帧 → 锁定位置 ═══
    let mut meta_loc: Option<FrameLocation> = None;
    let mut use_virtual_desktop = false;
    let scan_start = std::time::Instant::now();
    let mut last_warn: Option<std::time::Instant> = None;
    loop {
        if done.load(Ordering::SeqCst) {
            drop(tx);
            return Ok(());
        }

        let Some((pixels, img_w, img_h, candidates, is_virtual)) = scan_frame() else {
            warn_no_signal(scan_start, &mut last_warn);
            continue;
        };

        let pixels_arc = Arc::new(pixels);
        let mut found = false;
        for (loc, _) in &candidates {
            // 二维几何精修（v0.30）：小模块量化误差会让定位出的
            // modules_w/modules_h 漂移 ±15 模块，格式 CRC 全部失败；
            // 以格式信息 CRC 为裁决信号搜索正确几何，Meta 小载荷帧也能锁定。
            let Some((info, refined_loc)) =
                decode_format_info_with_geometry(&pixels_arc, img_w, img_h, loc)
            else {
                continue;
            };
            if info.frame_type != FrameType::Meta {
                continue;
            }
            if let Ok(frame) = decode_frame(&pixels_arc, img_w, img_h, &refined_loc, &EncodingMode::Auto) {
                if !frame.payload.is_empty() {
                    if refined_loc.modules_w != loc.modules_w
                        || refined_loc.modules_h != loc.modules_h
                    {
                        info!(
                            "几何精修: {}x{} → {}x{}（模块尺寸量化误差修正）",
                            loc.modules_w,
                            loc.modules_h,
                            refined_loc.modules_w,
                            refined_loc.modules_h
                        );
                    }
                    info!("编码模式: {:?}", frame.encoding_mode);
                    let _ = tx.send(frame);
                    meta_loc = Some(refined_loc);
                    use_virtual_desktop = is_virtual;
                    found = true;
                    break;
                }
            }
        }
        if found {
            info!("✓ Meta 帧解析成功，位置已锁定");
            break;
        }
    }
    let mut loc = meta_loc.unwrap();
    if use_virtual_desktop {
        info!("发送端帧位于非主显示器，切换为虚拟桌面截屏");
    }

    // ═══ Phase 2: 捕获数据帧 + FEC 校验帧 + EOT ═══
    // last_seq: 上一次截屏看到的非 Meta 帧序号（用于检测漏帧）
    //   - meta(seq=0) 之后第一个帧 seq=1
    //   - 同一帧重复截到（seq 相同）→ 正常，跳过
    //   - seq > last + 1 → 中间有帧未截到 → 漏帧告警
    let mut last_seq: Option<u16> = None;
    // 几何精修只做一次：格式信息锚定帧底部，其 CRC 对 modules_h 量化误差
    // 几乎不敏感，定位出的 modules_h 可能偏 ±10+ 模块；首个大载荷数据帧
    // 用严格 RS 解码裁决修正，后续所有帧复用正确位置。
    let mut geometry_refined = false;
    loop {
        if done.load(Ordering::SeqCst) {
            drop(tx);
            return Ok(());
        }

        let (pixels, img_w, img_h) = match if use_virtual_desktop {
            capture_virtual_desktop()
        } else {
            capture_full_screen()
        } {
            Ok(r) => r,
            Err(e) => {
                warn!("截屏失败: {}，重试...", e);
                continue;
            }
        };

        let info = match decode_format_info(&pixels, img_w, img_h, &loc) {
            Ok(i) => i,
            Err(_) => continue,
        };

        // 帧序号连续性检测（在 capture loop 中，不受线程池并发影响）
        // 对 Data、FecParity、EOT 帧均做检测
        //
        // 注意（M5）：重复帧不再跳过——首次截到的帧可能在线程池解码失败，
        // 若按 seq 去重跳过后续重复截取，该帧将永久丢失。
        // 重复帧照常送入解码，由 collector 的块级 first-write-wins 幂等去重。
        // last_seq 仅用于漏帧告警。
        if info.frame_type == FrameType::Data
            || info.frame_type == FrameType::FecParity
            || info.frame_type == FrameType::EOT
        {
            let seq = info.frame_sequence;
            if let Some(last) = last_seq {
                if seq != last {
                    if seq > last.wrapping_add(1) {
                        // 中间有帧未截到 → 漏帧
                        let missed = last.wrapping_add(1);
                        warn!(
                            "⚠ 漏帧检测: 期望 seq={}，实际截到 seq={}（跳过 {} 帧）",
                            missed,
                            seq,
                            seq.wrapping_sub(missed)
                        );
                    }
                    last_seq = Some(seq);
                }
                // seq == last: 同一帧重复截到，照常送入解码（幂等），不跳过
            } else {
                last_seq = Some(seq);
            }
        }

        match info.frame_type {
            FrameType::EOT => {
                info!("收到 EOT 信号");
                // M1: EOT 前 join 线程池，确保所有 in-flight 解码帧先进入 channel，
                // 避免 EOT 先于数据帧送达 aggregator 提前收尾。
                // threadpool 无 Drop 实现，不 join 的任务会随 worker 分离而丢失。
                pool.join();
                // M2: 发送失败说明 aggregator 已退出，静默结束即可
                let _ = tx.send(DecodedFrame {
                    frame_type: FrameType::EOT,
                    frame_sequence: info.frame_sequence,
                    block_index: 0,
                    total_blocks: 0,
                    payload: Vec::new(),
                    encoding_mode: EncodingMode::Binary,
                });
                drop(tx);
                return Ok(());
            }
            FrameType::Data | FrameType::FecParity => {
                // 首个大载荷数据帧：几何精修（一次性修正 modules_h）
                // 小载荷帧采样漂移不足，无法裁决，跳过并保留精修机会
                if !geometry_refined
                    && info.frame_type == FrameType::Data
                    && info.payload_bytes >= MIN_REFINE_PAYLOAD
                {
                    geometry_refined = true;
                    if let Some(refined) = refine_frame_geometry(
                        &pixels, img_w, img_h, &loc, &info, &EncodingMode::Auto,
                    ) {
                        if refined.modules_h != loc.modules_h {
                            info!(
                                "几何精修: modules_h {} → {}（非整数缩放修正）",
                                loc.modules_h, refined.modules_h
                            );
                            loc = refined;
                        }
                    }
                }
                let pixels_arc = Arc::new(pixels);
                let tx_c = tx.clone();
                let lloc = loc.clone();
                // M10: 背压——解码队列积压时暂停提交新任务，
                // 防止 display loop 风暴期间任务与像素缓冲堆积。
                while pool.queued_count() > MAX_DECODE_QUEUE {
                    if done.load(Ordering::SeqCst) {
                        drop(tx);
                        return Ok(());
                    }
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                pool.execute(move || {
                    if let Ok(frame) = decode_frame(&pixels_arc, img_w, img_h, &lloc, &EncodingMode::Auto) {
                        if !frame.payload.is_empty() {
                            // M2: 发送失败说明 aggregator 已退出，直接结束本任务
                            let _ = tx_c.send(frame);
                        }
                    }
                });
            }
            _ => {}
        }
    }
}
