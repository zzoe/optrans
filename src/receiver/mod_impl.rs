//! 接收端主循环实现
//!
//! 循环全屏截屏 → 全屏扫描定位 → 解码 → 收集去重 → P 帧合并 → 重组校验 → 写文件
//! v0.2 新增：encoding_mode 传递、帧类型日志增强。
//! v0.3 新增：循环时序日志。
//! v0.4 重构：全屏接收，去除区域锁定。
//! v0.5: 使用角落定位 (locate_by_corners) 替代全屏扫描定位。
//! v0.7: 改用全屏扫描定位 (locate_frame_with_diag)，支持任意窗口位置/尺寸/缩放，
//!       适用于 Horizon client 等远程桌面场景。
//! v0.8: 高频解码错误日志增加节流（同类错误每 2 秒最多输出一次），减少无效刷屏。

use std::time::{Duration, Instant};

use crate::cli::ReceiveArgs;
use crate::common::error::{OptransError, OptransResult};
use crate::common::types::{FrameKind, IngestResult};
use crate::decoder::decode::decode_frame;
use crate::decoder::locate::locate_frame_with_diag;
use crate::receiver::capture::capture_full_screen;
use crate::receiver::collect::FrameCollector;
use crate::receiver::reassemble::FileAssembler;

/// 日志节流器：相同类型的错误消息每 2 秒最多输出一次
struct LogThrottle {
    last_category: &'static str,
    last_time: Instant,
    suppressed_count: u64,
}

impl LogThrottle {
    fn new() -> Self {
        LogThrottle {
            last_category: "",
            last_time: Instant::now(),
            suppressed_count: 0,
        }
    }

    /// 检查是否应该打印此类别消息。
    /// 返回 `(should_print, suppressed_count_before)`。
    /// 调用方应在 `should_print==true` 时使用 `suppressed_count_before` 告知用户被抑制的条数。
    fn check(&mut self, category: &'static str) -> (bool, u64) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_time);

        if category != self.last_category || elapsed >= Duration::from_secs(2) {
            // 类别变化或超过 2 秒：允许输出
            let suppressed = self.suppressed_count;
            self.last_category = category;
            self.last_time = now;
            self.suppressed_count = 0;
            (true, suppressed)
        } else {
            // 同类同周期内：抑制
            self.suppressed_count += 1;
            (false, 0)
        }
    }
}

/// 执行接收操作的主入口
pub fn run_receive(args: ReceiveArgs) -> OptransResult<()> {
    let encoding_mode = args.encoding_mode.clone();
    let _module_pixels = args.module_pixels;

    eprintln!("[optrans] 开始接收...");
    eprintln!("[optrans] 截屏模式: 全屏（无超时，Ctrl+C 退出）");
    eprintln!("[optrans] 定位模式: 全屏扫描定位（自适应窗口尺寸/缩放）");
    eprintln!("[optrans] 编码模式: {:?}", encoding_mode);
    eprintln!("[optrans] 正在扫描屏幕，等待发送端信号...");

    let mut collector = FrameCollector::new();

    // 全屏扫描找不到帧时的等待追踪
    let mut no_frame_start = Instant::now();
    let mut last_heartbeat = Instant::now();

    // 解码错误节流器
    let mut throttle_fmt = LogThrottle::new();
    let mut throttle_decode = LogThrottle::new();

    // 一度收到有效帧后，重置日志状态
    let mut ever_got_valid = false;

    // 循环计数（用于时序日志）
    let mut loop_count: u64 = 0;

    loop {
        loop_count += 1;
        let loop_start = Instant::now();

        // 1. 全屏截取
        let t0 = Instant::now();
        let (pixels, img_w, img_h) = match capture_full_screen() {
            Ok(result) => result,
            Err(e) => {
                eprintln!("[optrans] 截屏失败: {}，重试...", e);
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let capture_ms = t0.elapsed().as_millis();

        // 2. 全屏扫描定位帧（不依赖固定角落位置）
        let t1 = Instant::now();
        let location = match locate_frame_with_diag(&pixels, img_w, img_h) {
            Some((loc, _diag)) => {
                no_frame_start = Instant::now();
                loc
            }
            None => {
                // 未找到帧，检查是否需要输出心跳
                let waited = no_frame_start.elapsed();
                if waited.as_secs() >= 3 && last_heartbeat.elapsed() >= Duration::from_secs(3) {
                    eprintln!(
                        "[optrans] 等待发送端... (已等待 {}s, {}×{} 截屏中未找到二维码)",
                        waited.as_secs(), img_w, img_h
                    );
                    last_heartbeat = Instant::now();
                }
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        let locate_ms = t1.elapsed().as_millis();

        // 3. 解码帧
        let t2 = Instant::now();
        let decoded = match decode_frame(&pixels, img_w, img_h, &location, &encoding_mode) {
            Ok(frame) => {
                // 解码成功，重置节流器
                throttle_fmt = LogThrottle::new();
                throttle_decode = LogThrottle::new();
                frame
            }
            Err(OptransError::InvalidFormatInfo(msg)) => {
                let (print, suppressed) = throttle_fmt.check("fmt");
                if print {
                    let extra = if suppressed > 0 {
                        format!(" (已抑制 {} 条同类错误)", suppressed)
                    } else {
                        String::new()
                    };
                    eprintln!(
                        "[optrans] 格式信息解码失败: {}{}",
                        msg, extra
                    );
                    // 仅第一次输出位置详情
                    if suppressed == 0 {
                        eprintln!(
                            "[optrans]   位置: tl=({:.1},{:.1}) ms={:.2} modules={}w×{}h",
                            location.tl.center_x, location.tl.center_y,
                            location.tl.module_size, location.modules_w, location.modules_h
                        );
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(e) => {
                let (print, suppressed) = throttle_decode.check("decode");
                if print {
                    let extra = if suppressed > 0 {
                        format!(" (已抑制 {} 条同类错误)", suppressed)
                    } else {
                        String::new()
                    };
                    eprintln!("[optrans] 帧解码失败: {}{}", e, extra);
                }
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        let decode_ms = t2.elapsed().as_millis();

        // 首次成功解码
        if !ever_got_valid {
            ever_got_valid = true;
            eprintln!("[optrans] ✓ 首个有效帧解码成功，已与发送端建立连接");
        }

        // 帧类型标识
        let kind_str = match decoded.frame_kind {
            FrameKind::I => "I",
            FrameKind::P => "P",
        };

        // 4. 摄入帧
        match collector.ingest(decoded) {
            Ok(IngestResult::NewMeta(meta)) => {
                eprintln!(
                    "[optrans] ✅ 检测到发送端！文件: {} ({} bytes, {} 块)",
                    meta.file_name, meta.file_size, meta.total_blocks
                );
            }
            Ok(IngestResult::NewBlock(idx)) => {
                let total = collector.meta().map(|m| m.total_blocks).unwrap_or(0);
                let received = collector.blocks_received();
                // 进度条式输出
                eprintln!(
                    "[optrans] 数据块({}) #{}, 进度: {}/{} ({:.0}%)",
                    kind_str,
                    idx,
                    received,
                    total,
                    if total > 0 { received as f64 / total as f64 * 100.0 } else { 0.0 }
                );
            }
            Ok(IngestResult::EotReceived) => {
                eprintln!("[optrans] 收到 EOT 信号({})", kind_str);

                if collector.is_complete() {
                    eprintln!("[optrans] 所有数据块已收到，开始重组...");
                    break;
                } else {
                    let missing = collector.missing_blocks();
                    eprintln!(
                        "[optrans] 收到 EOT 但缺失 {} 块: {:?}，继续等待...",
                        missing.len(),
                        missing
                    );
                }
            }
            Ok(IngestResult::Duplicate) => {
                // 忽略重复帧
            }
            Ok(IngestResult::Invalid) => {
                // 忽略无效帧
            }
            Err(e) => {
                eprintln!("[optrans] 帧处理失败: {}", e);
            }
        }

        // 每 10 次循环打印一次时序日志
        let total_ms = loop_start.elapsed().as_millis();
        if loop_count % 10 == 0 {
            eprintln!(
                "[optrans] 时序: 截屏{}ms 定位{}ms 解码{}ms 总{}ms @{}fps",
                capture_ms,
                locate_ms,
                decode_ms,
                total_ms,
                1000 / total_ms.max(1)
            );
        }

        // 短暂休眠避免 CPU 空转
        std::thread::sleep(Duration::from_millis(10));
    }

    // 5. 重组并写文件
    let meta = collector.meta().cloned().ok_or_else(|| {
        OptransError::FrameDecode("未收到元数据帧，无法重组文件".to_string())
    })?;

    let expected_file_size = meta.file_size;
    let blocks = collector.take_blocks();

    let total_blocks = meta.total_blocks as usize;
    if blocks.len() < total_blocks {
        let mut missing = Vec::new();
        for i in 0..(total_blocks as u32) {
            if !blocks.contains_key(&i) {
                missing.push(i);
            }
        }
        eprintln!(
            "[optrans] 警告: 缺少 {} 个数据块，文件可能不完整",
            missing.len()
        );
    }

    let mut assembler = FileAssembler::new(meta);

    let mut indices: Vec<u32> = blocks.keys().cloned().collect();
    indices.sort();

    for idx in &indices {
        if let Some(data) = blocks.get(idx) {
            assembler.add_block(*idx, data.clone());
        }
    }

    assembler.finalize_to_file(&args.output)?;

    eprintln!(
        "[optrans] 文件已保存: {:?} ({} bytes)",
        args.output, expected_file_size
    );

    Ok(())
}
