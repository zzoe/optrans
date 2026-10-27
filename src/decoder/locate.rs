//! 帧定位模块
//!
//! 通过 Finder Pattern 检测定位屏幕截图中帧的位置。
//!
//! v0.5 新增：`locate_by_corners` — 从屏幕三个已知角落直接定位 finder pattern，
//! 不再扫描全屏。原始 `locate_frame` 保留作为降级方案。

use crate::common::types::{FinderPattern, FrameLocation};

/// 定位候选上限（按像素面积降序截断）。到达上限说明候选被截断，
/// 调用方诊断日志据此判断真帧是否可能被挤出列表。
pub(crate) const MAX_CANDIDATES: usize = 100;

/// locate_frame 的诊断统计信息
#[derive(Debug, Clone)]
pub struct LocateDiagnostics {
    /// 水平扫描找到的候选点数量
    pub candidates_found: usize,
    /// 垂直验证通过的候选点数量
    pub candidates_verified: usize,
    /// 验证通过候选的 module_size 最小值（无候选时 0.0）
    pub ms_min: f64,
    /// 验证通过候选的 module_size 最大值（无候选时 0.0）
    pub ms_max: f64,
    /// module_size 聚类后的簇数。真帧三 finder 若被拆进不同簇，
    /// 三角搜索永远找不到组合（三角组=0），此字段用于诊断该场景。
    pub ms_clusters: usize,
}

impl LocateDiagnostics {
    fn new(found: usize, verified: usize, verified_cands: &[FinderCandidate]) -> Self {
        let (ms_min, ms_max, ms_clusters) = ms_stats(verified_cands);
        Self {
            candidates_found: found,
            candidates_verified: verified,
            ms_min,
            ms_max,
            ms_clusters,
        }
    }
}

/// 验证通过候选的 ms 散布与聚类簇数（诊断用）
fn ms_stats(verified: &[FinderCandidate]) -> (f64, f64, usize) {
    if verified.is_empty() {
        return (0.0, 0.0, 0);
    }
    let mut ms_min = f64::INFINITY;
    let mut ms_max = f64::NEG_INFINITY;
    for v in verified {
        ms_min = ms_min.min(v.module_size);
        ms_max = ms_max.max(v.module_size);
    }
    (ms_min, ms_max, cluster_by_module_size(verified).len())
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

    // 从角落 finder 位置推算模块尺寸（水平/垂直分开，支持非均匀缩放）
    let avg_module_size = (tl.2 + tr.2 + bl.2) / 3.0;
    let avg_v_module_size = (tl.3 + tr.3 + bl.3) / 3.0;

    // modules_w: TL→TR 的水平距离 / module_size + 7 (finder 自身宽度)
    let modules_w = ((tr.0 - tl.0) / avg_module_size).round() as u32 + 7;
    // modules_h: TL→BL 的垂直距离 / 垂直 module_size + 7
    let modules_h = ((bl.1 - tl.1) / avg_v_module_size).round() as u32 + 7;

    // 合理性检查
    if !(25..=1000).contains(&modules_w) || !(25..=1000).contains(&modules_h) {
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
/// `Some((center_x, center_y, module_size, v_module_size, v_module_size_alt))`
/// 如果找到，否则 `None`。`module_size` 为水平方向模块尺寸；
/// `v_module_size` / `v_module_size_alt` 为垂直方向模块尺寸的两种估计
/// （非整数缩放下单一估计有量化偏差，两种估计生成备选候选由 CRC 裁决）。
fn find_finder_at_corner(
    pixels: &[u8],
    w: usize,
    h: usize,
    corner_x: i32,
    corner_y: i32,
    m: i32,
    search_radius: i32,
) -> Option<(f64, f64, f64, f64, f64)> {
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
                            // 垂直验证（同时测量垂直模块尺寸并精修 Y 中心）
                            if let Some((v_ms, v_ms_alt, center_y)) = verify_finder_vertical_at(
                                pixels, w, h,
                                center_x, scan_y as f64,
                            ) {
                                return Some((center_x, center_y, module_size, v_ms, v_ms_alt));
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
///
/// 返回 `Some((垂直模块尺寸, 备选垂直模块尺寸, 亚像素 Y 中心))`
/// （垂直模块尺寸用于非均匀缩放下估算 modules_h），否则 `None`。
/// `亚像素 Y 中心` 用于精修候选的 center_y（见 check_vertical_pattern_sized）。
fn verify_finder_vertical_at(
    pixels: &[u8],
    w: usize,
    h: usize,
    cx: f64,
    cy: f64,
) -> Option<(f64, f64, f64)> {
    let cx_i = cx as i32;
    let cy_i = cy as i32;

    if cx_i < 4 || cx_i >= w as i32 - 4 {
        return None;
    }

    // 以候选点为中心，上下各扩展 20 像素（覆盖 finder pattern 7 模块高度）
    let y_start = (cy_i - 20).max(0) as usize;
    let y_end = (cy_i + 20).min(h as i32 - 1) as usize;

    // 在 cx 附近 ±4 像素（约 1 模块）范围内扫描多列
    for dx in [-4i32, -2, 0, 2, 4] {
        let x_idx = ((cx_i + dx) as usize) * 4;
        if let Some((v_ms, v_ms_alt, center_y)) =
            check_vertical_pattern_sized(pixels, w, x_idx, y_start, y_end)
        {
            return Some((v_ms, v_ms_alt, center_y));
        }
    }

    None
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

    if candidates.len() < 3 {
        return None;
    }

    // 第二步：垂直方向交叉验证每个候选
    let verified: Vec<FinderCandidate> = candidates
        .into_iter()
        .filter_map(|mut fp| {
            verify_finder_vertical(pixels, w, h, &fp).map(|(v_ms, v_ms_alt, center_y)| {
                fp.v_module_size = v_ms;
                fp.v_module_size_alt = v_ms_alt;
                fp.center_y = center_y;
                fp
            })
        })
        .collect();

    if verified.len() < 3 {
        return None;
    }

    // 第三步：从验证过的候选中找到所有构成直角三角形的 finder pattern
    // 返回按像素面积降序排列的候选列表，取第一个（面积最大）
    let results = find_three_corners(&verified);

    results.into_iter().next()
}

/// Finder Pattern 候选点（水平扫描结果）
#[derive(Debug, Clone)]
struct FinderCandidate {
    center_x: f64,
    center_y: f64,
    /// 水平方向模块尺寸（由水平 1:1:3:1:1 段估计）
    module_size: f64,
    /// 垂直方向模块尺寸（由垂直验证测量；水平扫描阶段暂等于 module_size，
    /// 垂直验证通过后被真实测量值替换，用于非均匀缩放下估算 modules_h）
    v_module_size: f64,
    /// 备选垂直模块尺寸（另一种估计；非整数缩放下单一估计有量化偏差，
    /// 备选值生成第二候选，由格式信息 CRC 裁决）
    v_module_size_alt: f64,
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
                            v_module_size: module_size,
                            v_module_size_alt: module_size,
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
                v_module_size: module_size,
                v_module_size_alt: module_size,
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
///
/// 返回 `Some((垂直模块尺寸, 备选垂直模块尺寸, 亚像素 Y 中心))` 如果验证通过，否则 `None`。
/// Y 中心为游程边界推算的亚像素值，用于替换量化在扫描行格子上的 center_y。
fn verify_finder_vertical(
    pixels: &[u8],
    w: usize,
    h: usize,
    fp: &FinderCandidate,
) -> Option<(f64, f64, f64)> {
    let cx = fp.center_x as i32;
    let cy = fp.center_y as i32;
    let ms = fp.module_size as i32;

    if cx < ms || cx >= w as i32 - ms {
        return None;
    }

    // 以候选点为中心，上下各扩展 5 个模块
    let half_span = (fp.module_size * 5.0) as i32;
    let y_start = (cy - half_span).max(0) as usize;
    let y_end = (cy + half_span).min(h as i32 - 1) as usize;

    if y_end <= y_start {
        return None;
    }

    // 优先尝试垂直扫描：滑动窗口对 finder 与 margin 黑边合并天然鲁棒
    // （端段被拉长但中间 3 段仍保持 1:3:1，mid/5 估计不受污染）。
    // 非整数缩放下必须测量真实垂直模块尺寸，不能轻易回退水平值，
    // 否则 modules_h 估算偏差会导致数据采样网格错位。
    //
    // 在中心 x 附近 ±1 模块范围内的若干列上尝试垂直扫描
    // finder 中心列全黑，必须扫描偏移约 1 模块的列（col 2 或 col 4）
    let step = (ms / 2).max(1);
    for dx in (-ms..=ms).step_by(step as usize) {
        let x_idx = ((cx + dx) as usize) * 4;
        if let Some((v_ms, v_ms_alt, center_y)) =
            check_vertical_pattern_sized(pixels, w, x_idx, y_start, y_end)
        {
            return Some((v_ms, v_ms_alt, center_y));
        }
    }

    // v0.15: 扫描失败时的降级——检测 finder 下方是否存在大量黑边
    // （FRAME_MARGIN_PX > 0 特征）。黑边可能截断扫描窗口内的 5 段 pattern，
    // 此时垂直尺寸无法测量，回退水平尺寸。
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
                // 黑边截断场景无法亚像素精修，保留扫描行的 center_y
                return Some((fp.module_size, fp.module_size, fp.center_y));
            }
        }
    }

    None
}

/// 在指定列上做垂直 1:1:3:1:1 pattern 扫描，返回垂直模块尺寸与亚像素 Y 中心
///
/// 返回 `(mid_ms, total_ms, center_y)`：
/// - `mid_ms`: 内部 3 段总和 / 5（与水平方向 check_finder_ratio 一致）
/// - `total_ms`: 全部 5 段总和 / 7（跨 7 模块平均，非整数缩放下量化误差更小）
/// - `center_y`: finder 中心的亚像素 Y（由游程边界推算，与水平 center_x 同算法）
///
/// 非整数缩放下单一估计可能量化偏差 ~4%（如 3.86→4.0），导致 modules_h
/// 估算错 10+ 模块；两种估计生成备选候选，由格式信息 CRC 裁决。
///
/// 亚像素 Y 中心很关键：center_y 若取扫描行号，量化在扫描步长（3px）格子上，
/// 数据采样网格按行累积偏移（顶端 ±1px 偏差 → 末端可达数像素），导致
/// 小模块（缩放后 <3.5px）场景 RS 纠错超容整帧丢弃。
fn check_vertical_pattern_sized(
    pixels: &[u8],
    w: usize,
    x_idx: usize,
    y_start: usize,
    y_end: usize,
) -> Option<(f64, f64, f64)> {
    let mut state_count = [0i32; 5];
    let mut state_idx = 0usize;

    let init_idx = y_start * w * 4 + x_idx;
    if init_idx + 2 >= pixels.len() {
        return None;
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
                    // y 是 pattern 之后的第一个像素，pattern 最后像素在 y-1
                    let center_y =
                        (y as f64) - (state_count[4] as f64
                            + state_count[3] as f64
                            + state_count[2] as f64 / 2.0);
                    let (ms, ms_alt) = sized_estimates(&state_count);
                    return Some((ms, ms_alt, center_y));
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

    if state_idx == 4 && check_finder_ratio(&state_count) {
        // 循环到 y_end 结束：pattern 之后第一个像素为 y_end + 1
        let center_y = ((y_end + 1) as f64)
            - (state_count[4] as f64 + state_count[3] as f64 + state_count[2] as f64 / 2.0);
        let (ms, ms_alt) = sized_estimates(&state_count);
        Some((ms, ms_alt, center_y))
    } else {
        None
    }
}

/// 从 5 段状态计数计算两种垂直模块尺寸估计
#[inline]
fn sized_estimates(state_count: &[i32; 5]) -> (f64, f64) {
    let mid_sum = (state_count[1] + state_count[2] + state_count[3]) as f64;
    let total_sum = state_count.iter().sum::<i32>() as f64;
    (mid_sum / 5.0, total_sum / 7.0)
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
        let mut sum_vms = candidates[i].v_module_size;
        let mut sum_vms_alt = candidates[i].v_module_size_alt;
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
                sum_vms += candidates[j].v_module_size;
                sum_vms_alt += candidates[j].v_module_size_alt;
                count += 1;
                used[j] = true;
            }
        }

        merged.push(FinderCandidate {
            center_x: sum_x / count as f64,
            center_y: sum_y / count as f64,
            module_size: sum_ms / count as f64,
            v_module_size: sum_vms / count as f64,
            v_module_size_alt: sum_vms_alt / count as f64,
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

    // v0.26: 全候选 O(n³) 搜索，不做 ms 预聚类。
    // 旧实现按 module_size 聚类后仅在簇内搜三角形：远程压缩/模糊让三个
    // 真 finder 的 ms 测量散布可达 ±30%（ms=2.0~8.0 混杂噪声的实测场景），
    // 真帧三 finder 被拆进不同簇 → 三角组=0，永远定位失败。
    // 约束 (e) 本身已做 ms 一致性过滤，预聚类纯属冗余且有害。
    // 真实场景 verified 候选 ≤ 数百，C(n,3) 的廉价约束检查代价可接受；
    // 极端噪声场景以迭代预算兜底（见 search_triangles）。
    let refs: Vec<&FinderCandidate> = candidates.iter().collect();
    let mut all_results: Vec<(FrameLocation, f64)> = Vec::new();
    search_triangles(&refs, &mut all_results);

    // 排序：像素面积降序（真帧 finder 间距最大，三角形面积几乎总是最大），
    // 同面积时按 TL 距屏幕左上角距离升序（仅作 tiebreak）。
    //
    // v0.26 修复两处排序缺陷（远程桌面窗口内图案收不到的根因）：
    // - 删除"帧型优先"硬编码范围（550-710×300-460 是 FRAME_MARGIN_PX=120
    //   时代的 580×340），当前默认参数的帧是 502×310，从不匹配，纯噪声；
    // - 删除按"距期望位置(34,34)"主排序——图案在远程桌面窗口内偏移时
    //   真帧 TL 不在屏幕角落，会被挤出截断前的候选列表，永远解不出 Meta。
    // 真假帧最终由解码阶段 CRC + payload 校验裁决，这里只保证真帧不被丢弃。
    all_results.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let da = a.0.tl.center_x.abs() + a.0.tl.center_y.abs();
                let db = b.0.tl.center_x.abs() + b.0.tl.center_y.abs();
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    all_results.truncate(MAX_CANDIDATES);
    all_results.into_iter().map(|(loc, _)| loc).collect()
}

/// 按 module_size 聚类：排序后，候选与簇均值差距 ≤ max(0.5px, 35%) 归为同组
///
/// v0.26 修复：原阈值为固定 ±0.5px，是 1:1 清晰缩放时代的取值。远程桌面
/// 压缩/模糊后各 finder 的 ms 测量可差 15~20%（ms≈5.2 时 0.5px 仅约 10%），
/// 真帧三 finder 被拆进不同簇，三角搜索无组合可试 → 三角组=0。
/// 改为相对容差（簇均值的 35%），ms 3.8/4.6/5.4 的真帧三 finder 归入同簇。
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
    let mut sum_ms = sorted[0].module_size;
    let mut count = 1usize;

    for &cand in &sorted[1..] {
        let mean = sum_ms / count as f64;
        if (cand.module_size - mean).abs() <= (0.35 * mean).max(0.5) {
            current.push(cand);
            sum_ms += cand.module_size;
            count += 1;
        } else {
            clusters.push(current);
            current = vec![cand];
            sum_ms = cand.module_size;
            count = 1;
        }
    }
    clusters.push(current);
    clusters
}

/// 对候选组做 O(n³) 三角搜索，结果追加到 results。
///
/// v0.26: 迭代预算代替旧的"结果数达 200 即 return"早停。旧实现在噪声
/// 三角形先被枚举到时提前退出，排在后面的真实三角形被连带丢弃（三角组=0
/// 的隐性根因之一）。预算只限制计算量，不限制结果收集；真实场景
/// verified ≤ 数百时 C(n,3) ≪ 预算，只有极端噪声才触顶（触顶保留已收集
/// 结果，由排序+截断保证真帧不被丢）。
fn search_triangles(
    candidates: &[&FinderCandidate],
    results: &mut Vec<(FrameLocation, f64)>,
) {
    let n = candidates.len();
    if n < 3 {
        return;
    }

    const MAX_ITERATIONS: u64 = 25_000_000;
    let mut iterations = 0u64;

    'budget: for i in 0..n {
        let a = candidates[i];
        for j in (i + 1)..n {
            let b = candidates[j];
            for k in (j + 1)..n {
                iterations += 1;
                if iterations > MAX_ITERATIONS {
                    log::warn!("三角搜索超出迭代预算（候选过多），提前截断");
                    break 'budget;
                }
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

                // --- 约束 (e): module_size 相似（最大差值 < 0.45 × 均值） ---
                // v0.26: 0.15→0.45。远程压缩/模糊后三 finder 的 ms 测量散布
                // 实测可达 ±30%（ms=2.0~8.0 混杂噪声场景），过严的容差会拒绝
                // 真三角形（三角组=0）。真假帧由 CRC + payload 校验裁决，
                // 此约束只排除 ms 量级完全不同的噪声组合。
                let avg_m =
                    (tl_cand.module_size + tr_cand.module_size + bl_cand.module_size) / 3.0;
                let diff_tl = (tl_cand.module_size - avg_m).abs();
                let diff_tr = (tr_cand.module_size - avg_m).abs();
                let diff_bl = (bl_cand.module_size - avg_m).abs();
                let max_diff = diff_tl.max(diff_tr).max(diff_bl);
                if max_diff > 0.45 * avg_m {
                    continue;
                }

                // --- 估算 modules_w 和 modules_h（支持长方形帧 / 非均匀缩放） ---
                // 水平跨度用水平模块尺寸，垂直跨度用垂直模块尺寸
                let modules_w = (d_top / avg_m).round() as u32 + 7;
                if !(25..=1000).contains(&modules_w) {
                    continue;
                }

                // --- 通过所有约束，记录此三角组 ---
                let pixel_area = d_top * d_left;

                // 非整数缩放下垂直模块尺寸的单一估计有量化偏差（如 3.86→4.0），
                // 会导致 modules_h 估算错 10+ 模块。两种估计各生成一个候选，
                // 由格式信息 CRC 裁决哪个正确。
                let avg_vm = (tl_cand.v_module_size
                    + tr_cand.v_module_size
                    + bl_cand.v_module_size)
                    / 3.0;
                let avg_vm_alt = (tl_cand.v_module_size_alt
                    + tr_cand.v_module_size_alt
                    + bl_cand.v_module_size_alt)
                    / 3.0;
                let vm_candidates: [f64; 2] = [avg_vm, avg_vm_alt];

                for (vi, vm) in vm_candidates.iter().enumerate() {
                    // 备选估计与主估计几乎相同时跳过（避免重复候选）
                    if vi == 1 && (avg_vm_alt - avg_vm).abs() < 0.05 {
                        break;
                    }
                    let modules_h = (d_left / vm).round() as u32 + 7;
                    if !(25..=1000).contains(&modules_h) {
                        continue;
                    }
                    let modules_per_side = modules_w.max(modules_h);

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
        .filter_map(|mut fp| {
            verify_finder_vertical(pixels, w, h, &fp).map(|(v_ms, v_ms_alt, center_y)| {
                fp.v_module_size = v_ms;
                fp.v_module_size_alt = v_ms_alt;
                fp.center_y = center_y;
                fp
            })
        })
        .collect();
    let verified_count = verified.len();

    if verified.len() < 3 {
        return None;
    }

    let results = find_three_corners(&verified);

    results.into_iter().next().map(|loc| {
        (
            loc,
            LocateDiagnostics::new(candidate_count, verified_count, &verified),
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
    locate_frame_all_with_stats(pixels, width, height).0
}

/// 定位帧并始终返回阶段统计（即使无候选）——诊断失败原因用：
/// 候选=0 → 图案/对比度问题；候选>0 但验证=0 → 垂直结构失真；
/// 验证>0 但三角组=0 → 三角约束被噪声/偏移破坏。
pub fn locate_frame_all_with_stats(
    pixels: &[u8],
    width: i32,
    height: i32,
) -> (Vec<(FrameLocation, LocateDiagnostics)>, LocateDiagnostics) {
    let empty_stats = |found: usize, verified: usize| {
        LocateDiagnostics::new(found, verified, &[])
    };

    if width < 50 || height < 50 {
        return (Vec::new(), empty_stats(0, 0));
    }

    let w = width as usize;
    let h = height as usize;

    let candidates = find_finder_candidates(pixels, w, h);
    let candidate_count = candidates.len();

    if candidates.len() < 3 {
        return (Vec::new(), empty_stats(candidate_count, 0));
    }

    let verified: Vec<FinderCandidate> = candidates
        .into_iter()
        .filter_map(|mut fp| {
            verify_finder_vertical(pixels, w, h, &fp).map(|(v_ms, v_ms_alt, center_y)| {
                fp.v_module_size = v_ms;
                fp.v_module_size_alt = v_ms_alt;
                fp.center_y = center_y;
                fp
            })
        })
        .collect();
    let verified_count = verified.len();

    if verified.len() < 3 {
        return (Vec::new(), empty_stats(candidate_count, verified_count));
    }

    let diag = LocateDiagnostics::new(candidate_count, verified_count, &verified);

    let results = find_three_corners(&verified)
        .into_iter()
        .map(|loc| (loc, diag.clone()))
        .collect();
    (results, diag)
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
        .filter_map(|mut fp| {
            verify_finder_vertical(pixels, w, h, &fp).map(|(v_ms, v_ms_alt, center_y)| {
                fp.v_module_size = v_ms;
                fp.v_module_size_alt = v_ms_alt;
                fp.center_y = center_y;
                fp
            })
        })
        .collect();

    if verified.len() < 3 {
        return Vec::new();
    }

    find_three_corners(&verified)
}
