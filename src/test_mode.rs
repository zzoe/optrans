//! 测试模式
//!
//! test send / test receive 复用正常 send/receive 的完整流程（run_send / run_receive），
//! 仅 test send 的文件内容固定为测试数据，使测试结果能反映真实传输效果。
//! test decode-image 保留离线调试能力：从 PNG 截图直接解码。

use crate::cli::{ReceiveArgs, SendArgs};
use crate::common::error::{OptransError, OptransResult};
use crate::common::types::FrameLocation;

#[cfg(test)]
use crate::common::types::{BlockData, FormatInfo, FrameKind, GrayEncodingConfig};
#[cfg(test)]
use crate::encoder::frame::Frame;
#[cfg(test)]
use crate::encoder::reed_solomon::RsEncoder;
#[cfg(test)]
use crate::encoder::render::render_frame_default;

// === test send / test receive：复用正常流程 ===

/// 发送固定测试内容（复用 run_send 完整流程）
///
/// 生成临时测试文件（"HELLO OPTRANS TEST\n" 重复填充至 64KB），
/// 用与正常 send 完全相同的参数和发送流程（分块、RS、渲染、单轮、EOT）发送。
/// 这样测试结果能反映真实传输效果，而非简化链路的虚假成功。
pub fn test_send(mut args: SendArgs) -> OptransResult<()> {
    let template = b"HELLO OPTRANS TEST\n";
    let target_size = 64 * 1024; // 64KB
    let mut data = Vec::with_capacity(target_size);
    while data.len() + template.len() <= target_size {
        data.extend_from_slice(template);
    }
    let remaining = target_size - data.len();
    if remaining > 0 {
        data.extend_from_slice(&template[..remaining.min(template.len())]);
    }

    let test_path = std::env::temp_dir().join("optrans_test_payload.bin");
    std::fs::write(&test_path, &data)?;
    eprintln!(
        "[test] 测试内容已写入临时文件: {:?} ({} bytes, 模板 {:?} 重复)",
        test_path,
        data.len(),
        template
    );

    args.path = test_path;
    crate::sender::transmit::run_send(args)
}

/// 接收测试帧（复用 run_receive 完整流程）
///
/// 使用与正常 receive 完全相同的主循环、定位、解码、降级重试、统计、EOT 处理。
pub fn test_receive(args: ReceiveArgs) -> OptransResult<()> {
    eprintln!("[test] 接收端：使用与正常 receive 相同的流程");
    crate::receiver::mod_impl::run_receive(args)
}

// === 离线调试：从 PNG 图片解码 ===

/// 从 PNG 图片文件加载 RGBA 像素数据
fn load_png_rgba(path: &str) -> OptransResult<(Vec<u8>, i32, i32)> {
    let img = image::open(path)
        .map_err(|e| OptransError::Win32(format!("加载图片失败 {}: {}", path, e)))?;
    let rgba = img.to_rgba8();
    let w = rgba.width() as i32;
    let h = rgba.height() as i32;
    Ok((rgba.into_raw(), w, h))
}

/// 从 PNG 图片文件解码测试帧（离线调试用）
///
/// 加载 PNG → 全屏扫描定位 finder pattern → 解码 payload
pub fn test_decode_image(path: &str) -> OptransResult<()> {
    eprintln!("[test] 解码图片：加载 {}", path);
    let (pixels, img_w, img_h) = load_png_rgba(path)?;
    eprintln!("[test] 图片尺寸 {}×{}", img_w, img_h);

    // 全图扫描定位（返回所有候选，逐个解码）
    eprintln!("[test] 尝试全屏扫描 locate_frame_all ...");
    let locations = crate::decoder::locate::locate_frame_all(&pixels, img_w, img_h);
    if locations.is_empty() {
        eprintln!("[test] 定位失败！图片中未找到 finder pattern");
        return Err(OptransError::FrameDecode("定位失败".into()));
    }

    for loc in &locations {
        eprintln!(
            "[test] 候选: tl=({:.0},{:.0}), modules=({}w×{}h), ms={:.2}",
            loc.tl.center_x, loc.tl.center_y, loc.modules_w, loc.modules_h, loc.tl.module_size
        );
        match decode_test_data_direct(&pixels, img_w, img_h, loc) {
            Ok(data) => {
                let text = String::from_utf8_lossy(&data);
                eprintln!("[test] 解码成功！payload = {:?}", text);
                return Ok(());
            }
            Err(_) => continue,
        }
    }
    eprintln!("[test] 所有候选解码失败 ({} 个候选)", locations.len());
    Err(OptransError::FrameDecode("所有候选解码失败".into()))
}

/// 测试模式：直接从数据矩阵提取模块值（跳过格式信息）
///
/// 因为收发两端的屏幕分辨率不匹配时，格式信息的模块数会不一致，
/// 导致格式信息解码失败。此函数直接按 finder pattern 定位的模块
/// 网格采样黑白像素，不依赖格式信息。
///
/// v0.8: 改用仿射变换采样（与 extract.rs sample_module 一致），
/// 基于全部 3 个 finder 中心点推算像素坐标，消除行列级联漂移。
fn decode_test_data_direct(
    pixels: &[u8],
    img_w: i32,
    img_h: i32,
    loc: &FrameLocation,
) -> Result<Vec<u8>, OptransError> {
    let n_w = loc.modules_w as usize;
    let n_h = loc.modules_h as usize;

    // 仿射变换参数（与 extract.rs sample_module 一致）
    // TL finder 中心在模块坐标 (3.5, 3.5)
    // TR finder 中心在模块坐标 (n_w - 3.5, 3.5)
    // BL finder 中心在模块坐标 (3.5, n_h - 3.5)
    let n_w_f = n_w as f64;
    let n_h_f = n_h as f64;
    let src_tl_x = 3.5;
    let src_tl_y = 3.5;
    let src_tr_x = n_w_f - 3.5;
    let src_bl_y = n_h_f - 3.5;
    let src_dx = src_tr_x - src_tl_x;
    let src_dy = src_bl_y - src_tl_y;

    let dst_tl_x = loc.tl.center_x;
    let dst_tl_y = loc.tl.center_y;
    let dst_tr_x = loc.tr.center_x;
    let dst_tr_y = loc.tr.center_y;
    let dst_bl_x = loc.bl.center_x;
    let dst_bl_y = loc.bl.center_y;

    let dst_dx_x = (dst_tr_x - dst_tl_x) / src_dx;
    let dst_dx_y = (dst_tr_y - dst_tl_y) / src_dx;
    let dst_dy_x = (dst_bl_x - dst_tl_x) / src_dy;
    let dst_dy_y = (dst_bl_y - dst_tl_y) / src_dy;

    // 收集数据模块比特值（仿射变换定位像素坐标） 编码器约定（render.rs encode_gray_direct,
    // gray_table=[0,255]）： bit 0 → 黑色 (gray=0)，bit 1 → 白色 (gray=255)
    // 模块 i 对应字节流的 bit i（LSB-first）
    let mut bits: Vec<u8> = Vec::new();

    for row in 2..n_h {
        for col in 0..n_w {
            // 跳过 finder pattern 区域
            let in_tl = col < 7 && row < 7;
            let in_tr = col >= n_w - 7 && row < 7;
            let in_bl = col < 7 && row >= n_h - 7;
            if in_tl || in_tr || in_bl {
                continue;
            }

            // 仿射变换：模块坐标 (col+0.5, row+0.5) → 像素坐标 (px, py)
            let mx = col as f64 + 0.5;
            let my = row as f64 + 0.5;
            let px = dst_tl_x + dst_dx_x * (mx - src_tl_x) + dst_dy_x * (my - src_tl_y);
            let py = dst_tl_y + dst_dx_y * (mx - src_tl_x) + dst_dy_y * (my - src_tl_y);
            let xi = px as i32;
            let yi = py as i32;

            if xi < 0 || yi < 0 || xi >= img_w || yi >= img_h {
                continue;
            }

            let idx = (yi as usize * img_w as usize + xi as usize) * 4;
            if idx + 2 >= pixels.len() {
                continue;
            }
            let lum = (pixels[idx] as u32 * 299
                + pixels[idx + 1] as u32 * 587
                + pixels[idx + 2] as u32 * 114)
                / 1000;
            // 与编码器约定一致：black(0) → bit 0, white(255) → bit 1
            let bit: u8 = if lum < 128 { 0 } else { 1 };
            bits.push(bit);
        }
    }

    // 将比特序列打包为字节（LSB-first，与编码器一致）
    if bits.len() < 32 {
        return Err(OptransError::FrameDecode("数据模块不足".into()));
    }

    let mut bytes: Vec<u8> = Vec::with_capacity(bits.len() / 8);
    for chunk in bits.chunks(8) {
        let mut byte: u8 = 0;
        for (bit_pos, &bit) in chunk.iter().enumerate() {
            byte |= bit << bit_pos;
        }
        bytes.push(byte);
    }

    // v0.6: 反向应用数据掩码（XOR 自反），恢复原始 RS 编码字节
    // 必须在读取 total_len 之前应用，因为 total_len 字段也被掩码处理过
    let rs_encoded = crate::common::mask::apply_mask(&bytes);

    let total_len =
        u32::from_le_bytes([rs_encoded[0], rs_encoded[1], rs_encoded[2], rs_encoded[3]]) as usize;

    if total_len == 0 || total_len > rs_encoded.len() * 2 {
        return Err(OptransError::FrameDecode(format!(
            "无效的数据长度: {}",
            total_len
        )));
    }

    let rs_data = &rs_encoded[4..];
    let num_blocks = (total_len + 216) / 217; // RS_DATA_LEN-1
    let expected_rs_bytes = num_blocks * 255;

    if rs_data.len() < expected_rs_bytes {
        return Err(OptransError::FrameDecode(format!(
            "RS 数据不足: 需要 {}B, 实际 {}B",
            expected_rs_bytes,
            rs_data.len()
        )));
    }

    use crate::decoder::reed_solomon::rs_decode_data;
    rs_decode_data(&rs_encoded[..4 + expected_rs_bytes.min(rs_encoded.len())])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成测试：编码 → 渲染 → 全屏扫描定位 → 解码，验证完整链路
    #[test]
    fn test_decode_image_synthetic() {
        let payload = b"HELLO OPTRANS TEST";

        // 1. 编码
        let info = FormatInfo::new_meta(0, 1, payload.len() as u16);
        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(payload).unwrap();
        let frame = Frame {
            format_info: info,
            encoded_payload,
            frame_kind: FrameKind::I,
        };

        // 2. 渲染（小尺寸帧，模拟远程桌面窗口）
        let gray_config = GrayEncodingConfig::new(2);
        let module_pixels: u32 = 4;
        let modules_w: u32 = 100;
        let modules_h: u32 = 80;
        let frame_pixels = render_frame_default(
            &frame,
            module_pixels,
            modules_w,
            modules_h,
            &gray_config,
            false,
            None,
        );
        let frame_w = (modules_w * module_pixels) as i32;
        let frame_h = (modules_h * module_pixels) as i32;

        // 3. 将帧嵌入更大的白色背景中（模拟截图：帧只占屏幕一部分）
        let screen_w = 800i32;
        let screen_h = 600i32;
        let offset_x = 200i32;
        let offset_y = 150i32;
        let mut screen = vec![255u8; (screen_w * screen_h * 4) as usize];

        // 复制帧像素到屏幕缓冲区
        for y in 0..frame_h {
            for x in 0..frame_w {
                let src_idx = ((y * frame_w + x) * 4) as usize;
                let dst_x = offset_x + x;
                let dst_y = offset_y + y;
                if dst_x < 0 || dst_y < 0 || dst_x >= screen_w || dst_y >= screen_h {
                    continue;
                }
                let dst_idx = ((dst_y * screen_w + dst_x) * 4) as usize;
                screen[dst_idx..dst_idx + 4].copy_from_slice(&frame_pixels[src_idx..src_idx + 4]);
            }
        }

        // 4. 全屏扫描定位（遍历候选逐个解码）
        let locations = crate::decoder::locate::locate_frame_all(&screen, screen_w, screen_h);
        assert!(!locations.is_empty(), "全屏扫描应能定位帧");
        let mut success = false;
        for loc in &locations {
            if let Ok(data) = decode_test_data_direct(&screen, screen_w, screen_h, loc) {
                assert_eq!(data, payload);
                success = true;
                eprintln!(
                    "解码成功: tl=({:.0},{:.0}), modules=({}w×{}h), ms={:.2}, payload={:?}",
                    loc.tl.center_x,
                    loc.tl.center_y,
                    loc.modules_w,
                    loc.modules_h,
                    loc.tl.module_size,
                    String::from_utf8_lossy(&data)
                );
                break;
            }
        }
        assert!(success, "所有候选解码失败");
    }

    /// 验证正式接收端解码路径（decode_frame）能正确解码合成帧
    ///
    /// 这个测试覆盖 extract.rs 的 sample_module / sample_module_binary 函数，
    /// 确保模块中心采样（+0.5 偏移）正确工作，避免采样到模块边界导致解码失败。
    #[test]
    fn test_decode_frame_full_pipeline() {
        use crate::common::types::EncodingMode;
        use crate::decoder::decode::decode_frame;

        let payload = b"HELLO OPTRANS TEST";

        // 1. 编码
        let info = FormatInfo::new_meta(0, 1, payload.len() as u16);
        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(payload).unwrap();
        let frame = Frame {
            format_info: info,
            encoded_payload,
            frame_kind: FrameKind::I,
        };

        // 2. 渲染（帧尺寸需足够大以容纳格式信息区：FORMAT_INFO_ENCODED_BITS=360
        //        需要 (360/2 + 14) = 194 模块宽，使用 200×150）
        let gray_config = GrayEncodingConfig::new(2);
        let module_pixels: u32 = 4;
        let modules_w: u32 = 200;
        let modules_h: u32 = 150;
        let frame_pixels = render_frame_default(
            &frame,
            module_pixels,
            modules_w,
            modules_h,
            &gray_config,
            false,
            None,
        );
        let frame_w = (modules_w * module_pixels) as i32;
        let frame_h = (modules_h * module_pixels) as i32;

        // 3. 将帧嵌入更大的白色背景中（模拟截图）
        let screen_w = 1200i32;
        let screen_h = 900i32;
        let offset_x = 200i32;
        let offset_y = 150i32;
        let mut screen = vec![255u8; (screen_w * screen_h * 4) as usize];

        for y in 0..frame_h {
            for x in 0..frame_w {
                let src_idx = ((y * frame_w + x) * 4) as usize;
                let dst_x = offset_x + x;
                let dst_y = offset_y + y;
                let dst_idx = ((dst_y * screen_w + dst_x) * 4) as usize;
                screen[dst_idx..dst_idx + 4].copy_from_slice(&frame_pixels[src_idx..src_idx + 4]);
            }
        }

        // 4. 全屏扫描定位（遍历候选逐个解码）
        let locations = crate::decoder::locate::locate_frame_all(&screen, screen_w, screen_h);
        assert!(!locations.is_empty(), "全屏扫描应能定位帧");
        let mut success = false;
        for loc in &locations {
            if let Ok(decoded) = decode_frame(&screen, screen_w, screen_h, loc, &EncodingMode::Auto)
            {
                assert_eq!(decoded.payload, payload);
                success = true;
                eprintln!(
                    "decode_frame 解码成功: payload = {:?}",
                    String::from_utf8_lossy(&decoded.payload)
                );
                break;
            }
        }
        assert!(success, "所有候选 decode_frame 失败");
    }

    /// 满帧 payload（无循环填充）+ decode_test_data_direct 往返测试
    ///
    /// 验证大 payload（3038B）在 RS 编码填满 97% 数据区后，
    /// 全屏扫描定位 + decode_test_data_direct 能正确往返。
    /// 余下 ~3% 白边不产生假 finder pattern。
    #[test]
    fn test_full_payload_tiling_roundtrip() {
        // 1. 生成满帧 payload（用模板重复填充）
        let template = b"HELLO OPTRANS TEST\n";
        let modules_w: u32 = 200;
        let modules_h: u32 = 150;

        // 计算满帧 payload 容量（二值模式）
        let total = (modules_w * modules_h) as usize;
        let finder = 3 * 49;
        let format = 2 * (modules_w as usize - 14);
        let available = total - finder - format;
        let max_rs = available / 8;
        let num_blocks = (max_rs - 4) / crate::common::constants::RS_TOTAL_LEN;
        let max_payload = num_blocks * crate::common::constants::RS_DATA_LEN;

        let mut payload = Vec::with_capacity(max_payload);
        while payload.len() + template.len() <= max_payload {
            payload.extend_from_slice(template);
        }
        let remaining = max_payload - payload.len();
        if remaining > 0 {
            payload.extend_from_slice(&template[..remaining]);
        }

        eprintln!(
            "满帧测试: payload={}B, RSblocks={}",
            payload.len(),
            num_blocks
        );

        // 2. RS 编码
        let info = FormatInfo::new_meta(0, 1, payload.len() as u16);
        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(&payload).unwrap();
        let frame = Frame {
            format_info: info,
            encoded_payload,
            frame_kind: FrameKind::I,
        };

        // 3. 渲染（带循环填充）
        let gray_config = GrayEncodingConfig::new(2);
        let module_pixels: u32 = 4;
        let frame_pixels = render_frame_default(
            &frame,
            module_pixels,
            modules_w,
            modules_h,
            &gray_config,
            false,
            None,
        );
        let frame_w = (modules_w * module_pixels) as i32;
        let frame_h = (modules_h * module_pixels) as i32;

        // 4. 嵌入黑色背景（模拟真实窗口：finder 黑边与背景一致，不融合）
        let screen_w = 1200i32;
        let screen_h = 900i32;
        let offset_x = 200i32;
        let offset_y = 150i32;
        // 黑色背景匹配窗口实际底色，避免 finder 白模块与白背景融合
        let mut screen = vec![0u8; (screen_w * screen_h * 4) as usize];

        for y in 0..frame_h {
            for x in 0..frame_w {
                let src_idx = ((y * frame_w + x) * 4) as usize;
                let dst_x = offset_x + x;
                let dst_y = offset_y + y;
                if dst_x < 0 || dst_y < 0 || dst_x >= screen_w || dst_y >= screen_h {
                    continue;
                }
                let dst_idx = ((dst_y * screen_w + dst_x) * 4) as usize;
                screen[dst_idx..dst_idx + 4].copy_from_slice(&frame_pixels[src_idx..src_idx + 4]);
            }
        }

        // 5. 全屏扫描定位（遍历候选逐个解码）
        let locations = crate::decoder::locate::locate_frame_all(&screen, screen_w, screen_h);
        assert!(!locations.is_empty(), "满帧测试：全屏扫描应能定位帧");
        let mut success = false;
        for loc in &locations {
            eprintln!(
                "  候选: modules=({}w×{}h), ms={:.2}",
                loc.modules_w, loc.modules_h, loc.tl.module_size
            );
            if let Ok(decoded) = decode_test_data_direct(&screen, screen_w, screen_h, loc) {
                assert_eq!(decoded, payload, "满帧测试：解码结果应与原始 payload 一致");
                success = true;
                eprintln!(
                    "满帧测试通过: payload={}B 完整往返 (前80字节: {:?})",
                    payload.len(),
                    String::from_utf8_lossy(&decoded[..80.min(decoded.len())])
                );
                break;
            }
        }
        assert!(success, "满帧测试：所有候选 decode_test_data_direct 失败");
    }

    /// 诊断测试：单独验证 RS 编码解码往返 + 数据掩码往返
    #[test]
    fn test_rs_mask_roundtrip_large_payload() {
        use crate::decoder::reed_solomon::rs_decode_data;
        use crate::encoder::reed_solomon::RsEncoder;

        // 构建与真实发送相同大小的 payload (26908 bytes)
        let block_size = 26900usize;
        let template = b"HELLO OPTRANS TEST\n";
        let mut block_data = Vec::with_capacity(block_size);
        while block_data.len() + template.len() <= block_size {
            block_data.extend_from_slice(template);
        }
        let remaining = block_size - block_data.len();
        if remaining > 0 {
            block_data.extend_from_slice(&template[..remaining]);
        }

        // 构建多块合并 payload
        let mut payload = Vec::new();
        payload.extend_from_slice(&1u16.to_le_bytes()); // count=1
        payload.extend_from_slice(&0u32.to_le_bytes()); // block_index=0
        payload.extend_from_slice(&(block_data.len() as u16).to_le_bytes()); // data_len
        payload.extend_from_slice(&block_data); // data
        assert_eq!(payload.len(), 26908);

        // 1. RS 编码
        let encoder = RsEncoder::new();
        let encoded = encoder.encode(&payload).unwrap();
        eprintln!(
            "RS encoded: {} bytes (header=4 + {}*255)",
            encoded.len(),
            (encoded.len() - 4) / 255
        );

        // 2. 应用掩码
        let masked = crate::common::mask::apply_mask(&encoded);

        // 3. 编码为灰度模块 (binary: 1 bit per module)
        let gray_config = GrayEncodingConfig::new(2);
        let (gray_values, _) = crate::encoder::render::encode_gray_n(&masked, &gray_config);
        eprintln!("Gray values: {} modules", gray_values.len());

        // 4. 解码为字节
        let encoded_bytes = encoded.len();
        let mut decoded_bytes = vec![0u8; encoded_bytes];
        for byte_idx in 0..encoded_bytes {
            let mut byte_val: u8 = 0;
            for bit in 0..8 {
                let bit_pos = byte_idx * 8 + bit;
                if bit_pos < gray_values.len() {
                    // gray_values are [0, 255] for binary mode
                    if gray_values[bit_pos] > 128 {
                        byte_val |= 1 << bit;
                    }
                }
            }
            decoded_bytes[byte_idx] = byte_val;
        }

        // 5. 验证灰色值→字节解码
        assert_eq!(
            &decoded_bytes, &masked,
            "Gray→Byte decoding should roundtrip"
        );

        // 6. 去掩码
        let unmasked = crate::common::mask::apply_mask(&decoded_bytes);
        assert_eq!(
            &unmasked, &encoded,
            "Unmask should recover original RS data"
        );

        // 7. RS 解码
        let decoded = rs_decode_data(&unmasked).unwrap();
        assert_eq!(
            decoded, payload,
            "Full RS roundtrip should recover original payload"
        );
        assert_eq!(
            &decoded[8..8 + block_data.len()],
            &block_data,
            "Decoded block data should match original"
        );

        eprintln!(
            "RS+Mask roundtrip test PASSED for {} bytes payload",
            payload.len()
        );
    }

    #[test]
    fn test_rs_exact_multiple_roundtrip() {
        // 专门测试 payload 为 RS_DATA_LEN 整数倍的边界情况
        use crate::common::constants::RS_DATA_LEN;
        use crate::decoder::reed_solomon::rs_decode_data;
        use crate::encoder::reed_solomon::RsEncoder;

        // 124 * 217 = 26908
        let payload = vec![0xABu8; RS_DATA_LEN * 124];
        assert_eq!(payload.len(), 26908);
        assert_eq!(payload.len() % RS_DATA_LEN, 0);

        let encoder = RsEncoder::new();
        let encoded = encoder.encode(&payload).unwrap();
        eprintln!(
            "Exact multiple: payload={}, encoded={} (blocks={})",
            payload.len(),
            encoded.len(),
            (encoded.len() - 4) / 255
        );

        let decoded = rs_decode_data(&encoded).unwrap();
        assert_eq!(
            decoded,
            payload,
            "Exact multiple RS roundtrip failed: expected {} bytes, got {}",
            payload.len(),
            decoded.len()
        );
        eprintln!("Exact multiple RS roundtrip PASSED");

        // Also test with mask
        let masked = crate::common::mask::apply_mask(&encoded);
        let unmasked = crate::common::mask::apply_mask(&masked);
        assert_eq!(unmasked, encoded);
        eprintln!("Mask roundtrip PASSED");
    }

    /// 完整往返测试：构建 3 数据块帧 → 渲染 → locate → decode_frame → 验证数据
    ///
    /// 验证 v0.12 修复：三角搜索 TR/BL 约束收紧后，正确候选排在首位，CRC 通过。
    #[test]
    fn test_three_block_full_roundtrip() {
        use crate::common::types::EncodingMode;
        use crate::decoder::decode::decode_frame;

        let template = b"HELLO OPTRANS TEST\n";
        let target_size = 64 * 1024;
        let mut data = Vec::with_capacity(target_size);
        while data.len() + template.len() <= target_size {
            data.extend_from_slice(template);
        }
        let remaining = target_size - data.len();
        if remaining > 0 {
            data.extend_from_slice(&template[..remaining.min(template.len())]);
        }

        let module_pixels: u32 = 4;
        let modules_w: u32 = 640;
        let modules_h: u32 = 400;
        let gray_config = GrayEncodingConfig::new(2);

        let total = (modules_w * modules_h) as usize;
        let finder = 3 * 49;
        let format = 2 * ((modules_w as usize) - 14);
        let max_rs_encoded = (total - finder - format) / 8;
        let num_rs_blocks = (max_rs_encoded - 4) / crate::common::constants::RS_TOTAL_LEN;
        let max_payload = num_rs_blocks * crate::common::constants::RS_DATA_LEN;
        let block_size = max_payload.saturating_sub(8);

        let mut blocks: Vec<BlockData> = Vec::new();
        let mut offset = 0;
        let mut idx = 0u32;
        while offset < data.len() {
            let end = (offset + block_size).min(data.len());
            blocks.push(BlockData {
                index: idx,
                data: data[offset..end].to_vec(),
            });
            offset = end;
            idx += 1;
        }
        let total_blocks = blocks.len() as u32;

        let frame_w = (modules_w * module_pixels) as i32;
        let frame_h = (modules_h * module_pixels) as i32;
        let encoding_mode = EncodingMode::Binary;

        for block in &blocks {
            let seq = block.index as u16 + 1;
            let frame = Frame::new_data_merged(&[block], seq, total_blocks, 0).unwrap();
            let pixels = render_frame_default(
                &frame,
                module_pixels,
                modules_w,
                modules_h,
                &gray_config,
                false,
                None,
            );
            let locations = crate::decoder::locate::locate_frame_all(&pixels, frame_w, frame_h);
            assert!(
                !locations.is_empty(),
                "block_{}: no candidates",
                block.index
            );

            // 正确候选应在首位（v0.12 修复保证）
            let loc = &locations[0];
            assert_eq!(loc.modules_w, 640, "block_{}: modules_w", block.index);
            assert_eq!(loc.modules_h, 400, "block_{}: modules_h", block.index);

            let decoded = decode_frame(&pixels, frame_w, frame_h, loc, &encoding_mode)
                .expect("decode_frame must succeed");
            assert_eq!(decoded.frame_sequence, seq);
            assert!(!decoded.payload.is_empty());

            // 验证多块格式
            let count = u16::from_le_bytes([decoded.payload[0], decoded.payload[1]]);
            assert_eq!(count, 1);
            let dec_idx = u32::from_le_bytes(decoded.payload[2..6].try_into().unwrap());
            assert_eq!(dec_idx, block.index);
            let dlen = u16::from_le_bytes([decoded.payload[6], decoded.payload[7]]) as usize;
            assert_eq!(dlen, block.data.len());
            assert_eq!(&decoded.payload[8..8 + dlen], block.data.as_slice());
        }
    }
}
