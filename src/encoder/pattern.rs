//! Finder pattern 生成与格式信息编码
//!
//! v0.5: is_finder_module 和 is_format_module 支持长方形帧（独立宽高参数）。

/// 生成 7×7 的 Finder Pattern 模块矩阵
/// 1=黑色模块, 0=白色模块
pub fn generate_finder_pattern() -> [[u8; 7]; 7] {
    // 7×7 全 1 矩阵（外边框全黑）
    let mut pattern = [[1u8; 7]; 7];
    // 内部 5×5 区域挖空
    for r in 1..6 {
        for c in 1..6 {
            pattern[r][c] = 0;
        }
    }
    // 中心 3×3 区域填黑
    for r in 2..5 {
        for c in 2..5 {
            pattern[r][c] = 1;
        }
    }
    pattern
}

/// 判断模块坐标 (col, row) 是否位于 finder pattern 区域内
///
/// - n_w: 水平方向模块数
/// - n_h: 垂直方向模块数
///
/// Finder 位置：
/// - TL: (0, 0)
/// - TR: (n_w - 7, 0)
/// - BL: (0, n_h - 7)
///
/// M8: 尺寸小于 finder 时用 saturating_sub 防无符号下溢
/// （release 模式下溢不 panic 但产生错误判定）。
pub fn is_finder_module(col: u32, row: u32, n_w: u32, n_h: u32) -> bool {
    let s = 7; // finder pattern 尺寸
    if n_w < s || n_h < s {
        // 尺寸不足以容纳 finder，任何坐标都不在 finder 内
        return false;
    }
    // 左上角
    if col < s && row < s {
        return true;
    }
    // 右上角
    if col >= n_w - s && row < s {
        return true;
    }
    // 左下角
    if col < s && row >= n_h - s {
        return true;
    }
    false
}

/// 判断模块坐标是否位于格式信息区
///
/// 格式信息区：模块行 n_h-2 和 n_h-1（帧底部最后两行）
/// 位于两个 finder pattern 之间（列 7..n_w-8）。
/// v0.24: 从顶部移到帧末尾——底部最后渲染，格式信息完整=全帧已渲染（防撕裂）
///
/// M8: 尺寸不足时用 saturating_sub 防无符号下溢。
pub fn is_format_module(col: u32, row: u32, n_w: u32, n_h: u32) -> bool {
    if n_h < 2 || n_w < 14 {
        return false;
    }
    if row < n_h - 2 {
        return false;
    }
    let s = 7; // finder pattern 尺寸
    if col >= s && col < n_w - s {
        return true;
    }
    false
}
