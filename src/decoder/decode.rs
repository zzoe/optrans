//! 帧解码模块
//!
//! 从截屏像素中完整解码一帧：
//! 1. 提取格式信息并解码（三重投票 + CRC 校验）
//! 2. 提取数据载荷并 RS 解码

use crate::common::crc::crc16_xmodem;
use crate::common::error::{OptransError, OptransResult};
use crate::common::rs_utils::rs_encoded_size;
use crate::common::types::{
    DecodedFrame, EncodingMode, FinderPattern, FormatInfo, FrameKind, FrameLocation,
    FrameType, GrayEncodingConfig, MetaPayload,
};
use crate::decoder::extract;
use crate::decoder::reed_solomon::rs_decode_data;

/// 从截屏中解码格式信息
///
/// 主路径用 3×3 采样（radius=1）。CRC 失败时降级重试：
/// 1. 整体平移 ±1 像素（8 种组合）——纠正 finder 中心亚像素抖动导致的整体偏移
/// 2. 单点采样（radius=0）——定位精准时最稳，避开相邻模块
/// 3. 5×5 采样（radius=2）——抗随机噪声
///
/// v0.13: 动态 N 路多数投票（3× 或 5× 重复），抵抗远程桌面视频压缩噪声。
pub fn decode_format_info(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
) -> OptransResult<FormatInfo> {
    let raw_bits = extract::extract_format_modules_with_radius(pixels, img_width, img_height, loc, 0);

    // v0.13: 根据帧宽动态计算重复次数
    let (repetitions, min_bits) = format_info_params(loc.modules_w);

    // 比特不足（定位到极小假帧）：微调无意义，直接返回
    if raw_bits.len() < min_bits {
        return Err(OptransError::InvalidFormatInfo(format!(
            "格式信息比特不足: 需要 {} bits，实际 {} bits",
            min_bits,
            raw_bits.len()
        )));
    }

    // 假帧快速拒绝
    if raw_bits.iter().all(|&b| b == 1) || raw_bits.iter().all(|&b| b == 0) {
        return Err(OptransError::InvalidFormatInfo(
            "假帧: 格式信息区全白/全黑，定位到空白区域".to_string(),
        ));
    }

    // 原始尝试
    match vote_and_check(&raw_bits, repetitions) {
        Ok(info) => Ok(info),
        Err(first_err) => {
            let is_fake = matches!(&first_err, OptransError::InvalidFormatInfo(m) if m.starts_with("假帧"));
            if is_fake {
                return Err(first_err);
            }
            // CRC 失败：降级重试
            for dx in [-1i32, 0, 1] {
                for dy in [-1i32, 0, 1] {
                    if dx == 0 && dy == 0 { continue; }
                    let adj = shift_loc(loc, dx as f64, dy as f64);
                    let raw2 = extract::extract_format_modules_with_radius(pixels, img_width, img_height, &adj, 0);
                    if raw2.len() >= min_bits {
                        if let Ok(info) = vote_and_check(&raw2, repetitions) {
                            return Ok(info);
                        }
                        // v0.13: 平移后帧宽不变，但为防御性也尝试不同重复次数
                        let r2 = format_info_params(adj.modules_w).0;
                        if r2 != repetitions && raw2.len() >= format_info_params(adj.modules_w).1
                            && let Ok(info) = vote_and_check(&raw2, r2) {
                                return Ok(info);
                            }
                    }
                }
            }
            // 2. 不同采样半径（先试 3×3 默认）
            for radius in [1i32, 2] {
                let raw2 = extract::extract_format_modules_with_radius(
                    pixels, img_width, img_height, loc, radius,
                );
                if raw2.len() >= min_bits
                    && let Ok(info) = vote_and_check(&raw2, repetitions) {
                        return Ok(info);
                    }
            }
            Err(first_err)
        }
    }
}

/// v0.13: 根据帧模块宽度计算格式信息重复次数和所需最小比特数
fn format_info_params(modules_w: u32) -> (usize, usize) {
    let format_area = 2 * (modules_w as usize - 14);
    let copy_bits = 15 * 8; // 120 bits per copy
    let max_reps = (format_area / copy_bits).max(1);
    let reps = max_reps.min(5);
    (reps, reps * copy_bits)
}

/// N 路多数投票 + CRC 校验 + 解包 (v0.13: 支持 3×/5× 重复)
fn vote_and_check(raw_bits: &[u8], repetitions: usize) -> OptransResult<FormatInfo> {
    let copy_bits = 15 * 8; // 120 bits per copy
    let min_required = copy_bits * repetitions;

    if raw_bits.len() < min_required {
        return Err(OptransError::InvalidFormatInfo(format!(
            "格式信息比特不足: 需要 {} bits，实际 {} bits",
            min_required, raw_bits.len()
        )));
    }

    let encoded_bits = &raw_bits[..min_required];

    let mut decoded_15bytes = [0u8; 15];
    for byte_idx in 0..15 {
        let mut byte_val: u8 = 0;
        for bit in 0..8 {
            let bit_pos = byte_idx * 8 + bit;
            let mut sum: u32 = 0;
            for rep in 0..repetitions {
                let idx = bit_pos + rep * copy_bits;
                if idx < encoded_bits.len() {
                    sum += encoded_bits[idx] as u32;
                }
            }
            // 多数投票：超过半数则为 1
            if sum * 2 > repetitions as u32 {
                byte_val |= 1 << bit;
            }
        }
        decoded_15bytes[byte_idx] = byte_val;
    }

    let data_part = &decoded_15bytes[..13];
    let crc_received = u16::from_le_bytes(
        decoded_15bytes[13..15]
            .try_into()
            .map_err(|_| OptransError::InvalidFormatInfo("CRC 提取失败".to_string()))?,
    );

    let crc_computed = crc16_xmodem(data_part);
    if crc_computed != crc_received {
        if crc_received == 0x0000 || crc_received == 0xFFFF {
            return Err(OptransError::InvalidFormatInfo(format!(
                "假帧: CRC 全0/全1 (收到 0x{:04X})，定位到空白区域",
                crc_received
            )));
        }
        return Err(OptransError::InvalidFormatInfo(format!(
            "CRC 校验失败: 计算 0x{:04X}, 收到 0x{:04X}",
            crc_computed, crc_received
        )));
    }

    unpack_format_info(data_part)
}

/// 生成整体平移后的 FrameLocation（不依赖 Clone，逐字段构造）
fn shift_loc(loc: &FrameLocation, dx: f64, dy: f64) -> FrameLocation {
    FrameLocation {
        tl: FinderPattern {
            center_x: loc.tl.center_x + dx,
            center_y: loc.tl.center_y + dy,
            module_size: loc.tl.module_size,
        },
        tr: FinderPattern {
            center_x: loc.tr.center_x + dx,
            center_y: loc.tr.center_y + dy,
            module_size: loc.tr.module_size,
        },
        bl: FinderPattern {
            center_x: loc.bl.center_x + dx,
            center_y: loc.bl.center_y + dy,
            module_size: loc.bl.module_size,
        },
        modules_per_side: loc.modules_per_side,
        modules_w: loc.modules_w,
        modules_h: loc.modules_h,
        module_pixels: loc.module_pixels,
    }
}

/// 从格式信息中检测编码模式
pub fn detect_encoding_mode(format_info: &FormatInfo) -> EncodingMode {
    if format_info.encoding_mode_bit == 0 {
        EncodingMode::Binary
    } else {
        EncodingMode::Gray(crate::common::constants::GRAY_LEVELS_DEFAULT)
    }
}

/// 计算灰度编码模式下的模块数
fn calc_gray_module_count(byte_count: usize, levels: u32) -> usize {
    match levels {
        2 => byte_count * 8,
        4 => byte_count * 4,
        8 => (byte_count * 8).div_ceil(3),
        16 => byte_count * 2,
        _ => byte_count * 8,
    }
}

/// 解码帧的数据载荷
pub fn decode_payload(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
    info: &FormatInfo,
    encoding_mode: &EncodingMode,
) -> OptransResult<Vec<u8>> {
    if info.payload_bytes == 0 {
        return Ok(Vec::new());
    }

    let payload_bytes = info.payload_bytes as usize;
    let encoded_bytes = rs_encoded_size(payload_bytes);

    let encoded_bits = match encoding_mode {
        EncodingMode::Gray(levels) => calc_gray_module_count(encoded_bytes, *levels),
        _ => encoded_bytes * 8,
    };

    // 提取数据模块原始值
    let module_values = extract::extract_data_modules(
        pixels,
        img_width,
        img_height,
        loc,
        encoded_bits,
        encoding_mode,
    );

    if module_values.len() < encoded_bits {
        return Err(OptransError::FrameDecode(format!(
            "数据模块值不足: 需要 {} modules，实际 {} modules",
            encoded_bits,
            module_values.len()
        )));
    }

    // 模块值 → 字节（仅使用需要的模块数）
    let relevant_modules = &module_values[..encoded_bits];
    let encoded_data = match encoding_mode {
        EncodingMode::Gray(levels) => {
            let config = GrayEncodingConfig::new(*levels);
            extract::decode_gray_n(relevant_modules, *levels, &config.thresholds, encoded_bytes)
        }
        _ => {
            let mut data = Vec::with_capacity(encoded_bytes);
            for byte_idx in 0..encoded_bytes {
                let mut byte_val: u8 = 0;
                for bit in 0..8 {
                    let bit_pos = byte_idx * 8 + bit;
                    if bit_pos < relevant_modules.len() && relevant_modules[bit_pos] == 1 {
                        byte_val |= 1 << bit;
                    }
                }
                data.push(byte_val);
            }
            data
        }
    };

    // v0.6: 反向应用数据掩码（XOR 自反），恢复原始 RS 编码字节
    let encoded_data = crate::common::mask::apply_mask(&encoded_data);

    // RS 解码
    let decoded = rs_decode_data(&encoded_data)?;

    Ok(decoded)
}

/// 完整解码一帧（格式信息 + 载荷）
pub fn decode_frame(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
    encoding_mode: &EncodingMode,
) -> OptransResult<DecodedFrame> {
    let format_info = decode_format_info(pixels, img_width, img_height, loc)?;

    let actual_mode = match encoding_mode {
        EncodingMode::Auto => detect_encoding_mode(&format_info),
        other => other.clone(),
    };

    let payload =
        decode_payload(pixels, img_width, img_height, loc, &format_info, &actual_mode)?;

    Ok(DecodedFrame {
        frame_type: format_info.frame_type,
        frame_sequence: format_info.frame_sequence,
        block_index: format_info.block_index,
        total_blocks: format_info.total_blocks,
        payload,
        frame_kind: format_info.frame_kind,
        encoding_mode: actual_mode,
    })
}

/// 从解码后的元数据帧载荷中解析 MetaPayload
pub fn parse_meta_payload(payload: &[u8]) -> OptransResult<MetaPayload> {
    if payload.is_empty() {
        return Err(OptransError::FrameDecode("元数据载荷为空".to_string()));
    }

    let mut offset = 0usize;

    if offset >= payload.len() {
        return Err(OptransError::FrameDecode(
            "元数据格式错误：缺少文件名长度".to_string(),
        ));
    }
    let name_len = payload[offset] as usize;
    offset += 1;

    if offset + name_len > payload.len() {
        return Err(OptransError::FrameDecode(
            "元数据格式错误：文件名超出边界".to_string(),
        ));
    }
    let file_name = String::from_utf8(payload[offset..offset + name_len].to_vec())
        .map_err(|e| OptransError::FrameDecode(format!("文件名 UTF-8 解码失败: {}", e)))?;
    offset += name_len;

    if offset + 8 > payload.len() {
        return Err(OptransError::FrameDecode(
            "元数据格式错误：缺少文件大小".to_string(),
        ));
    }
    let file_size = u64::from_le_bytes(
        payload[offset..offset + 8]
            .try_into()
            .map_err(|_| OptransError::FrameDecode("文件大小解析失败".to_string()))?,
    );
    offset += 8;

    if offset + 32 > payload.len() {
        return Err(OptransError::FrameDecode(
            "元数据格式错误：缺少 SHA-256".to_string(),
        ));
    }
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&payload[offset..offset + 32]);
    offset += 32;

    if offset + 4 > payload.len() {
        return Err(OptransError::FrameDecode(
            "元数据格式错误：缺少总块数".to_string(),
        ));
    }
    let total_blocks = u32::from_le_bytes(
        payload[offset..offset + 4]
            .try_into()
            .map_err(|_| OptransError::FrameDecode("总块数解析失败".to_string()))?,
    );

    Ok(MetaPayload {
        file_name,
        file_size,
        sha256,
        total_blocks,
    })
}

/// 从 FormatInfo 打包的 13 字节解包
fn unpack_format_info(data: &[u8]) -> OptransResult<FormatInfo> {
    if data.len() < 13 {
        return Err(OptransError::InvalidFormatInfo(format!(
            "格式信息数据不足: 需要 13 bytes，实际 {} bytes",
            data.len()
        )));
    }

    let mut val: u128 = 0;
    for i in 0..13 {
        val |= (data[i] as u128) << (i * 8);
    }

    let frame_type_raw = (val & 0x3) as u8;
    let frame_sequence = ((val >> 2) & 0xFFFF) as u16;
    let block_index = ((val >> 18) & 0xFFFF_FFFF) as u32;
    let total_blocks = ((val >> 50) & 0xFFFF_FFFF) as u32;
    let payload_bytes = ((val >> 82) & 0xFFFFF) as u32; // 20 bits
    let frame_kind_val = ((val >> 102) & 0x1) as u8;
    let encoding_mode_bit = ((val >> 103) & 0x1) as u8;

    let frame_type = FrameType::from_u8(frame_type_raw).ok_or_else(|| {
        OptransError::InvalidFormatInfo(format!("未知帧类型: {}", frame_type_raw))
    })?;

    let frame_kind = FrameKind::from_u8(frame_kind_val).unwrap_or(FrameKind::I);

    Ok(FormatInfo {
        frame_type,
        frame_sequence,
        block_index,
        total_blocks,
        payload_bytes,
        frame_kind,
        encoding_mode_bit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unpack_format_info_roundtrip() {
        let info = FormatInfo {
            frame_type: FrameType::Data,
            frame_sequence: 42,
            block_index: 100,
            total_blocks: 500,
            payload_bytes: 2000,
            frame_kind: FrameKind::I,
            encoding_mode_bit: 0,
        };

        let mut packed = vec![0u8; 13];
        {
            let mut val: u128 = 0;
            val |= info.frame_type as u128;
            val |= (info.frame_sequence as u128) << 2;
            val |= (info.block_index as u128) << 18;
            val |= (info.total_blocks as u128) << 50;
            val |= (info.payload_bytes as u128) << 82;
            val |= (info.frame_kind as u128) << 102;
            val |= (info.encoding_mode_bit as u128) << 103;
            for i in 0..13 {
                packed[i] = ((val >> (i * 8)) & 0xFF) as u8;
            }
        }

        let unpacked = unpack_format_info(&packed).unwrap();
        assert_eq!(unpacked.frame_type, FrameType::Data);
        assert_eq!(unpacked.frame_sequence, 42);
        assert_eq!(unpacked.block_index, 100);
        assert_eq!(unpacked.total_blocks, 500);
        assert_eq!(unpacked.payload_bytes, 2000);
        assert_eq!(unpacked.frame_kind, FrameKind::I);
        assert_eq!(unpacked.encoding_mode_bit, 0);
    }

    #[test]
    fn test_unpack_format_info_v02() {
        let info = FormatInfo {
            frame_type: FrameType::Data,
            frame_sequence: 7,
            block_index: 3,
            total_blocks: 10,
            payload_bytes: 500,
            frame_kind: FrameKind::P,
            encoding_mode_bit: 1,
        };

        let mut packed = vec![0u8; 13];
        {
            let mut val: u128 = 0;
            val |= info.frame_type as u128;
            val |= (info.frame_sequence as u128) << 2;
            val |= (info.block_index as u128) << 18;
            val |= (info.total_blocks as u128) << 50;
            val |= (info.payload_bytes as u128) << 82;
            val |= (info.frame_kind as u128) << 102;
            val |= (info.encoding_mode_bit as u128) << 103;
            for i in 0..13 {
                packed[i] = ((val >> (i * 8)) & 0xFF) as u8;
            }
        }

        let unpacked = unpack_format_info(&packed).unwrap();
        assert_eq!(unpacked.frame_type, FrameType::Data);
        assert_eq!(unpacked.frame_kind, FrameKind::P);
        assert_eq!(unpacked.encoding_mode_bit, 1);
    }

    #[test]
    fn test_unpack_format_info_payload_20bit() {
        // payload_bytes 为 20 bits，测试边界值 1,048,575 (2^20-1)
        let info = FormatInfo {
            frame_type: FrameType::Data,
            frame_sequence: 1,
            block_index: 0,
            total_blocks: 1,
            payload_bytes: 1_048_575,
            frame_kind: FrameKind::I,
            encoding_mode_bit: 0,
        };

        let mut packed = vec![0u8; 13];
        let mut val: u128 = 0;
        val |= info.frame_type as u128;
        val |= (info.frame_sequence as u128) << 2;
        val |= (info.block_index as u128) << 18;
        val |= (info.total_blocks as u128) << 50;
        val |= (info.payload_bytes as u128) << 82;
        val |= (info.frame_kind as u128) << 102;
        val |= (info.encoding_mode_bit as u128) << 103;
        for i in 0..13 {
            packed[i] = ((val >> (i * 8)) & 0xFF) as u8;
        }

        let unpacked = unpack_format_info(&packed).unwrap();
        assert_eq!(unpacked.payload_bytes, 1_048_575);
    }

    #[test]
    fn test_parse_meta_payload() {
        let mut payload = Vec::new();
        let file_name = "test.dat";
        payload.push(file_name.len() as u8);
        payload.extend_from_slice(file_name.as_bytes());
        payload.extend_from_slice(&12345u64.to_le_bytes());
        payload.extend_from_slice(&[0xAB; 32]);
        payload.extend_from_slice(&10u32.to_le_bytes());

        let meta = parse_meta_payload(&payload).unwrap();
        assert_eq!(meta.file_name, "test.dat");
        assert_eq!(meta.file_size, 12345);
        assert_eq!(meta.sha256, [0xAB; 32]);
        assert_eq!(meta.total_blocks, 10);
    }

    #[test]
    fn test_detect_encoding_mode() {
        let binary_info = FormatInfo {
            encoding_mode_bit: 0,
            ..FormatInfo::new_eot(0)
        };
        assert_eq!(detect_encoding_mode(&binary_info), EncodingMode::Binary);

        let gray_info = FormatInfo {
            encoding_mode_bit: 1,
            ..FormatInfo::new_eot(0)
        };
        assert!(matches!(detect_encoding_mode(&gray_info), EncodingMode::Gray(_)));
    }

    #[test]
    fn test_calc_gray_module_count() {
        assert_eq!(calc_gray_module_count(100, 2), 800);
        assert_eq!(calc_gray_module_count(100, 4), 400);
        assert_eq!(calc_gray_module_count(100, 8), 267);
        assert_eq!(calc_gray_module_count(100, 16), 200);
    }
}
