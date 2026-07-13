//! 模块提取模块
//!
//! 通过 Finder Pattern 中心建立仿射变换，从截屏像素中采样每个模块的值。
//! 包含格式信息区模块提取和数据矩阵模块提取。
//! v0.2 新增：5×5 中值采样、多级灰度分类器、灰度解码。
//! v0.5: 支持长方形帧，使用 loc.modules_w / loc.modules_h 替代 modules_per_side。

use crate::common::types::{EncodingMode, FrameLocation};
use crate::common::constants::{SAMPLE_RADIUS_BINARY, SAMPLE_RADIUS_GRAY};

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
    fn from_location(loc: &FrameLocation) -> Self {
        let n_w = loc.modules_w as f64;
        let n_h = loc.modules_h as f64;
        let src_tl_x = 3.5;
        let src_tl_y = 3.5;
        let src_tr_x = n_w - 3.5;
        let src_bl_y = n_h - 3.5;

        let src_dx = src_tr_x - src_tl_x;
        let src_dy = src_bl_y - src_tl_y;

        let dst_dx_x = (loc.tr.center_x - loc.tl.center_x) / src_dx;
        let dst_dx_y = (loc.tr.center_y - loc.tl.center_y) / src_dx;
        let dst_dy_x = (loc.bl.center_x - loc.tl.center_x) / src_dy;
        let dst_dy_y = (loc.bl.center_y - loc.tl.center_y) / src_dy;

        // 预计算偏移，避免每个模块都做 (mx - 3.5) 运算
        let base_x = loc.tl.center_x - dst_dx_x * src_tl_x - dst_dy_x * src_tl_y;
        let base_y = loc.tl.center_y - dst_dx_y * src_tl_x - dst_dy_y * src_tl_y;

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
    let start_col = s;
    let end_col = n_w - s;

    let affine = AffineTransform::from_location(loc);

    let mut bits = Vec::with_capacity((end_col - start_col) * 2);

    for row in 0..2usize {
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

    let affine = AffineTransform::from_location(loc);

    let mut values = Vec::with_capacity(payload_bits);

    for row in 0..n_h {
        for col in 0..n_w {
            // 跳过 finder pattern 区域
            if is_in_finder(col, row, n_w, n_h) {
                continue;
            }
            // 跳过格式信息区
            if row < 2 && col >= s && col < n_w - s {
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
/// - Gray 模式：5×5 中值 → 原始灰度值 (0-255)
pub fn sample_module(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
    module_col: usize,
    module_row: usize,
    encoding_mode: &EncodingMode,
) -> u8 {
    let affine = AffineTransform::from_location(loc);
    sample_module_with_affine(pixels, img_width, img_height, &affine, module_col, module_row, encoding_mode)
}

/// 使用预计算的仿射变换采样单个模块（避免重复计算仿射参数）
///
/// v0.19.4: 灰度级 ≤4 时用 3×3 均值（与二值相同窗口大小），避免 5×5 窗口
/// 跨模块重叠；灰度级 >4 时才用 5×5 中值抗噪。
#[inline]
fn sample_module_with_affine(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    affine: &AffineTransform,
    module_col: usize,
    module_row: usize,
    encoding_mode: &EncodingMode,
) -> u8 {
    let (px, py) = affine.to_pixel(module_col, module_row);

    match encoding_mode {
        EncodingMode::Binary | EncodingMode::Auto => {
            sample_module_binary_at(pixels, img_width, img_height, px, py)
        }
        EncodingMode::Gray(levels) if *levels <= 4 => {
            // 低灰度级：3×3 均值，返回原始亮度（0-255）
            sample_pixel_region_luminance(pixels, img_width, img_height, px, py, SAMPLE_RADIUS_BINARY)
        }
        EncodingMode::Gray(_) => {
            // 高灰度级（5/8/16）：5×5 中值抗噪
            sample_module_gray(pixels, img_width, img_height, px, py)
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

// ─── v0.2 新增：5×5 中值采样 ───

/// 灰度采样（5×5 中值，返回原始亮度值 0-255）
pub fn sample_module_gray(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    px: f64,
    py: f64,
) -> u8 {
    sample_pixel_region_median(pixels, img_width, img_height, px, py, SAMPLE_RADIUS_GRAY)
}

/// 在像素坐标周围采样区域取亮度中值
///
/// 采样 radius×radius 区域，取中值。
/// v0.19.4: 用固定大小栈数组代替堆 Vec，消除每次采样的 alloc 开销。
/// 每帧数十万次采样，此优化可减少大量 malloc/free 调用。
fn sample_pixel_region_median(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    px: f64,
    py: f64,
    radius: i32,
) -> u8 {
    let w = img_width as usize;
    let cx = px.round() as i32;
    let cy = py.round() as i32;

    // 最大 radius=2 → 5×5=25 个像素，用栈数组避免堆分配
    let mut luminances: [u32; 25] = [0; 25];
    let mut count: usize = 0;

    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let sx = cx + dx;
            let sy = cy + dy;
            if sx >= 0 && sx < img_width && sy >= 0 && sy < img_height {
                let idx = (sy as usize * w + sx as usize) * 4;
                if idx + 2 < pixels.len() {
                    let r = pixels[idx] as u32;
                    let g = pixels[idx + 1] as u32;
                    let b = pixels[idx + 2] as u32;
                    luminances[count] = (r * 299 + g * 587 + b * 114) / 1000;
                    count += 1;
                }
            }
        }
    }

    if count == 0 {
        return 0;
    }

    // 仅排序有效元素
    luminances[..count].sort_unstable();
    luminances[count / 2] as u8
}

/// 在像素坐标周围采样区域取亮度均值（返回 0-255 原始值）
///
/// 与 sample_pixel_region_mean 相同算法，但不做二值化阈值判断。
/// 用于低灰度级（≤4）模式：3×3 窗口不会跨模块边界，原始亮度值
/// 被 classify_gray 分类到对应灰度级别。
fn sample_pixel_region_luminance(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    px: f64,
    py: f64,
    radius: i32,
) -> u8 {
    let w = img_width as usize;
    let cx = px.round() as i32;
    let cy = py.round() as i32;

    let mut sum: u32 = 0;
    let mut count: u32 = 0;

    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let sx = cx + dx;
            let sy = cy + dy;
            if sx >= 0 && sx < img_width && sy >= 0 && sy < img_height {
                let idx = (sy as usize * w + sx as usize) * 4;
                if idx + 2 < pixels.len() {
                    let r = pixels[idx] as u32;
                    let g = pixels[idx + 1] as u32;
                    let b = pixels[idx + 2] as u32;
                    sum += (r * 299 + g * 587 + b * 114) / 1000;
                    count += 1;
                }
            }
        }
    }

    if count == 0 {
        return 0;
    }
    (sum / count) as u8
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
    let w = img_width as usize;
    let cx = px.round() as i32;
    let cy = py.round() as i32;

    let mut sum: u32 = 0;
    let mut count: u32 = 0;

    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let sx = cx + dx;
            let sy = cy + dy;
            if sx >= 0 && sx < img_width && sy >= 0 && sy < img_height {
                let idx = (sy as usize * w + sx as usize) * 4;
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

    let avg = sum / count;
    // 约定：白色像素 → 1，黑色像素 → 0（与编码器 gray_table=[0,255] 对应：digit 0→黑，digit 1→白）
    if avg < 128 {
        0
    } else {
        1
    }
}

// ─── v0.2 新增：多级分类器 ───

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

// ─── v0.2 新增：灰度解码 ───

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
    if gray_levels == 5 {
        decode_gray_5(gray_values, original_len)
    } else {
        let bits_per_module = match gray_levels {
            2 => 1,
            4 => 2,
            8 => 3,
            16 => 4,
            _ => 1,
        };
        decode_gray_direct(gray_values, bits_per_module, thresholds, original_len)
    }
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

/// 5 进制灰度解码
fn decode_gray_5(gray_values: &[u8], original_len: usize) -> Vec<u8> {
    let thresholds: [u8; 4] = [32, 96, 160, 224];
    let mut bytes = Vec::with_capacity(original_len);

    for chunk in gray_values.chunks(7) {
        let mut v: u32 = 0;
        let mut multiplier: u32 = 1;

        for &gray in chunk {
            let digit = classify_gray(gray, &thresholds) as u32;
            v += digit * multiplier;
            multiplier *= 5;
        }

        bytes.push(v as u8);
        bytes.push((v >> 8) as u8);
    }

    bytes.truncate(original_len);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_gray_5() {
        let thresholds: [u8; 4] = [32, 96, 160, 224];
        assert_eq!(classify_gray(0, &thresholds), 0);
        assert_eq!(classify_gray(31, &thresholds), 0);
        assert_eq!(classify_gray(32, &thresholds), 1);
        assert_eq!(classify_gray(95, &thresholds), 1);
        assert_eq!(classify_gray(128, &thresholds), 2);
        assert_eq!(classify_gray(192, &thresholds), 3);
        assert_eq!(classify_gray(255, &thresholds), 4);
    }

    #[test]
    fn test_classify_gray_4() {
        let thresholds: [u8; 3] = [42, 127, 212];
        assert_eq!(classify_gray(0, &thresholds), 0);
        assert_eq!(classify_gray(85, &thresholds), 1);
        assert_eq!(classify_gray(170, &thresholds), 2);
        assert_eq!(classify_gray(255, &thresholds), 3);
    }

    #[test]
    fn test_decode_gray_5_roundtrip() {
        // 编码已知数据
        let data: Vec<u8> = vec![0xAB, 0xCD, 0x12, 0x34, 0xFF];
        let gray_values = encode_gray_5_test(&data);

        // 解码
        let decoded = decode_gray_5(&gray_values, data.len());
        assert_eq!(decoded, data);
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

        for levels in [2u32, 4, 5, 8, 16] {
            let config = crate::common::types::GrayEncodingConfig::new(levels);
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

    fn encode_gray_5_test(data: &[u8]) -> Vec<u8> {
        const GRAY_MAP: [u8; 5] = [0, 64, 128, 192, 255];
        let mut gray_values = Vec::new();
        for chunk in data.chunks(2) {
            let b0 = chunk[0] as u16;
            let b1 = if chunk.len() > 1 { chunk[1] as u16 } else { 0 };
            let mut v = b0 | (b1 << 8);
            for _ in 0..7 {
                let digit = (v % 5) as usize;
                gray_values.push(GRAY_MAP[digit]);
                v /= 5;
            }
        }
        gray_values
    }

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
