//! 模块提取模块
//!
//! 通过 Finder Pattern 中心建立仿射变换，从截屏像素中采样每个模块的值。
//! 包含格式信息区模块提取和数据矩阵模块提取。
//! v0.5: 支持长方形帧 / v0.21: 统一单像素采样（2/4/8/16 级全支持）

use crate::common::types::{EncodingMode, FrameLocation};

/// 二值模式采样半径（3×3，radius=1）
const SAMPLE_RADIUS_BINARY: i32 = 1;

// ═══════════════════════════════════════════════════════════════════
//  仿射变换预计算（v0.19.4 优化）
// ═══════════════════════════════════════════════════════════════════
//
// Finder pattern 定位后，从模块坐标 → 像素坐标的仿射变换参数是常量。
// 预计算一次，避免每个模块都重复 10+ 次 f64 运算。
// 对于 640×400 帧的 25 万+ 数据模块，节省约 250 万次浮点运算。

/// 预计算的仿射变换参数：从模块坐标 (col, row) → 像素坐标 (x, y)
struct AffineTransform {
    dst_dx_x: f64,
    dst_dx_y: f64,
    dst_dy_x: f64,
    dst_dy_y: f64,
    /// base_x = dst_tl_x - dst_dx_x*3.5 - dst_dy_x*3.5
    base_x: f64,
    /// base_y = dst_tl_y - dst_dx_y*3.5 - dst_dy_y*3.5
    base_y: f64,
}

impl AffineTransform {
    /// v0.28: 模块尺寸从三个 finder 角的实际跨度推算（支持分数级缩放）。
    ///
    /// 捕获端可能对发送端图案做非整数缩放（典型场景：Horizon 窗口把远程桌面
    /// 缩放到客户区，如 4px 模块 → 3.2px）。旧实现强制整数 module_pixels，
    /// 采样网格跨整帧累积误差（600+ 模块 × 0.2px ≈ 120px），格式信息区必然
    /// 采到空白区导致解码失败。
    ///
    /// modules_w/h 由 locate 的几何约束确定为整数（可靠），跨度 ÷ 模块数即得
    /// 真实（可能为分数的）模块尺寸；倾斜分量仍由 finder 差值推算。
    fn from_location(loc: &FrameLocation, module_pixels: u32) -> Self {
        let mp = module_pixels as f64;
        let src_dx = (loc.modules_w as f64 - 7.0).max(1.0);
        let src_dy = (loc.modules_h as f64 - 7.0).max(1.0);

        // 主方向取 finder 实际跨度（分数级），异常时回退约定 module_pixels
        let mut dst_dx_x = (loc.tr.center_x - loc.tl.center_x) / src_dx;
        if !dst_dx_x.is_finite() || dst_dx_x <= 0.0 {
            dst_dx_x = mp;
        }
        let mut dst_dy_y = (loc.bl.center_y - loc.tl.center_y) / src_dy;
        if !dst_dy_y.is_finite() || dst_dy_y <= 0.0 {
            dst_dy_y = mp;
        }
        // 倾斜分量由 finder Y/X 差推算
        let dst_dx_y = (loc.tr.center_y - loc.tl.center_y) / src_dx;
        let dst_dy_x = (loc.bl.center_x - loc.tl.center_x) / src_dy;

        // 预计算偏移，避免每个模块都做 (mx - 3.5) 运算
        let base_x = loc.tl.center_x - dst_dx_x * 3.5 - dst_dy_x * 3.5;
        let base_y = loc.tl.center_y - dst_dx_y * 3.5 - dst_dy_y * 3.5;

        AffineTransform { dst_dx_x, dst_dx_y, dst_dy_x, dst_dy_y, base_x, base_y }
    }

    /// 模块中心 → 像素坐标（仅 4 次乘 + 4 次加）
    #[inline]
    fn to_pixel(&self, module_col: usize, module_row: usize) -> (f64, f64) {
        let mx = module_col as f64 + 0.5;
        let my = module_row as f64 + 0.5;
        let px = self.base_x + self.dst_dx_x * mx + self.dst_dy_x * my;
        let py = self.base_y + self.dst_dx_y * mx + self.dst_dy_y * my;
        (px, py)
    }
}

/// 从格式信息区提取所有模块的比特值
///
/// 格式信息区：模块行 0 和 行 1，列 7..n_w-8
/// 返回模块比特序列，1=黑色, 0=白色
/// 格式信息区始终使用二值采样。
pub fn extract_format_modules(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
) -> Vec<u8> {
    extract_format_modules_with_radius(pixels, img_width, img_height, loc, SAMPLE_RADIUS_BINARY)
}

/// 带自定义采样半径的格式信息提取（用于 CRC 失败时降级重试）
///
/// - radius=1（默认 3×3）：常规路径
/// - radius=0（单点）：定位精准时最稳，避免采到相邻模块
/// - radius=2（5×5）：抗随机噪声，定位有偏时可能采到邻居
///
/// 大文件场景下格式信息 CRC 失败率高，主因是 finder 中心定位的亚像素抖动导致
/// 仿射变换远端模块采样偏移。降级重试可在不改变定位的前提下提升解码成功率。
pub fn extract_format_modules_with_radius(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
    radius: i32,
) -> Vec<u8> {
    let n_w = loc.modules_w as usize;
    let s = 7usize; // finder pattern 尺寸
    // L5: 小尺寸防御——帧不足以容纳两个 finder + 格式区时无格式信息可提取
    // （n_w < 14 或 n_h < 2 时 n_w - s / n_h - 2 会下溢）
    if n_w < 2 * s || loc.modules_h < 2 {
        return Vec::new();
    }
    let start_col = s;
    let end_col = n_w - s;

    let affine = AffineTransform::from_location(loc, loc.module_pixels);

    let n_h = loc.modules_h as usize;

    let mut bits = Vec::with_capacity((end_col - start_col) * 2);

    // v0.24: 格式信息从帧顶部移到帧底部（n_h-2, n_h-1）
    // 底部是最后渲染的行，格式信息完整 = 全帧已渲染 → 防撕裂
    for row in (n_h - 2)..n_h {
        for col in start_col..end_col {
            let (px, py) = affine.to_pixel(col, row);
            let module_val = if radius <= 0 {
                sample_pixel_point(pixels, img_width, img_height, px, py)
            } else {
                sample_pixel_region_mean(pixels, img_width, img_height, px, py, radius)
            };
            bits.push(module_val);
        }
    }

    bits
}

/// 单点采样：直接取模块中心像素亮度，阈值 128（用于降级重试 radius=0）
fn sample_pixel_point(pixels: &[u8], img_width: i32, img_height: i32, px: f64, py: f64) -> u8 {
    let w = img_width as usize;
    let cx = px.round() as i32;
    let cy = py.round() as i32;
    if cx < 0 || cx >= img_width || cy < 0 || cy >= img_height {
        return 0;
    }
    let idx = (cy as usize * w + cx as usize) * 4;
    if idx + 2 >= pixels.len() {
        return 0;
    }
    let r = pixels[idx] as u32;
    let g = pixels[idx + 1] as u32;
    let b = pixels[idx + 2] as u32;
    let lum = ((r * 299 + g * 587 + b * 114) / 1000) as u8;
    if lum < 128 { 0 } else { 1 }
}

/// 从数据矩阵提取所有模块的值（二值或灰度取决于 encoding_mode）
///
/// 按行优先遍历所有非保留模块，采样其值。
/// 二值模式：返回 0/1 bits
/// 灰度模式：返回原始灰度值 (0-255)
pub fn extract_data_modules(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
    payload_bits: usize,
    encoding_mode: &EncodingMode,
) -> Vec<u8> {
    let n_w = loc.modules_w as usize;
    let n_h = loc.modules_h as usize;
    let s = 7usize;

    // L5: 小尺寸防御——帧不足以容纳 finder 时数据区提取无意义
    // （is_in_finder 内 n_w - s / n_h - s 在小尺寸下下溢）
    if n_w <= s || n_h <= s {
        return Vec::new();
    }

    let affine = AffineTransform::from_location(loc, loc.module_pixels);
    let module_size = mean_module_size(loc);

    let mut values = Vec::with_capacity(payload_bits);

    for row in 0..n_h {
        for col in 0..n_w {
            // 跳过 finder pattern 区域
            if is_in_finder(col, row, n_w, n_h) {
                continue;
            }
            // 跳过格式信息区（v0.24: 位于帧底部最后两行）
            if row >= n_h - 2 && col >= s && col < n_w - s {
                continue;
            }

            if values.len() >= payload_bits {
                break;
            }

            let module_val = sample_module_with_affine(
                pixels,
                img_width,
                img_height,
                &affine,
                col,
                row,
                encoding_mode,
                module_size,
            );
            values.push(module_val);
        }
        if values.len() >= payload_bits {
            break;
        }
    }

    values
}

/// 判断坐标是否在 finder pattern 区域内
fn is_in_finder(col: usize, row: usize, n_w: usize, n_h: usize) -> bool {
    let s = 7;
    // TL
    if col < s && row < s {
        return true;
    }
    // TR
    if col >= n_w - s && row < s {
        return true;
    }
    // BL
    if col < s && row >= n_h - s {
        return true;
    }
    false
}

/// 采样单个模块的值（根据编码模式选择采样策略）
///
/// - Binary 模式：3×3 均值 + 阈值 128 → 0 或 1
/// - Gray 模式：模块内面积均值 → 原始灰度值 (0-255)
pub fn sample_module(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
    module_col: usize,
    module_row: usize,
    encoding_mode: &EncodingMode,
) -> u8 {
    let affine = AffineTransform::from_location(loc, loc.module_pixels);
    let module_size = mean_module_size(loc);
    sample_module_with_affine(
        pixels,
        img_width,
        img_height,
        &affine,
        module_col,
        module_row,
        encoding_mode,
        module_size,
    )
}

/// 采样窗口可用的模块尺寸：取水平/垂直两个方向的较小值
///
/// 非均匀缩放下（如垂直 0.9625）两轴模块尺寸不同，finder 的
/// 1:1:3:1:1 游程只测得到水平方向；若按水平值选窗口，垂直方向会越界
/// 压到相邻模块。由 finder 跨度反推两轴尺寸后取小值。
pub(crate) fn mean_module_size(loc: &FrameLocation) -> f64 {
    if loc.modules_w > 7 && loc.modules_h > 7 {
        let sx = (loc.tr.center_x - loc.tl.center_x).abs() / (loc.modules_w as f64 - 7.0);
        let sy = (loc.bl.center_y - loc.tl.center_y).abs() / (loc.modules_h as f64 - 7.0);
        if sx > 0.5 && sy > 0.5 {
            return sx.min(sy);
        }
    }
    loc.tl.module_size
}

/// 灰度采样的窗口边长（像素）
///
/// 灰度最窄级距只有 12（16 级 @ 179 动态范围），单像素采样把整条链路的
/// 像素噪声直接灌进判界：实测 16 级帧在第 12 个 RS 块就越过纠错上限
/// （19/255），而前面 11 块完全正确——典型的逐模块随机误判。
///
/// 窗口必须完全落在模块内，否则相邻模块的极值会污染均值（权重 1/n²，
/// 影响远大于噪声）。中心取整误差 0.5px + 半宽 n/2，要求 m ≥ n + 1：
/// - m ≥ 7px → 5×5（σ 降 5 倍）
/// - m ≥ 5px → 3×3（σ 降 3 倍）
/// - m ≥ 4px → 2×2（σ 降 2 倍，偶数窗口向右下偏半像素仍在模块内）
/// - m < 4px → 单点
pub(crate) fn gray_sample_window(module_size: f64) -> i32 {
    if module_size >= 7.0 {
        5
    } else if module_size >= 5.0 {
        3
    } else if module_size >= 4.0 {
        2
    } else {
        1
    }
}

/// 使用预计算的仿射变换采样单个模块（避免重复计算仿射参数）
#[inline]
fn sample_module_with_affine(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    affine: &AffineTransform,
    module_col: usize,
    module_row: usize,
    encoding_mode: &EncodingMode,
    module_size: f64,
) -> u8 {
    let (px, py) = affine.to_pixel(module_col, module_row);

    match encoding_mode {
        EncodingMode::Binary | EncodingMode::Auto => {
            sample_module_binary_at(pixels, img_width, img_height, px, py)
        }
        EncodingMode::Gray(_) => {
            let n = gray_sample_window(module_size);
            // M11: 2D 判界（镜像 sample_pixel_point）——负坐标经 wrapping 转 usize
            // 会绕回巨大正数，仅靠 idx 长度判断会读到远处像素（甚至越界返回 0）
            let cx = px.round() as i32;
            let cy = py.round() as i32;
            if cx < 0 || cx >= img_width || cy < 0 || cy >= img_height {
                return 0;
            }
            if n <= 1 {
                let w = img_width as usize;
                let idx = (cy as usize * w + cx as usize) * 4;
                if idx + 2 < pixels.len() {
                    let r = pixels[idx] as u32;
                    let g = pixels[idx + 1] as u32;
                    let b = pixels[idx + 2] as u32;
                    ((r * 299 + g * 587 + b * 114) / 1000) as u8
                } else {
                    0
                }
            } else {
                sample_pixel_region_luma(pixels, img_width, img_height, cx, cy, n)
            }
        }
    }
}

/// 在像素坐标处执行二值采样（3×3 均值）
fn sample_module_binary_at(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    px: f64,
    py: f64,
) -> u8 {
    sample_pixel_region_mean(pixels, img_width, img_height, px, py, SAMPLE_RADIUS_BINARY)
}

/// 在像素坐标周围采样区域取亮度均值（用于二值模式）
fn sample_pixel_region_mean(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    px: f64,
    py: f64,
    radius: i32,
) -> u8 {
    let avg = sample_pixel_region_luma(
        pixels,
        img_width,
        img_height,
        px.round() as i32,
        py.round() as i32,
        2 * radius + 1,
    );
    // 约定：白色像素 → 1，黑色像素 → 0（与编码器 gray_table=[0,255] 对应：digit 0→黑，digit 1→白）
    if avg < 128 { 0 } else { 1 }
}

/// 在像素坐标周围取 n×n 区域的亮度均值（越界像素忽略）
///
/// n 为偶数时窗口向右下偏半像素，仍落在模块内部。
fn sample_pixel_region_luma(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    cx: i32,
    cy: i32,
    n: i32,
) -> u8 {
    let w = img_width as usize;
    let lo = -(n - 1) / 2;
    let hi = lo + n - 1;

    let mut sum: u32 = 0;
    let mut count: u32 = 0;

    for dy in lo..=hi {
        for dx in lo..=hi {
            let sx = cx + dx;
            let sy = cy + dy;
            if sx >= 0 && sx < img_width && sy >= 0 && sy < img_height {
                let idx = (sy as usize * w + sx as usize) * 4;
                if idx + 2 >= pixels.len() {
                    continue;
                }
                let r = pixels[idx] as u32;
                let g = pixels[idx + 1] as u32;
                let b = pixels[idx + 2] as u32;
                sum += (r * 299 + g * 587 + b * 114) / 1000;
                count += 1;
            }
        }
    }

    if count == 0 {
        return 0;
    }
    (sum / count) as u8
}

// ─── 多级分类器 ───

/// 将亮度值分类为级别索引
///
/// # 参数
/// - `luminance`: 亮度值 (0-255)
/// - `thresholds`: 解码阈值表（levels-1 个值）
///
/// # 返回
/// 级别索引 0..levels-1
pub fn classify_gray(luminance: u8, thresholds: &[u8]) -> u8 {
    for (i, &th) in thresholds.iter().enumerate() {
        if luminance < th {
            return i as u8;
        }
    }
    thresholds.len() as u8
}

// ─── 灰度解码 ───

/// 通用灰度解码：将模块灰度值序列解码为字节
///
/// # 参数
/// - `gray_values`: 模块灰度值序列
/// - `gray_levels`: 灰度级数
/// - `thresholds`: 解码阈值表
/// - `original_len`: 原始数据长度（用于截断）
///
/// # 返回
/// 解码后的字节序列
pub fn decode_gray_n(
    gray_values: &[u8],
    gray_levels: u32,
    thresholds: &[u8],
    original_len: usize,
) -> Vec<u8> {
    let bits_per_module = match gray_levels {
        2 => 1,
        4 => 2,
        8 => 3,
        16 => 4,
        _ => 1,
    };
    decode_gray_direct(gray_values, bits_per_module, thresholds, original_len)
}

/// 直映射灰度解码
fn decode_gray_direct(
    gray_values: &[u8],
    bits_per_module: u32,
    thresholds: &[u8],
    original_len: usize,
) -> Vec<u8> {
    let total_bits = original_len * 8;
    let num_bytes = total_bits.div_ceil(8);
    let mut bytes = vec![0u8; num_bytes];
    let mask = (1u32 << bits_per_module) - 1;

    for (i, &gray) in gray_values.iter().enumerate() {
        let digit = classify_gray(gray, thresholds) as u32;
        let bit_offset = (i as u64) * (bits_per_module as u64);
        let byte_idx = (bit_offset / 8) as usize;
        let bit_pos = (bit_offset % 8) as u32;

        if byte_idx < bytes.len() {
            bytes[byte_idx] |= ((digit & mask) as u8) << bit_pos;
        }
        let remaining = bits_per_module as i32 - (8 - bit_pos as i32);
        if remaining > 0 && byte_idx + 1 < bytes.len() {
            bytes[byte_idx + 1] |=
                ((digit & mask) as u8) >> (8 - bit_pos);
        }
    }

    bytes.truncate(original_len);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_gray_4() {
        let thresholds: [u8; 3] = [42, 127, 212];
        assert_eq!(classify_gray(0, &thresholds), 0);
        assert_eq!(classify_gray(85, &thresholds), 1);
        assert_eq!(classify_gray(170, &thresholds), 2);
        assert_eq!(classify_gray(255, &thresholds), 3);
    }

    #[test]
    fn test_decode_gray_2_roundtrip() {
        let thresholds: [u8; 1] = [128];
        let data: Vec<u8> = vec![0b10101010, 0xFF, 0x00];
        let gray_values = encode_gray_direct_test(&data, 1, &[0, 255]);
        let decoded = decode_gray_direct(&gray_values, 1, &thresholds, data.len());
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_decode_gray_4_roundtrip() {
        let thresholds: [u8; 3] = [42, 127, 212];
        let data: Vec<u8> = vec![0xE4, 0x00, 0xFF];
        let gray_values = encode_gray_direct_test(&data, 2, &[0, 85, 170, 255]);
        let decoded = decode_gray_direct(&gray_values, 2, &thresholds, data.len());
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_decode_gray_8_roundtrip() {
        let thresholds: [u8; 7] = [18, 54, 91, 127, 164, 200, 237];
        let gray_8: [u8; 8] = [0, 36, 73, 109, 146, 182, 219, 255];
        let data: Vec<u8> = vec![0b111_110_10, 0b1_101_100_0, 0xFF];
        let gray_values = encode_gray_direct_test(&data, 3, &gray_8);
        let decoded = decode_gray_direct(&gray_values, 3, &thresholds, data.len());
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_decode_gray_16_roundtrip() {
        let thresholds: [u8; 15] = [
            8, 25, 42, 59, 76, 93, 110, 128, 144, 161, 178, 195, 212, 229, 246,
        ];
        let gray_16: [u8; 16] = [
            0, 17, 34, 51, 68, 85, 102, 119, 136, 153, 170, 187, 204, 221, 238, 255,
        ];
        let data: Vec<u8> = vec![0xAB, 0x12, 0xFF];
        let gray_values = encode_gray_direct_test(&data, 4, &gray_16);
        let decoded = decode_gray_direct(&gray_values, 4, &thresholds, data.len());
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_decode_gray_n_unified() {
        // 测试统一的 decode_gray_n 接口
        let data: Vec<u8> = vec![0xAB, 0xCD, 0x12, 0x34];

        for levels in [2u32, 4, 8, 16] {
            let config = crate::common::types::GrayEncodingConfig::new(levels).unwrap();
            let (gray_values, original_len) =
                crate::encoder::render::encode_gray_n(&data, &config);
            let decoded = decode_gray_n(&gray_values, levels, &config.thresholds, original_len);
            assert_eq!(
                decoded, data,
                "levels={}: decoded mismatch",
                levels
            );
        }
    }

    // ─── 测试辅助编码函数 ───

    fn encode_gray_direct_test(data: &[u8], bits_per_module: u32, gray_table: &[u8]) -> Vec<u8> {
        let total_bits = (data.len() * 8) as u64;
        let num_modules =
            ((total_bits + bits_per_module as u64 - 1) / bits_per_module as u64) as usize;
        let mask = (1u32 << bits_per_module) - 1;
        let mut gray_values = Vec::with_capacity(num_modules);
        for i in 0..num_modules {
            let bit_offset = (i as u64) * (bits_per_module as u64);
            let byte_idx = (bit_offset / 8) as usize;
            let bit_pos = (bit_offset % 8) as u32;
            let mut digit: u32 = 0;
            if byte_idx < data.len() {
                digit = (data[byte_idx] as u32 >> bit_pos) & mask;
            }
            let remaining = bits_per_module as i32 - (8 - bit_pos as i32);
            if remaining > 0 && byte_idx + 1 < data.len() {
                digit |= ((data[byte_idx + 1] as u32) & ((1u32 << remaining) - 1))
                    << (8 - bit_pos);
            }
            gray_values.push(gray_table[(digit & mask) as usize]);
        }
        gray_values
    }
}
