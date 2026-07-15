//! 帧 → 像素缓冲区渲染
//!
//! 将 Frame 渲染为 RGBA 像素缓冲区，用于窗口显示。
//! v0.5: 支持长方形帧，render_frame 接受 (modules_w, modules_h) 双维度参数。

use crate::common::constants::{
    HEARTBEAT_BASE_GRAY, HEARTBEAT_DELTA, HEARTBEAT_PIXEL_SIZE,
    RGBA_CHANNELS,
};
use crate::common::crc::crc16_xmodem;
use crate::common::types::GrayEncodingConfig;
use crate::encoder::frame::Frame;
use crate::encoder::pattern::{generate_finder_pattern, is_finder_module, is_format_module};

/// 将帧渲染为 RGBA 像素缓冲区
///
/// # 参数
/// - `frame`: 要渲染的帧
/// - `module_pixels`: 每模块像素数
/// - `modules_w`: 水平方向模块数
/// - `modules_h`: 垂直方向模块数
/// - `gray_config`: 灰度编码配置
/// - `enable_heartbeat`: 是否注入心跳噪声
///
/// # 返回
/// RGBA 像素缓冲区，尺寸为 `(modules_w * module_pixels) × (modules_h * module_pixels) × 4` 字节
pub fn render_frame(
    frame: &Frame,
    module_pixels: u32,
    modules_w: u32,
    modules_h: u32,
    gray_config: &GrayEncodingConfig,
    enable_heartbeat: bool,
) -> Vec<u8> {
    let n_w = modules_w as usize;
    let n_h = modules_h as usize;
    let mp = module_pixels as usize;
    let img_width = n_w * mp;
    let img_height = n_h * mp;
    let total_pixels = img_width * img_height;
    let mut pixels = vec![0u8; total_pixels * RGBA_CHANNELS];

    // 预计算 finder pattern 矩阵
    let finder = generate_finder_pattern();

    // 预计算格式信息区填充比特
    let format_bits = encode_format_info_bits(&frame.format_info, modules_w);

    // 预计算数据矩阵灰度值序列
    let data_gray_values = build_data_gray_sequence(&frame.encoded_payload, n_w, n_h, gray_config);

    // 遍历每个模块坐标，填充像素
    let mut data_module_idx: usize = 0;
    let n_w_u32 = n_w as u32;
    let n_h_u32 = n_h as u32;

    for module_row in 0..n_h {
        for module_col in 0..n_w {
            let module_gray: u8 = if is_finder_module(module_col as u32, module_row as u32, n_w_u32, n_h_u32)
            {
                // Finder Pattern 模块（始终二值：黑/白）
                let fp_row = if module_row < 7 {
                    module_row
                } else if module_row >= n_h - 7 {
                    module_row - (n_h - 7)
                } else {
                    continue;
                };
                let fp_col = if module_col < 7 {
                    module_col
                } else if module_col >= n_w - 7 {
                    module_col - (n_w - 7)
                } else {
                    continue;
                };
                if finder[fp_row][fp_col] == 1 {
                    0u8
                } else {
                    255u8
                }
            } else if is_format_module(module_col as u32, module_row as u32, n_w_u32, n_h_u32) {
                // 格式信息区模块（始终二值：黑/白）
                let format_idx = format_module_index(module_col, module_row, n_w, n_h);
                if format_idx < format_bits.len() {
                    // 与数据模块约定一致：bit 1 → 白(255)，bit 0 → 黑(0)
                    if format_bits[format_idx] == 1 {
                        255u8
                    } else {
                        0u8
                    }
                } else {
                    255u8
                }
            } else {
                // 数据矩阵模块
                if data_module_idx < data_gray_values.len() {
                    let val = data_gray_values[data_module_idx];
                    data_module_idx += 1;
                    val
                } else {
                    // 数据不足，填充白色
                    255u8
                }
            };

            // 填充该模块对应的像素块
            fill_module_pixels_gray(&mut pixels, img_width, module_col, module_row, mp, module_gray);
        }
    }

    // 心跳注入
    if enable_heartbeat {
        inject_heartbeat(&mut pixels, img_width, n_w, n_h, mp, frame.format_info.frame_sequence);
    }

    pixels
}

/// 计算格式信息模块在比特序列中的索引
///
/// 格式信息区：模块行 n_h-2 和 n_h-1（帧底部），列 7..n_w-8
/// 填充顺序：先行 n_h-2（从左到右），再行 n_h-1（从左到右）
fn format_module_index(col: usize, row: usize, n_w: usize, n_h: usize) -> usize {
    let s = 7;
    let start_col = s;
    let end_col = n_w - s;
    let cols_per_row = end_col - start_col;

    if row == n_h - 2 {
        col - start_col
    } else {
        cols_per_row + (col - start_col)
    }
}

/// 将格式信息编码为 bits
///
/// v0.13: 动态重复次数。当格式信息区 ≥600 模块时使用 5× 重复（更抗视频压缩噪声），
/// 否则使用 3× 重复（兼容窄帧）。
fn encode_format_info_bits(info: &crate::common::types::FormatInfo, n_w: u32) -> Vec<u8> {
    use crate::common::constants::{
        FORMAT_INFO_BYTES,
        FORMAT_INFO_WITH_CRC,
    };

    // 动态计算重复次数：每份拷贝 120 bits (15 bytes × 8)，n 份 = 120*n
    // 格式信息区 = 2 × (n_w - 14) 模块
    let format_area_modules = 2 * (n_w as usize - 14);
    let copy_bits = FORMAT_INFO_WITH_CRC * 8; // 120
    let max_repetitions = format_area_modules / copy_bits; // 整数除，向下取整
    let repetitions = max_repetitions.min(5).max(1);

    // 1. 打包 104 bits → 13 bytes
    let mut packed = vec![0u8; FORMAT_INFO_BYTES];
    pack_format_info(info, &mut packed);

    // 2. CRC-16：13 bytes → 15 bytes
    let crc = crc16_xmodem(&packed);
    let mut with_crc = Vec::with_capacity(FORMAT_INFO_WITH_CRC);
    with_crc.extend_from_slice(&packed);
    with_crc.extend_from_slice(&crc.to_le_bytes());

    // 3. N 重重复：15 bytes × N
    let total_bytes = FORMAT_INFO_WITH_CRC * repetitions;
    let total_bits = total_bytes * 8;
    let mut encoded = Vec::with_capacity(total_bytes);
    for _ in 0..repetitions {
        encoded.extend_from_slice(&with_crc);
    }

    // 4. 展开为 bits
    let mut bits = Vec::with_capacity(total_bits);
    for byte in &encoded {
        for bit in 0..8 {
            bits.push((byte >> bit) & 1);
        }
    }

    bits
}

/// 将 FormatInfo 打包为 13 字节（104 bits）
///
/// 位布局：
/// - bits 0-1:    frame_type (2b)
/// - bits 2-17:   frame_sequence (16b)
/// - bits 18-49:  block_index (32b)
/// - bits 50-81:  total_blocks (32b)
/// - bits 82-102: payload_bytes (21b)
/// - bit  103:    encoding_mode_bit (1b)
/// 总计：104 bits → 13 bytes
fn pack_format_info(info: &crate::common::types::FormatInfo, buf: &mut [u8]) {
    let ft = info.frame_type as u128;
    let seq = info.frame_sequence as u128;
    let bi = info.block_index as u128;
    let tb = info.total_blocks as u128;
    let pb = info.payload_bytes as u128;
    let emb = info.encoding_mode_bit as u128; // bit 103

    let mut val: u128 = 0;
    val |= ft; // bits 0-1
    val |= seq << 2; // bits 2-17
    val |= bi << 18; // bits 18-49
    val |= tb << 50; // bits 50-81
    val |= pb << 82; // bits 82-102 (21 bits)
    val |= emb << 103; // bit 103: encoding_mode_bit

    // 104 bits → 13 bytes（小端序写入）
    for i in 0..13 {
        buf[i] = ((val >> (i * 8)) & 0xFF) as u8;
    }
}

/// 从已编码载荷数据构建数据矩阵灰度值序列
///
/// 根据灰度配置将 RS 编码后的字节序列编码为模块灰度值。
/// 超出数据模块数的部分被截断，末尾少数模块留白。
///
/// v0.6: 应用数据掩码（XOR）打散大片同色区域，提升采样稳定性。
fn build_data_gray_sequence(
    encoded_payload: &[u8],
    n_w: usize,
    n_h: usize,
    gray_config: &GrayEncodingConfig,
) -> Vec<u8> {
    let total_modules = n_w * n_h;
    let finder_modules = 3 * (7 * 7);
    let format_modules = 2 * (n_w - 14);
    let max_data_modules = total_modules - finder_modules - format_modules;

    // 应用数据掩码（XOR），打散大片同色区域
    let masked_payload = crate::common::mask::apply_mask(encoded_payload);

    // 将掩码后的数据按灰度配置编码为模块灰度值
    let (gray_values, _original_len) = encode_gray_n(&masked_payload, gray_config);

    // 截断到数据模块数（末尾少数模块由渲染循环填充）
    gray_values.into_iter().take(max_data_modules).collect()
}

// ─── 通用灰度编码 ───

/// 通用灰度编码：将字节切片按指定级数编码为模块灰度值序列
///
/// # 参数
/// - `data`: 原始字节数据
/// - `gray_config`: 灰度编码配置
///
/// # 返回
/// (灰度值序列, 原始数据长度)
pub fn encode_gray_n(data: &[u8], gray_config: &GrayEncodingConfig) -> (Vec<u8>, usize) {
    encode_gray_direct(data, gray_config.bits_per_module(), &gray_config.gray_table)
}

/// 直映射灰度编码：每模块编码 N bits（N=1,2,3,4）
///
/// 适用于 levels=2/4/8/16 的位宽直映射
fn encode_gray_direct(data: &[u8], bits_per_module: u32, gray_table: &[u8]) -> (Vec<u8>, usize) {
    let original_len = data.len();
    let total_bits = (original_len * 8) as u64;
    let num_modules = total_bits.div_ceil(bits_per_module as u64) as usize;
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
        let remaining_bits = bits_per_module as i32 - (8 - bit_pos as i32);
        if remaining_bits > 0 && byte_idx + 1 < data.len() {
            digit |= ((data[byte_idx + 1] as u32) & ((1u32 << remaining_bits) - 1))
                << (8 - bit_pos);
        }

        gray_values.push(gray_table[(digit & mask) as usize]);
    }

    (gray_values, original_len)
}

// ─── 心跳注入 ───

/// 在像素缓冲区右下角注入心跳噪声
///
/// 心跳位置：右下角 (n_w-3, n_h-3) 模块起始的 2×2 像素块
/// 心跳值：根据 frame_seq % 4 循环 (128, 129, 128, 127)，基准 ±1
pub fn inject_heartbeat(
    pixels: &mut [u8],
    img_width: usize,
    n_w: usize,
    n_h: usize,
    module_pixels: usize,
    frame_seq: u16,
) {
    // 心跳模块坐标：右下角偏移 3 个模块的 2×2 像素块
    let module_col = n_w - 3;
    let module_row = n_h - 3;
    let start_x = module_col * module_pixels;
    let start_y = module_row * module_pixels;

    // frame_seq % 4 → gray value 循环
    let hb_gray = match frame_seq % 4 {
        0 => HEARTBEAT_BASE_GRAY,
        1 => HEARTBEAT_BASE_GRAY + HEARTBEAT_DELTA,
        2 => HEARTBEAT_BASE_GRAY,
        3 => HEARTBEAT_BASE_GRAY - HEARTBEAT_DELTA,
        _ => HEARTBEAT_BASE_GRAY,
    };

    let size = HEARTBEAT_PIXEL_SIZE as usize;
    for py in 0..size {
        for px in 0..size {
            let x = start_x + px;
            let y = start_y + py;
            let img_height = pixels.len() / (img_width * RGBA_CHANNELS);
            if x < img_width && y < img_height {
                let idx = (y * img_width + x) * RGBA_CHANNELS;
                if idx + 3 < pixels.len() {
                    pixels[idx] = hb_gray;
                    pixels[idx + 1] = hb_gray;
                    pixels[idx + 2] = hb_gray;
                    pixels[idx + 3] = 255;
                }
            }
        }
    }
}

// ─── 模块像素填充（灰度版）───

/// 填充单个模块对应的像素块（灰度版本）
///
/// gray_value: 0=纯黑, 255=纯白, 中间值=灰度
fn fill_module_pixels_gray(
    pixels: &mut [u8],
    img_width: usize,
    module_col: usize,
    module_row: usize,
    module_pixels: usize,
    gray_value: u8,
) {
    let start_x = module_col * module_pixels;
    let start_y = module_row * module_pixels;

    for py in 0..module_pixels {
        for px in 0..module_pixels {
            let x = start_x + px;
            let y = start_y + py;
            let idx = (y * img_width + x) * RGBA_CHANNELS;
            if idx + 3 < pixels.len() {
                pixels[idx] = gray_value;
                pixels[idx + 1] = gray_value;
                pixels[idx + 2] = gray_value;
                pixels[idx + 3] = 255; // Alpha
            }
        }
    }
}

/// 便捷渲染函数
///
/// v0.5: 接受 (modules_w, modules_h) 双维度参数
pub fn render_frame_default(
    frame: &Frame,
    module_pixels: u32,
    modules_w: u32,
    modules_h: u32,
    gray_config: &GrayEncodingConfig,
    enable_heartbeat: bool,
) -> Vec<u8> {
    render_frame(
        frame,
        module_pixels,
        modules_w,
        modules_h,
        gray_config,
        enable_heartbeat,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::MODULES_PER_SIDE_DEFAULT;
    use crate::common::types::{FormatInfo, FrameType};

    #[test]
    fn test_pack_format_info_roundtrip() {
        let info = FormatInfo::new_data_binary(42, 100, 500, 2000);
        let mut packed = vec![0u8; 13];
        pack_format_info(&info, &mut packed);

        let val: u128 = (0..13).fold(0u128, |acc, i| {
            acc | ((packed[i] as u128) << (i * 8))
        });

        assert_eq!((val & 0x3) as u8, FrameType::Data as u8);
        assert_eq!(((val >> 2) & 0xFFFF) as u16, 42);
        assert_eq!(((val >> 18) & 0xFFFF_FFFF) as u32, 100);
        assert_eq!(((val >> 50) & 0xFFFF_FFFF) as u32, 500);
        // payload_bytes 21 bits
        assert_eq!(((val >> 82) & 0x1F_FFFF) as u32, 2000);
        // encoding_mode_bit=0 at bit 103
        assert_eq!(((val >> 103) & 0x1) as u8, 0);
    }

    #[test]
    fn test_encode_gray_2_roundtrip() {
        let config = GrayEncodingConfig::new(2);
        let data: Vec<u8> = vec![0b10101010, 0b01010101, 0xFF];
        let (gray_values, original_len) = encode_gray_n(&data, &config);

        // 用内部解码验证
        let decoded = decode_gray_direct_internal(&gray_values, 1, &[128], original_len);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_encode_gray_4_roundtrip() {
        let config = GrayEncodingConfig::new(4);
        let data: Vec<u8> = vec![0xE4, 0x00, 0xFF]; // 0xE4 = 11 10 01 00
        let (gray_values, original_len) = encode_gray_n(&data, &config);
        let decoded = decode_gray_direct_internal(&gray_values, 2, &config.thresholds, original_len);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_encode_gray_8_roundtrip() {
        let config = GrayEncodingConfig::new(8);
        let data: Vec<u8> = vec![0b111_110_10, 0b1_101_100_0];
        let (gray_values, original_len) = encode_gray_n(&data, &config);
        let decoded = decode_gray_direct_internal(&gray_values, 3, &config.thresholds, original_len);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_encode_gray_16_roundtrip() {
        let config = GrayEncodingConfig::new(16);
        let data: Vec<u8> = vec![0xAB, 0x12, 0xFF];
        let (gray_values, original_len) = encode_gray_n(&data, &config);
        let decoded = decode_gray_direct_internal(&gray_values, 4, &config.thresholds, original_len);
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_inject_heartbeat_values() {
        let n_w: usize = 200;
        let n_h: usize = 200;
        let mp: usize = 4;
        let width = n_w * mp;
        let height = n_h * mp;
        let pixels = vec![255u8; width * height * 4];

        // Test all 4 heartbeat phases
        for seq in 0..4u16 {
            let mut p = pixels.clone();
            inject_heartbeat(&mut p, width, n_w, n_h, mp, seq);

            let hb_x = (n_w - 3) * mp;
            let hb_y = (n_h - 3) * mp;
            let idx = (hb_y * width + hb_x) * 4;

            let expected = match seq {
                0 => 128,
                1 => 129,
                2 => 128,
                3 => 127,
                _ => 128,
            };
            assert_eq!(p[idx], expected, "seq={}: expected {}, got {}", seq, expected, p[idx]);
        }
    }

    #[test]
    fn test_render_frame_creates_correct_size() {
        let info = FormatInfo::new_eot(0);
        let frame = Frame {
            format_info: info,
            encoded_payload: vec![0u8; 100],
        };
        let config = GrayEncodingConfig::new(2);
        let n = MODULES_PER_SIDE_DEFAULT;
        let pixels = render_frame_default(&frame, 4, n, n, &config, false);
        assert_eq!(pixels.len(), 800 * 800 * 4);
    }

    /// v0.20: 4 级灰度渲染→解码往返测试
    ///
    /// 生成已知 payload，RS 编码，4 级灰度渲染到像素缓冲区，
    /// 直接从像素读取模块值，解码验证一致性。
    #[test]
    fn test_gray4_pixel_roundtrip() {
        use crate::common::mask;
        use crate::decoder::extract;

        let gray_config = GrayEncodingConfig::new(4);
        let mp = 4u32;
        let n_w = 200u32;
        let n_h = 200u32;

        // 生成 test payload
        let payload: Vec<u8> = (0..200u32).map(|i| (i.wrapping_mul(37).wrapping_add(13) % 256) as u8).collect();

        // RS 编码
        let encoder = crate::encoder::reed_solomon::RsEncoder::new();
        let rs_encoded = encoder.encode(&payload).unwrap();

        let info = FormatInfo::new_data(0, 0, 1, rs_encoded.len() as u32, 1);
        let frame = Frame { format_info: info, encoded_payload: rs_encoded.clone() };

        // 渲染到像素缓冲区
        let pixels = render_frame_default(&frame, mp, n_w, n_h, &gray_config, false);

        // 直接从像素采样数据模块：模块内所有像素同色，取中心像素即可
        let n_w_usize = n_w as usize;
        let n_h_usize = n_h as usize;
        let img_w = (n_w * mp) as usize;
        let mp_usize = mp as usize;
        let expected_count = rs_encoded.len() * 4;
        let mut sampled = Vec::with_capacity(expected_count);

        'outer: for row in 0..n_h_usize {
            for col in 0..n_w_usize {
                // 跳过 finder pattern
                if col < 7 && row < 7 { continue; }
                if col >= n_w_usize - 7 && row < 7 { continue; }
                if col < 7 && row >= n_h_usize - 7 { continue; }
                // 跳过格式信息区（v0.24: 位于帧底部）
                if row >= n_h_usize - 2 && col >= 7 && col < n_w_usize - 7 { continue; }
                // 已取够
                if sampled.len() >= expected_count { break 'outer; }

                // 模块中心像素坐标
                let cx = col * mp_usize + mp_usize / 2;
                let cy = row * mp_usize + mp_usize / 2;
                let idx = (cy * img_w + cx) * 4;
                if idx + 2 < pixels.len() {
                    let r = pixels[idx] as u32;
                    let g = pixels[idx + 1] as u32;
                    let b = pixels[idx + 2] as u32;
                    sampled.push(((r * 299 + g * 587 + b * 114) / 1000) as u8);
                }
            }
        }

        assert!(sampled.len() >= expected_count,
            "采样不足: 需要 {} 模块, 实际 {} 个", expected_count, sampled.len());

        // 灰度解码 → 去掩码 → RS 解码
        let decoded = extract::decode_gray_n(&sampled, 4, &gray_config.thresholds, rs_encoded.len());
        let unmasked = mask::apply_mask(&decoded);
        let final_data = crate::decoder::reed_solomon::rs_decode_data(&unmasked)
            .expect("RS 解码失败");

        assert_eq!(final_data, payload,
            "4级灰度往返测试失败！{} bytes", payload.len());
    }

    #[test]
    fn test_render_frame_rectangular() {
        // 测试长方形帧渲染
        let info = FormatInfo::new_eot(0);
        let frame = Frame {
            format_info: info,
            encoded_payload: vec![0u8; 100],
        };
        let config = GrayEncodingConfig::new(2);
        let n_w: u32 = 160;
        let n_h: u32 = 90;
        let pixels = render_frame_default(&frame, 4, n_w, n_h, &config, false);
        assert_eq!(pixels.len(), (640 * 360 * 4) as usize);
    }

    // ─── 内部解码辅助函数（仅用于测试） ───

    /// 直映射灰度解码（内部测试用）
    fn decode_gray_direct_internal(
        gray_values: &[u8],
        bits_per_module: u32,
        thresholds: &[u8],
        original_len: usize,
    ) -> Vec<u8> {
        let total_bits = original_len * 8;
        let mut bytes = vec![0u8; (total_bits + 7) / 8];
        let mask = (1u32 << bits_per_module) - 1;

        for (i, &gray) in gray_values.iter().enumerate() {
            let digit = classify_gray(gray, thresholds);

            let bit_offset = (i as u64) * (bits_per_module as u64);
            let byte_idx = (bit_offset / 8) as usize;
            let bit_pos = (bit_offset % 8) as u32;

            if byte_idx < bytes.len() {
                bytes[byte_idx] |= (digit as u8) << bit_pos;
            }
            let remaining =
                bits_per_module as i32 - (8 - bit_pos as i32);
            if remaining > 0 && byte_idx + 1 < bytes.len() {
                bytes[byte_idx + 1] |=
                    (digit as u8 & (mask as u8)) >> (8 - bit_pos);
            }
        }

        bytes.truncate(original_len);
        bytes
    }

    /// 灰度值分类（测试用）
    fn classify_gray(gray: u8, thresholds: &[u8]) -> u32 {
        for (i, &th) in thresholds.iter().enumerate() {
            if gray < th {
                return i as u32;
            }
        }
        thresholds.len() as u32
    }
}
