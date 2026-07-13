//! 帧定位模块
//!
//! 通过 Finder Pattern 检测定位屏幕截图中帧的位置。
//!
//! v0.5 新增：`locate_by_corners` — 从屏幕三个已知角落直接定位 finder pattern，
//! 不再扫描全屏。原始 `locate_frame` 保留作为降级方案。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use log::debug;

use crate::common::types::{FinderPattern, FrameLocation};

/// locate_frame 的诊断统计信息
#[derive(Debug, Clone)]
pub struct LocateDiagnostics {
    /// 水平扫描找到的候选点数量
    pub candidates_found: usize,
    /// 垂直验证通过的候选点数量
    pub candidates_verified: usize,
}

/// 诊断日志时间戳（毫秒），用于限制每 2 秒最多输出一次
static LAST_DIAG_MS: AtomicU64 = AtomicU64::new(0);

/// 返回当前系统时间的毫秒表示
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// 频率限制诊断输出：每 2 秒最多一次
fn diag_ratelimited(msg: &str) {
    let now = now_ms();
    let last = LAST_DIAG_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last) >= 2000 {
        LAST_DIAG_MS.store(now, Ordering::Relaxed);
        debug!("{}", msg);
    }
}

// ─── v0.5: 角落定位（推荐） ───

/// 从三个已知角落位置直接定位帧，不扫描全屏
///
/// Finder pattern 位于帧的三个角落，因为发送端在编码图案四周留有
/// `FRAME_MARGIN_PX` 像素黑边，故 finder pattern 7×7 模块块的左上角
/// 也需从屏幕绝对角落向内偏移 `margin` 像素：
/// - TL: (margin, margin)
/// - TR: (capture_w - 7*m - margin, margin)
/// - BL: (margin, capture_h - 7*m - margin)
///
/// 在每个角落的预期位置附近 ±search_radius 像素搜索 finder pattern。
///
/// # 参数
/// - `pixels`: RGBA 像素缓冲区（全屏截图）
/// - `capture_w`: 截图宽度
/// - `capture_h`: 截图高度
/// - `module_pixels`: 每模块像素数（发送端参数，必须与发送端一致）
///
/// # 返回
/// `Some(FrameLocation)` 如果三个角落都找到了 finder pattern，否则 `None`
pub fn locate_by_corners(
    pixels: &[u8],
    capture_w: i32,
    capture_h: i32,
    module_pixels: u32,
) -> Option<FrameLocation> {
    let m = module_pixels as i32;
    let margin = crate::common::constants::FRAME_MARGIN_PX_DEFAULT as i32;
    let search_radius: i32 = 20;

    let w = capture_w as usize;
    let h = capture_h as usize;

    // 三个角落的预期 finder pattern 左上角像素坐标
    //（各从屏幕绝对角落向内偏移 margin 像素，与发送端黑边对齐）
    // TL: (margin, margin)
    let tl = find_finder_at_corner(pixels, w, h, margin, margin, m, search_radius)?;

    // TR: (capture_w - 7*m - margin, margin)
    let tr_corner_x = capture_w - 7 * m - margin;
    let tr = find_finder_at_corner(pixels, w, h, tr_corner_x, margin, m, search_radius)?;

    // BL: (margin, capture_h - 7*m - margin)
    let bl_corner_y = capture_h - 7 * m - margin;
    let bl = find_finder_at_corner(pixels, w, h, margin, bl_corner_y, m, search_radius)?;

    // 从角落 finder 位置推算模块尺寸
    let avg_module_size = (tl.2 + tr.2 + bl.2) / 3.0;

    // modules_w: TL→TR 的水平距离 / module_size + 7 (finder 自身宽度)
    let modules_w = ((tr.0 - tl.0) / avg_module_size).round() as u32 + 7;
    // modules_h: TL→BL 的垂直距离 / module_size + 7
    let modules_h = ((bl.1 - tl.1) / avg_module_size).round() as u32 + 7;

    // 合理性检查
    if !(25..=1000).contains(&modules_w) || !(25..=1000).contains(&modules_h) {
        diag_ratelimited(&format!(
            "角落定位: modules 尺寸异常 ({}w×{}h)，跳过",
            modules_w, modules_h
        ));
        return None;
    }

    let modules_per_side = modules_w.max(modules_h);

    Some(FrameLocation {
        tl: FinderPattern {
            center_x: tl.0,
            center_y: tl.1,
            module_size: tl.2,
        },
        tr: FinderPattern {
            center_x: tr.0,
            center_y: tr.1,
            module_size: tr.2,
        },
        bl: FinderPattern {
            center_x: bl.0,
            center_y: bl.1,
            module_size: bl.2,
        },
        modules_per_side,
        modules_w,
        modules_h,
        module_pixels: tl.2.round() as u32, // 取 TL finder 估算的模块像素
    })
}

/// 在指定角落附近搜索 finder pattern
///
/// # 参数
/// - `corner_x`, `corner_y`: finder pattern 7×7 模块块的左上角预期像素坐标
/// - `m`: module_pixels（预期每模块像素数）
/// - `search_radius`: 搜索半径（像素）
///
/// # 返回
/// `Some((center_x, center_y, module_size))` 如果找到，否则 `None`
fn find_finder_at_corner(
    pixels: &[u8],
    w: usize,
    h: usize,
    corner_x: i32,
    corner_y: i32,
    m: i32,
    search_radius: i32,
) -> Option<(f64, f64, f64)> {
    // Finder pattern 中心预期位置：corner + 3.5 模块
    let expected_cx = corner_x as f64 + 3.5 * m as f64;
    let expected_cy = corner_y as f64 + 3.5 * m as f64;

    // 水平扫描：在预期 y 坐标附近扫描 1:1:3:1:1 比例
    let y_start = ((expected_cy as i32) - search_radius).max(0);
    let y_end = ((expected_cy as i32) + search_radius).min(h as i32 - 1);

    let x_start = ((expected_cx as i32) - 10 * m).max(1);
    let x_end = ((expected_cx as i32) + 10 * m).min(w as i32 - 1);

    for scan_y in (y_start..y_end).step_by(2) {
        let row_offset = (scan_y as usize) * w * 4;

        let mut state_count = [0i32; 5];
        let mut state_idx: usize = 0;

        // 初始化
        if x_start >= w as i32 {
            continue;
        }
        let init_idx = row_offset + (x_start as usize) * 4;
        if init_idx + 2 >= pixels.len() {
            continue;
        }
        let init_lum = pixel_luminance(pixels, init_idx);
        let mut current_state: u8 = if init_lum < 128 { 1 } else { 0 };
        state_count[0] = 1;

        for x in (x_start + 1)..x_end {
            let idx = row_offset + (x as usize) * 4;
            if idx + 2 >= pixels.len() {
                break;
            }
            let lum = pixel_luminance(pixels, idx);
            let bit: u8 = if lum < 128 { 1 } else { 0 };

            if bit == current_state {
                state_count[state_idx] += 1;
            } else {
                if state_idx == 4 {
                    if check_finder_ratio(&state_count) {
                        // x 是 pattern 之后的第一个像素，pattern 最后像素在 x-1
                        // center = 第3段(中央黑块)的中点 = x - s[4] - s[3] - s[2]/2
                        let center_x = (x as f64)
                            - (state_count[4] as f64 + state_count[3] as f64 + state_count[2] as f64 / 2.0);
                        // 模块尺寸由内部 3 段估计（端段可能被合并污染）
                        let mid_sum = (state_count[1] + state_count[2] + state_count[3]) as f64;
                        let module_size = mid_sum / 5.0;

                        // 验证找到的中心是否接近预期位置
                        if (center_x - expected_cx).abs() < (search_radius + 5 * m) as f64 {
                            // 垂直验证
                            let vfy = verify_finder_vertical_at(
                                pixels, w, h,
                                center_x, scan_y as f64,
                            );
                            if vfy {
                                return Some((center_x, scan_y as f64, module_size));
                            }
                        }
                    }
                    for i in 0..4 {
                        state_count[i] = state_count[i + 1];
                    }
                    state_count[4] = 0;
                    state_idx = 4;
                } else {
                    state_idx += 1;
                    state_count[state_idx] = 0;
                }
                current_state = bit;
                state_count[state_idx] += 1;
            }
        }
    }

    None
}

/// 在指定位置做垂直方向 finder pattern 验证（简化版）
///
/// 在 cx 附近 ±4 像素（约 1 模块）的多列上扫描，
/// 因为 finder 中心列全黑，必须偏移 1 模块才能看到 1:1:3:1:1 pattern。
fn verify_finder_vertical_at(
    pixels: &[u8],
    w: usize,
    h: usize,
    cx: f64,
    cy: f64,
) -> bool {
    let cx_i = cx as i32;
    let cy_i = cy as i32;

    if cx_i < 4 || cx_i >= w as i32 - 4 {
        return false;
    }

    // 以候选点为中心，上下各扩展 20 像素（覆盖 finder pattern 7 模块高度）
    let y_start = (cy_i - 20).max(0) as usize;
    let y_end = (cy_i + 20).min(h as i32 - 1) as usize;

    // 在 cx 附近 ±4 像素（约 1 模块）范围内扫描多列
    for dx in [-4i32, -2, 0, 2, 4] {
        let x_idx = ((cx_i + dx) as usize) * 4;
        if check_vertical_pattern(pixels, w, x_idx, y_start, y_end) {
            return true;
        }
    }

    false
}

// ─── 原有全屏扫描定位（保留作为降级方案） ───

/// 在像素缓冲区中定位帧（全屏扫描）
///
/// # 参数
/// - `pixels`: RGBA 像素缓冲区
/// - `width`: 图像宽度（像素）
/// - `height`: 图像高度（像素）
///
/// # 返回
/// `Some(FrameLocation)` 如果检测到有效的帧定位图案，否则 `None`
pub fn locate_frame(pixels: &[u8], width: i32, height: i32) -> Option<FrameLocation> {
    // 参数验证
    if width < 50 || height < 50 {
        return None;
    }

    let w = width as usize;
    let h = height as usize;

    // 第一步：扫描水平行，找 1:1:3:1:1 比例
    let candidates = find_finder_candidates(pixels, w, h);
    let candidate_count = candidates.len();

    if candidates.len() < 3 {
        if candidate_count > 0 {
            diag_ratelimited(&format!(
                "未找到足够的 Finder 候选点(找到 {} 个，需要 ≥3)",
                candidate_count
            ));
        }
        return None;
    }

    // 第二步：垂直方向交叉验证每个候选
    let verified: Vec<FinderCandidate> = candidates
        .into_iter()
        .filter(|fp| verify_finder_vertical(pixels, w, h, fp))
        .collect();
    let verified_count = verified.len();

    // 如果找到了候选但未通过垂直验证，输出诊断
    if candidate_count >= 3 && verified_count < 3 {
        diag_ratelimited(&format!(
            "候选点未通过垂直验证(候选{}个, 通过{}个, 需要≥3)",
            candidate_count, verified_count
        ));
    }

    if verified.len() < 3 {
        return None;
    }

    // 第三步：从验证过的候选中找到所有构成直角三角形的 finder pattern
    // 返回按像素面积降序排列的候选列表，取第一个（面积最大）
    let results = find_three_corners(&verified);

    if results.is_empty() {
        // 最终未能找到 3 个 corner 时，输出诊断
        diag_ratelimited(&format!(
            "验证通过但未能构成有效三角形(候选{}个, 验证通过{}个)",
            candidate_count, verified_count
        ));
    }

    results.into_iter().next()
}

/// Finder Pattern 候选点（水平扫描结果）
#[derive(Debug, Clone)]
struct FinderCandidate {
    center_x: f64,
    center_y: f64,
    module_size: f64,
}

/// 将像素值转为灰度（用于黑白判断）
#[inline]
fn pixel_luminance(pixels: &[u8], idx: usize) -> u8 {
    // RGBA: R=idx, G=idx+1, B=idx+2
    let r = pixels[idx] as u32;
    let g = pixels[idx + 1] as u32;
    let b = pixels[idx + 2] as u32;
    // 加权灰度
    ((r * 299 + g * 587 + b * 114) / 1000) as u8
}

/// 水平扫描找 1:1:3:1:1 比例
fn find_finder_candidates(pixels: &[u8], w: usize, h: usize) -> Vec<FinderCandidate> {
    let mut candidates = Vec::new();
    let step = 3; // 每隔几行扫描一次以加速

    for y in (0..h).step_by(step) {
        let row_offset = y * w * 4;

        let mut state_count = [0i32; 5];
        let mut state_idx = 0usize;

        // 初始化：跳过开头
        let init_lum = pixel_luminance(pixels, row_offset);
        let mut current_state = if init_lum < 128 { 1 } else { 0 };
        state_count[0] = 1;

        for x in 1..w {
            let idx = row_offset + x * 4;
            let lum = pixel_luminance(pixels, idx);
            let bit = if lum < 128 { 1u8 } else { 0u8 };

            if bit == current_state {
                state_count[state_idx] += 1;
            } else {
                // 状态切换
                if state_idx == 4 {
                    // 已完成 5 段，检查比例
                    if check_finder_ratio(&state_count) {
                        // x 是 pattern 之后的第一个像素，pattern 最后像素在 x-1
                        // center = 第3段(中央黑块)的中点 = x - s[4] - s[3] - s[2]/2
                        let center_x = (x as f64)
                            - (state_count[4] as f64 + state_count[3] as f64 + state_count[2] as f64 / 2.0);
                        let center_y = y as f64;
                        // 模块尺寸由内部 3 段估计（端段可能被合并污染）
                        let mid_sum = (state_count[1] + state_count[2] + state_count[3]) as f64;
                        let module_size = mid_sum / 5.0;

                        candidates.push(FinderCandidate {
                            center_x,
                            center_y,
                            module_size,
                        });
                    }
                    // 滑动窗口：移除第一段
                    for i in 0..4 {
                        state_count[i] = state_count[i + 1];
                    }
                    state_count[4] = 0;
                    state_idx = 4;
                } else {
                    state_idx += 1;
                    state_count[state_idx] = 0;
                }
                current_state = bit;
                state_count[state_idx] += 1;
            }
        }

        // 检查行末
        if state_idx == 4 && check_finder_ratio(&state_count) {
            // w 是行末之后的位置，pattern 最后像素在 w-1
            // center = 第3段(中央黑块)的中点 = w - s[4] - s[3] - s[2]/2
            let center_x = (w as f64)
                - (state_count[4] as f64 + state_count[3] as f64 + state_count[2] as f64 / 2.0);
            let center_y = y as f64;
            // 模块尺寸由内部 3 段估计（端段可能被合并污染）
            let mid_sum = (state_count[1] + state_count[2] + state_count[3]) as f64;
            let module_size = mid_sum / 5.0;

            candidates.push(FinderCandidate {
                center_x,
                center_y,
                module_size,
            });
        }
    }

    // 合并距离过近的候选（去重）
    merge_nearby_candidates(&candidates, 10.0)
}

/// 检查 5 段是否符合 1:1:3:1:1 比例
///
/// 宽容策略：仅内部 3 段（1:3:1）严格匹配，端段（0 和 4）只要求下界 ≥0.5 模块。
/// 因为 finder 的端段可能与紧邻的数据区黑色模块合并，导致端段被拉长，
/// 此时若强制上下界会漏检。中心 3 段由 finder 内部结构保证不会与外界合并。
///
/// 模块尺寸仅由内部 3 段总和 / 5 估计（端段不参与，避免被合并段污染）。
fn check_finder_ratio(counts: &[i32; 5]) -> bool {
    // 内部 3 段总和应 = 5 模块（1+3+1）
    let mid_sum = counts[1] + counts[2] + counts[3];
    if mid_sum <= 0 {
        return false;
    }
    let module_est = mid_sum as f64 / 5.0;
    // 模块尺寸下界 2.0：真实发送帧 module_size 通常 ≥3，噪声误检多在 1-2
    if module_est < 2.0 {
        return false;
    }

    // 内部 3 段严格匹配 1:3:1（容差 25%，避免误匹配 finder 行 1/5 的 1:5:1 模式）
    let tolerance = 0.25;
    let expected_mid = [1.0, 3.0, 1.0];
    for (i, &exp) in expected_mid.iter().enumerate() {
        let actual = counts[i + 1] as f64 / module_est;
        if (actual - exp).abs() > tolerance {
            return false;
        }
    }

    // 端段只要求下界 ≥ 0.5 模块（上界不限，可能合并数据区）
    // 同时端段不能太短（至少 0.5 模块），避免噪声误判
    for i in [0, 4] {
        let actual = counts[i] as f64 / module_est;
        if actual < 0.5 {
            return false;
        }
    }

    true
}

/// 垂直方向交叉验证 finder pattern
///
/// 在候选中心 x 附近 ±1 模块的多列上做垂直扫描，
/// 寻找 1:1:3:1:1 比例。任一列匹配即通过。
/// 注意：finder pattern 中心列全是黑色，必须扫描偏移 1 模块的列才能看到 pattern。
fn verify_finder_vertical(
    pixels: &[u8],
    w: usize,
    h: usize,
    fp: &FinderCandidate,
) -> bool {
    let cx = fp.center_x as i32;
    let cy = fp.center_y as i32;
    let ms = fp.module_size as i32;

    if cx < ms || cx >= w as i32 - ms {
        return false;
    }

    // 以候选点为中心，上下各扩展 5 个模块
    let half_span = (fp.module_size * 5.0) as i32;
    let y_start = (cy - half_span).max(0) as usize;
    let y_end = (cy + half_span).min(h as i32 - 1) as usize;

    if y_end <= y_start {
        return false;
    }

    // v0.15: 检测 finder 下方是否存在大量黑边（FRAME_MARGIN_PX > 0 特征）。
    // 黑边与 finder 边框合并会截断垂直扫描的 5 段 pattern。
    // 仅在实际检测到黑边时跳过验证，避免误伤无 margin 场景。
    let below_check_y = (cy + half_span + 2).min(h as i32 - 2) as usize;
    if below_check_y < h {
        let check_idx = below_check_y * w * 4 + (cx as usize).min(w - 1) * 4;
        if check_idx + 2 < pixels.len() {
            // 采样下方 2 模块区域是否全黑（margin）
            let margin_range = (fp.module_size * 2.0) as usize;
            let mut all_black = true;
            for dy in 0..margin_range.min(h - below_check_y) {
                let idx = (below_check_y + dy) * w * 4 + (cx as usize).min(w - 1) * 4;
                if idx + 2 < pixels.len() && pixel_luminance(pixels, idx) >= 80 {
                    all_black = false;
                    break;
                }
            }
            if all_black {
                return true; // margin detected, skip verification
            }
        }
    }

    // 在中心 x 附近 ±1 模块范围内的若干列上尝试垂直扫描
    // finder 中心列全黑，必须扫描偏移约 1 模块的列（col 2 或 col 4）
    let step = (ms / 2).max(1);
    for dx in (-ms..=ms).step_by(step as usize) {
        let x_idx = ((cx + dx) as usize) * 4;
        if check_vertical_pattern(pixels, w, x_idx, y_start, y_end) {
            return true;
        }
    }

    false
}

/// 在指定列上做垂直 1:1:3:1:1 pattern 扫描
fn check_vertical_pattern(
    pixels: &[u8],
    w: usize,
    x_idx: usize,
    y_start: usize,
    y_end: usize,
) -> bool {
    let mut state_count = [0i32; 5];
    let mut state_idx = 0usize;

    let init_idx = y_start * w * 4 + x_idx;
    if init_idx + 2 >= pixels.len() {
        return false;
    }
    let init_lum = pixel_luminance(pixels, init_idx);
    let mut current_state = if init_lum < 128 { 1u8 } else { 0u8 };
    state_count[0] = 1;

    for y in (y_start + 1)..=y_end {
        let idx = y * w * 4 + x_idx;
        if idx + 2 >= pixels.len() {
            break;
        }
        let lum = pixel_luminance(pixels, idx);
        let bit = if lum < 128 { 1u8 } else { 0u8 };

        if bit == current_state {
            state_count[state_idx] += 1;
        } else {
            if state_idx == 4 {
                if check_finder_ratio(&state_count) {
                    return true;
                }
                for i in 0..4 {
                    state_count[i] = state_count[i + 1];
                }
                state_count[4] = 0;
                state_idx = 4;
            } else {
                state_idx += 1;
                state_count[state_idx] = 0;
            }
            current_state = bit;
            state_count[state_idx] += 1;
        }
    }

    state_idx == 4 && check_finder_ratio(&state_count)
}

/// 合并位置过近的候选点
fn merge_nearby_candidates(
    candidates: &[FinderCandidate],
    max_dist: f64,
) -> Vec<FinderCandidate> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut merged: Vec<FinderCandidate> = Vec::new();
    let mut used = vec![false; candidates.len()];

    for i in 0..candidates.len() {
        if used[i] {
            continue;
        }
        used[i] = true;

        let mut sum_x = candidates[i].center_x;
        let mut sum_y = candidates[i].center_y;
        let mut sum_ms = candidates[i].module_size;
        let mut count = 1;

        for j in (i + 1)..candidates.len() {
            if used[j] {
                continue;
            }
            let dx = candidates[i].center_x - candidates[j].center_x;
            let dy = candidates[i].center_y - candidates[j].center_y;
            let dist = (dx * dx + dy * dy).sqrt();

            if dist < max_dist {
                sum_x += candidates[j].center_x;
                sum_y += candidates[j].center_y;
                sum_ms += candidates[j].module_size;
                count += 1;
                used[j] = true;
            }
        }

        merged.push(FinderCandidate {
            center_x: sum_x / count as f64,
            center_y: sum_y / count as f64,
            module_size: sum_ms / count as f64,
        });
    }

    merged
}

/// 从验证过的 finder pattern 中找到所有构成等腰直角三角形的组合。
///
/// 使用 O(n³) 组合遍历，对每三个候选点检查几何约束。
/// 返回所有通过约束的三角组，按像素面积（TL↔TR 距离 × TL↔BL 距离）降序排列。
///
/// 为什么返回多个候选：
/// - 定位阶段无法可靠区分真假帧（噪声可能形成比真帧更大的三角）
/// - 返回多候选，由解码阶段（CRC + payload 校验）决定哪个是真帧
/// - QR 码识别思路：不靠单一启发式，靠纠错码验证
///
/// 为什么用像素面积排序：
/// - 真帧全屏铺满时 finder 间距最大，噪声假三角组间距小得多
/// - 像素面积优先确保真帧排在候选列表前面（CPU 代价最小）
fn find_three_corners(candidates: &[FinderCandidate]) -> Vec<FrameLocation> {
    let n = candidates.len();
    if n < 3 {
        return Vec::new();
    }

    // 按 module_size 聚类，每组独立 O(n³)。
    // 真帧 finder 的 ms 相近且与噪声候选的 ms 差异大（如 ms≈4 vs ms≈2），
    // 聚类后真帧所在组 n 很小（~3-20），O(n³) 可忽略。
    let clusters = cluster_by_module_size(candidates);

    let mut all_results: Vec<(FrameLocation, f64)> = Vec::new();

    for cluster in &clusters {
        if cluster.len() < 3 {
            continue;
        }
        search_triangles(cluster, &mut all_results);
    }

    // 跨组诊断（限频）
    if !all_results.is_empty() {
        let frame_like_count = all_results.iter()
            .filter(|(loc, _)| {
                // v0.15: 放宽帧型范围，覆盖 FRAME_MARGIN_PX=120 的 580×340 及原始 640×400
                loc.modules_w >= 550 && loc.modules_w <= 710
                    && loc.modules_h >= 300 && loc.modules_h <= 460
            })
            .count();
        let best = all_results.iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((_, best_area)) = best {
            diag_ratelimited(&format!(
                "find_3c: {}群×{}候选, 三角组{}个, 640×400型{}个(best area={:.0})",
                clusters.len(), n, all_results.len(), frame_like_count, best_area
            ));
        }
    }

    // 排序：帧型候选始终排在前面（同类内按像素面积降序），
    // 确保真帧优先被 decode 尝试，不被面积更大的假帧抢位。
    // 剩下的按像素面积降序。
    all_results.sort_by(|a, b| {
        // v0.15: 放宽帧型范围，覆盖 FRAME_MARGIN_PX=120 的 580×340
        let a_is_frame = a.0.modules_w >= 550 && a.0.modules_w <= 710
            && a.0.modules_h >= 300 && a.0.modules_h <= 460;
        let b_is_frame = b.0.modules_w >= 550 && b.0.modules_w <= 710
            && b.0.modules_h >= 300 && b.0.modules_h <= 460;
        match (a_is_frame, b_is_frame) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal),
        }
    });
    const MAX_CANDIDATES: usize = 50;
    all_results.truncate(MAX_CANDIDATES);
    all_results.into_iter().map(|(loc, _)| loc).collect()
}

/// 按 module_size 聚类：排序后，相邻候选的 ms 差距 ≤0.5 归为同组
fn cluster_by_module_size(candidates: &[FinderCandidate]) -> Vec<Vec<&FinderCandidate>> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<&FinderCandidate> = candidates.iter().collect();
    sorted.sort_by(|a, b| {
        a.module_size
            .partial_cmp(&b.module_size)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut clusters: Vec<Vec<&FinderCandidate>> = Vec::new();
    let mut current: Vec<&FinderCandidate> = vec![sorted[0]];
    let mut group_start_ms = sorted[0].module_size;

    for &cand in &sorted[1..] {
        if (cand.module_size - group_start_ms).abs() <= 0.5 {
            current.push(cand);
        } else {
            clusters.push(current);
            current = vec![cand];
            group_start_ms = cand.module_size;
        }
    }
    clusters.push(current);
    clusters
}

/// 对单个候选组做 O(n³) 三角搜索，结果追加到 results。
/// v0.17: 每簇结果上限 200，防止大簇 O(n³) 膨胀（479→109M 次迭代 >400ms）。
/// 真帧三角在迭代早期出现，提前截断不影响定位精度。
fn search_triangles(
    candidates: &[&FinderCandidate],
    results: &mut Vec<(FrameLocation, f64)>,
) {
    let n = candidates.len();
    if n < 3 {
        return;
    }

    const MAX_PER_CLUSTER: usize = 200;
    let start_count = results.len();

    for i in 0..n {
        let a = candidates[i];
        for j in (i + 1)..n {
            let b = candidates[j];
            for k in (j + 1)..n {
                let c = candidates[k];

                // 按 y 排序，top2 → TL/TR，bottom1 → BL
                let mut trio = [a, b, c];
                trio.sort_by(|p, q| {
                    p.center_y
                        .partial_cmp(&q.center_y)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

                // TL: top2 中 x 较小者, TR: top2 中 x 较大者
                let (tl_cand, tr_cand) = if trio[0].center_x < trio[1].center_x {
                    (trio[0], trio[1])
                } else {
                    (trio[1], trio[0])
                };
                let bl_cand = trio[2];

                // --- 约束 (a): TL 与 TR 水平对齐 ---
                let dx = tr_cand.center_x - tl_cand.center_x;
                if dx < 1.0 {
                    continue;
                }
                // v0.12: TR 必须与 TL 在同一水平线（同行），容差 5 模块。
                // 原约束 dy > 0.25*dx 过于宽松（对于 2500px 帧允许 625px 的 y 偏移），
                // 导致帧中部假 finder 被误选为 TR，产生错误仿射变换。
                let dy = (tr_cand.center_y - tl_cand.center_y).abs();
                let avg_m_a = (tl_cand.module_size + tr_cand.module_size + bl_cand.module_size) / 3.0;
                if dy > avg_m_a * 5.0 {
                    continue;
                }

                // --- 约束 (b): BL 在 TL 下方 >50px，且 x 接近 ---
                let dy_bl = bl_cand.center_y - tl_cand.center_y;
                if dy_bl < 50.0 {
                    continue;
                }
                // v0.12: BL 必须与 TL 水平对齐（同列），容差 10 模块。
                // 原约束 |dx_bl| < dx * 0.5 过于宽松（对于 2500px 帧允许 1250px 偏移），
                // 导致数据区中部的假 finder 被误选为 BL，产生错误仿射变换，CRC 全部失败。
                // 修复为基于模块尺寸的绝对约束：finder pattern 本身宽 7 模块，
                // 10 模块容差足够覆盖定位抖动，又远小于帧宽/高。
                let dx_bl = bl_cand.center_x - tl_cand.center_x;
                let avg_m_b = (tl_cand.module_size + tr_cand.module_size + bl_cand.module_size) / 3.0;
                if dx_bl.abs() > avg_m_b * 10.0 {
                    continue;
                }

                // --- 计算边长 ---
                let d_top = (dx * dx + dy * dy).sqrt();
                let d_left = (dx_bl * dx_bl + dy_bl * dy_bl).sqrt();

                if d_top < 10.0 || d_left < 10.0 {
                    continue;
                }

                // --- 约束 (c): 矩形约束 (ratio 0.3–3.0，允许长方形帧) ---
                let ratio = d_top / d_left;
                if !(0.3..=3.0).contains(&ratio) {
                    continue;
                }

                // --- 约束 (d): 对角线 TR→BL ≈ √(d_top² + d_left²) (容差 20%) ---
                let dx_diag = bl_cand.center_x - tr_cand.center_x;
                let dy_diag = bl_cand.center_y - tr_cand.center_y;
                let d_diag = (dx_diag * dx_diag + dy_diag * dy_diag).sqrt();

                let expected_diag = (d_top * d_top + d_left * d_left).sqrt();
                if (d_diag - expected_diag).abs() > expected_diag * 0.20 {
                    continue;
                }

                // --- 约束 (e): module_size 相似（最大差值 < 0.15 × 均值） ---
                let avg_m =
                    (tl_cand.module_size + tr_cand.module_size + bl_cand.module_size) / 3.0;
                let diff_tl = (tl_cand.module_size - avg_m).abs();
                let diff_tr = (tr_cand.module_size - avg_m).abs();
                let diff_bl = (bl_cand.module_size - avg_m).abs();
                let max_diff = diff_tl.max(diff_tr).max(diff_bl);
                if max_diff > 0.15 * avg_m {
                    continue;
                }

                // --- 估算 modules_w 和 modules_h（支持长方形帧） ---
                let modules_w = (d_top / avg_m).round() as u32 + 7;
                let modules_h = (d_left / avg_m).round() as u32 + 7;
                if !(25..=1000).contains(&modules_w) || !(25..=1000).contains(&modules_h) {
                    continue;
                }
                let modules_per_side = modules_w.max(modules_h);

                // --- 通过所有约束，记录此三角组 ---
                let pixel_area = d_top * d_left;

                let tl_fp = FinderPattern {
                    center_x: tl_cand.center_x,
                    center_y: tl_cand.center_y,
                    module_size: tl_cand.module_size,
                };
                let tr_fp = FinderPattern {
                    center_x: tr_cand.center_x,
                    center_y: tr_cand.center_y,
                    module_size: tr_cand.module_size,
                };
                let bl_fp = FinderPattern {
                    center_x: bl_cand.center_x,
                    center_y: bl_cand.center_y,
                    module_size: bl_cand.module_size,
                };

                results.push((
                    FrameLocation {
                        tl: tl_fp,
                        tr: tr_fp,
                        bl: bl_fp,
                        modules_per_side,
                        modules_w,
                        modules_h,
                        module_pixels: tl_cand.module_size.round() as u32,
                    },
                    pixel_area,
                ));
                // v0.17: 早停防止大簇 O(n³) 膨胀
                if results.len() - start_count >= MAX_PER_CLUSTER {
                    return;
                }
            }
        }
    }
}

/// 在像素缓冲区中定位帧，同时返回诊断统计
///
/// 与 locate_frame 逻辑相同，但额外返回候选点和验证通过的计数。
/// 用于测试模式输出诊断日志。
pub fn locate_frame_with_diag(
    pixels: &[u8],
    width: i32,
    height: i32,
) -> Option<(FrameLocation, LocateDiagnostics)> {
    if width < 50 || height < 50 {
        return None;
    }

    let w = width as usize;
    let h = height as usize;

    let candidates = find_finder_candidates(pixels, w, h);
    let candidate_count = candidates.len();

    if candidates.len() < 3 {
        return None;
    }

    let verified: Vec<FinderCandidate> = candidates
        .into_iter()
        .filter(|fp| verify_finder_vertical(pixels, w, h, fp))
        .collect();
    let verified_count = verified.len();

    if verified.len() < 3 {
        return None;
    }

    let results = find_three_corners(&verified);

    results.into_iter().next().map(|loc| {
        (
            loc,
            LocateDiagnostics {
                candidates_found: candidate_count,
                candidates_verified: verified_count,
            },
        )
    })
}

/// 在像素缓冲区中定位帧，返回所有候选（含诊断统计）。
///
/// 返回所有通过几何约束的三角组，按像素面积降序排列。
/// 调用方应遍历候选逐个解码，由 CRC/payload 校验决定哪个是真帧。
pub fn locate_frame_all_with_diag(
    pixels: &[u8],
    width: i32,
    height: i32,
) -> Vec<(FrameLocation, LocateDiagnostics)> {
    if width < 50 || height < 50 {
        return Vec::new();
    }

    let w = width as usize;
    let h = height as usize;

    let candidates = find_finder_candidates(pixels, w, h);
    let candidate_count = candidates.len();

    if candidates.len() < 3 {
        return Vec::new();
    }

    let verified: Vec<FinderCandidate> = candidates
        .into_iter()
        .filter(|fp| verify_finder_vertical(pixels, w, h, fp))
        .collect();
    let verified_count = verified.len();

    if verified.len() < 3 {
        return Vec::new();
    }

    // 诊断：module_size 分布（限频输出，确认真帧 finder 是否被检测到）
    diag_ratelimited(&{
        let mut buckets = [0usize; 9]; // [0,1), [1,2), ..., [8,9), [9+)
        for v in &verified {
            let idx = (v.module_size as usize).min(8);
            buckets[idx] += 1;
        }
        format!(
            "finder ms分布: [0-1):{} [1-2):{} [2-3):{} [3-4):{} [4-5):{} [5-6):{} [6-7):{} [7-8):{} [8+):{}",
            buckets[0], buckets[1], buckets[2], buckets[3],
            buckets[4], buckets[5], buckets[6], buckets[7], buckets[8],
        )
    });

    let diag = LocateDiagnostics {
        candidates_found: candidate_count,
        candidates_verified: verified_count,
    };

    find_three_corners(&verified)
        .into_iter()
        .map(|loc| (loc, diag.clone()))
        .collect()
}

/// 在像素缓冲区中定位帧，返回所有候选。
pub fn locate_frame_all(
    pixels: &[u8],
    width: i32,
    height: i32,
) -> Vec<FrameLocation> {
    if width < 50 || height < 50 {
        return Vec::new();
    }

    let w = width as usize;
    let h = height as usize;

    let candidates = find_finder_candidates(pixels, w, h);
    if candidates.len() < 3 {
        return Vec::new();
    }

    let verified: Vec<FinderCandidate> = candidates
        .into_iter()
        .filter(|fp| verify_finder_vertical(pixels, w, h, fp))
        .collect();

    if verified.len() < 3 {
        return Vec::new();
    }

    find_three_corners(&verified)
}
