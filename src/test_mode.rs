//! 最小化测试模式
//!
//! 用于调试端到端传输，验证 finder pattern 检测是否正常工作。
//!
//! # 简化策略
//! - **二值模式**（gray_levels=2）：不用灰度编码、不用中值采样
//! - **单帧**：元数据帧，payload = "HELLO OPTRANS TEST"
//! - **无握手**：发送端显示 10 秒后退出
//! - **无 EOT**：接收端收到第一帧就打印并退出
//! - **保留 finder pattern**：与完整版相同的 finder + 格式信息链路
//! - v0.7: 使用全屏扫描定位（不依赖固定角落位置，支持任意窗口尺寸/缩放）

use std::time::{Duration, Instant};

use crate::common::error::{OptransError, OptransResult};
use crate::common::types::{FrameKind, FormatInfo, FrameLocation, GrayEncodingConfig};
use crate::encoder::frame::Frame;
use crate::encoder::reed_solomon::RsEncoder;
use crate::encoder::render::render_frame_default;
use crate::receiver::capture::capture_full_screen;
use crate::sender::window::SenderWindow;

/// 从 PNG 图片文件加载 RGBA 像素数据
fn load_png_rgba(path: &str) -> OptransResult<(Vec<u8>, i32, i32)> {
    let img = image::open(path)
        .map_err(|e| OptransError::Win32(format!("加载图片失败 {}: {}", path, e)))?;
    let rgba = img.to_rgba8();
    let w = rgba.width() as i32;
    let h = rgba.height() as i32;
    Ok((rgba.into_raw(), w, h))
}

/// 发送测试帧（持续显示，Ctrl+C 退出）
///
/// 1. 根据窗口尺寸计算每帧最大 payload
/// 2. 用 "HELLO OPTRANS TEST\n" 重复填充到满帧
/// 3. 二值模式 RS(255,217) 编码 → 渲染
/// 4. 创建全屏长方形窗口，持续显示直到窗口关闭或 Ctrl+C
pub fn test_send() -> OptransResult<()> {
    let module_pixels: u32 = 4;
    let gray_config = GrayEncodingConfig::new(2); // 2 级 = 二值

    // 1. 创建全屏窗口，获取模块尺寸
    let mut window = SenderWindow::create(module_pixels)?;
    let modules_w = window.modules_per_side_w;
    let modules_h = window.modules_per_side_h;

    // 2. 计算此帧尺寸下能容纳的最大 payload（二值模式）
    let max_payload = calculate_max_payload_binary(modules_w, modules_h);

    // 3. 用测试消息重复填充到满帧
    let template = b"HELLO OPTRANS TEST\n";
    let mut payload = Vec::with_capacity(max_payload);
    while payload.len() + template.len() <= max_payload {
        payload.extend_from_slice(template);
    }
    // 剩余字节用模板开头填充
    let remaining = max_payload - payload.len();
    if remaining > 0 {
        payload.extend_from_slice(&template[..remaining.min(template.len())]);
    }

    eprintln!(
        "[test] 发送端：测试帧 payload={}B (模板 \"HELLO OPTRANS TEST\\n\" 重复 {}次), 帧{}w×{}h",
        payload.len(),
        payload.len() / template.len(),
        modules_w, modules_h
    );

    // 4. 创建 FormatInfo（Meta 帧，seq=0）
    let info = FormatInfo::new_meta(0, 1, payload.len() as u16);

    // 5. RS 编码
    let encoder = RsEncoder::new();
    let encoded_payload = encoder.encode(&payload)?;

    eprintln!(
        "[test] 发送端：RS 编码完成 (payload {}B → {}B)",
        payload.len(),
        encoded_payload.len()
    );

    // 6. 构建 Frame
    let frame = Frame {
        format_info: info,
        encoded_payload,
        frame_kind: FrameKind::I,
    };

    // 7. 渲染
    let pixels = render_frame_default(&frame, module_pixels, modules_w, modules_h, &gray_config, false, None);

    let img_w = modules_w as usize * module_pixels as usize;
    let img_h = modules_h as usize * module_pixels as usize;
    eprintln!(
        "[test] 发送端：渲染完成 ({}×{} RGBA = {} bytes)",
        img_w, img_h, pixels.len()
    );

    window.update_bitmap(&pixels);

    eprintln!(
        "[test] 发送端：已显示测试帧 — pattern {}×{} ({}px边距), modules={}w×{}h",
        img_w, img_h,
        crate::common::constants::FRAME_MARGIN_PX,
        modules_w, modules_h
    );

    // 8. 持续显示 3 秒后自动退出（或窗口关闭/Ctrl+C 提前退出）
    let start_tick = unsafe { crate::win32::ffi::GetTickCount() };
    loop {
        if !window.process_messages() {
            eprintln!("[test] 发送端：窗口已关闭，退出");
            return Ok(());
        }
        let elapsed = unsafe { crate::win32::ffi::GetTickCount() } - start_tick;
        if elapsed >= 3000 {
            eprintln!("[test] 发送端：3 秒已到，自动退出");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 计算二值模式下帧能容纳的最大 payload 字节数
fn calculate_max_payload_binary(modules_w: u32, modules_h: u32) -> usize {
    use crate::common::constants::{RS_DATA_LEN, RS_TOTAL_LEN};

    let total = (modules_w * modules_h) as usize;
    let finder = 3 * 49;
    let format = 2 * (modules_w as usize - 14);
    let available = total - finder - format;

    // 二值：8 模块 = 1 字节（RS 编码后）
    let max_rs_encoded = available / 8;

    // RS: 4B header + ceil(payload/217) × 255
    let header = 4;
    if max_rs_encoded <= header {
        return 0;
    }
    let num_blocks = (max_rs_encoded - header) / RS_TOTAL_LEN;
    let max_payload = num_blocks * RS_DATA_LEN;

    eprintln!(
        "[test] 帧容量: {}w×{}h → {} 可用数据模块 → {}B RS编码 → {}B payload ({}个RS块×{})",
        modules_w, modules_h, available, max_rs_encoded, max_payload, num_blocks, RS_DATA_LEN
    );

    max_payload
}

/// 接收测试帧（含诊断日志）
///
/// 1. 全屏截取 → locate_frame_with_diag（全屏扫描） → decode_frame
/// 2. 如果解码成功，打印 payload 文本
/// 3. 失败时打印亮度诊断，每 10 轮保存全屏 BMP
/// 4. Ctrl+C 退出
///
/// v0.7: 改用全屏扫描定位，不依赖固定角落位置，
/// 支持 Horizon client 等任意窗口尺寸的远程桌面场景。
pub fn test_receive() -> OptransResult<()> {
    eprintln!("[test] 接收端：开始全屏扫描定位...");
    eprintln!("[test] 接收端：将在首次截屏时保存 capture_debug.bmp 用于诊断");

    let mut scan_count: u64 = 0;
    let mut debug_saved = false;
    let mut last_err_time = Instant::now();
    let mut last_err_count: u64 = 0;

    loop {
        scan_count += 1;

        // 1. 全屏截取
        let (pixels, img_w, img_h) = match capture_full_screen() {
            Ok(result) => result,
            Err(e) => {
                eprintln!("[test] 接收端：截屏失败: {}，重试...", e);
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };

        // 首次截屏打印尺寸并保存全屏 BMP + 全图亮度诊断
        if !debug_saved {
            debug_saved = true;

            // 全图采样：每隔 N 像素取一次亮度，统计是否有非白像素
            let step: usize = 200; // 每 200 像素采样一个
            let w = img_w as usize;
            let mut min_lum: u8 = 255;
            let mut max_lum: u8 = 0;
            let mut white_count: u64 = 0;
            let mut black_count: u64 = 0;
            let mut gray_count: u64 = 0;
            let mut total_samples: u64 = 0;

            for y in (0..img_h as usize).step_by(step) {
                for x in (0..w).step_by(step) {
                    let idx = (y * w + x) * 4;
                    if idx + 2 >= pixels.len() { continue; }
                    let r = pixels[idx] as u32;
                    let g = pixels[idx+1] as u32;
                    let b = pixels[idx+2] as u32;
                    let lum = ((r * 299 + g * 587 + b * 114) / 1000) as u8;
                    min_lum = min_lum.min(lum);
                    max_lum = max_lum.max(lum);
                    if lum < 50 { black_count += 1; }
                    else if lum > 200 { white_count += 1; }
                    else { gray_count += 1; }
                    total_samples += 1;
                }
            }

            eprintln!(
                "[test] 接收端：截屏尺寸 {}×{}",
                img_w, img_h
            );
            eprintln!(
                "[test] 接收端：全图采样({}个), 亮度范围=[{},{}], 黑(<50)={}, 白(>200)={}, 灰={}",
                total_samples, min_lum, max_lum, black_count, white_count, gray_count
            );
            if black_count == 0 && gray_count == 0 {
                eprintln!("[test] 接收端：⚠ 全图纯白！截屏未捕获到任何窗口内容。");
                eprintln!("[test] 接收端：⚠ 可能原因: (1) test send 未启动 (2) RDP 会话桌面 DC 不可 BitBlt");
            } else {
                eprintln!("[test] 接收端：✓ 全图有非白内容，发送窗口可能已显示在桌面");
            }

            save_bmp("capture_debug.bmp", &pixels, img_w, img_h);
            eprintln!(
                "[test] 接收端：已保存全屏截屏到 capture_debug.bmp ({}×{})",
                img_w, img_h
            );
        }

        // 2. 全屏扫描定位帧
        match crate::decoder::locate::locate_frame_with_diag(&pixels, img_w, img_h) {
            Some((loc, diag)) => {
                eprintln!(
                    "[test] 接收端：全屏扫描成功！tl=({:.0},{:.0}), tr=({:.0},{:.0}), bl=({:.0},{:.0}), modules=({}w×{}h), ms={:.2} (候选{}, 验证{})",
                    loc.tl.center_x, loc.tl.center_y,
                    loc.tr.center_x, loc.tr.center_y,
                    loc.bl.center_x, loc.bl.center_y,
                    loc.modules_w, loc.modules_h,
                    loc.tl.module_size,
                    diag.candidates_found, diag.candidates_verified
                );

                match decode_test_data_direct(&pixels, img_w, img_h, &loc) {
                    Ok(data) => {
                        let text = String::from_utf8_lossy(&data);
                        eprintln!("[test] 接收端：解码成功！payload = {:?}", text);
                        return Ok(());
                    }
                    Err(e) => {
                        let now = Instant::now();
                        let elapsed = now.duration_since(last_err_time);
                        if elapsed >= Duration::from_secs(2) {
                            let total_errs = last_err_count + 1;
                            eprintln!(
                                "[test] 接收端：定位成功但解码失败(共{}次): {}，继续扫描...",
                                total_errs, e
                            );
                            last_err_time = now;
                            last_err_count = 0;
                        } else {
                            last_err_count += 1;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                }
            }
            None => {
                if scan_count % 10 == 0 {
                    eprintln!(
                        "[test] 接收端：未找到帧 (已扫描 {} 次)，全屏扫描未能定位 finder pattern",
                        scan_count
                    );
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// 测试模式：直接从数据矩阵提取模块值（跳过格式信息）
///
/// 因为收发两端的屏幕分辨率不匹配时，格式信息的模块数会不一致，
/// 导致格式信息解码失败。此函数直接按 finder pattern 定位的模块
/// 网格采样黑白像素，不依赖格式信息。
///
/// v0.8: 改用仿射变换采样（与 extract_data_modules/sample_module 一致），
/// 基于全部 3 个 finder 中心点推算像素坐标，消除行列级联漂移。
fn decode_test_data_direct(
    pixels: &[u8], img_w: i32, img_h: i32, loc: &FrameLocation,
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
            let lum = (pixels[idx] as u32 * 299 + pixels[idx + 1] as u32 * 587 + pixels[idx + 2] as u32 * 114) / 1000;
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

    let total_len = u32::from_le_bytes([rs_encoded[0], rs_encoded[1], rs_encoded[2], rs_encoded[3]]) as usize;

    if total_len == 0 || total_len > rs_encoded.len() * 2 {
        return Err(OptransError::FrameDecode(format!("无效的数据长度: {}", total_len)));
    }

    let rs_data = &rs_encoded[4..];
    let num_blocks = (total_len + 216) / 217; // RS_DATA_LEN-1
    let expected_rs_bytes = num_blocks * 255;

    if rs_data.len() < expected_rs_bytes {
        return Err(OptransError::FrameDecode(format!("RS 数据不足: 需要 {}B, 实际 {}B", expected_rs_bytes, rs_data.len())));
    }

    use crate::decoder::reed_solomon::rs_decode_data;
    rs_decode_data(&rs_encoded[..4 + expected_rs_bytes.min(rs_encoded.len())])
}

/// 从 PNG 图片文件解码测试帧（离线调试用）
///
/// 加载 PNG → 全屏扫描定位 finder pattern → 解码 payload
pub fn test_decode_image(path: &str) -> OptransResult<()> {
    eprintln!("[test] 解码图片：加载 {}", path);
    let (pixels, img_w, img_h) = load_png_rgba(path)?;
    eprintln!("[test] 图片尺寸 {}×{}", img_w, img_h);

    // 全图扫描定位（不依赖固定角落位置，支持帧在图片任意位置）
    eprintln!("[test] 尝试全屏扫描 locate_frame ...");
    let loc = match crate::decoder::locate::locate_frame(&pixels, img_w, img_h) {
        Some(loc) => {
            eprintln!(
                "[test] 全屏扫描成功！tl=({:.0},{:.0}), tr=({:.0},{:.0}), bl=({:.0},{:.0}), modules=({}w×{}h), module_size={:.2}",
                loc.tl.center_x, loc.tl.center_y,
                loc.tr.center_x, loc.tr.center_y,
                loc.bl.center_x, loc.bl.center_y,
                loc.modules_w, loc.modules_h,
                loc.tl.module_size
            );
            loc
        }
        None => {
            eprintln!("[test] 定位失败！图片中未找到 finder pattern");
            return Err(OptransError::FrameDecode("定位失败".into()));
        }
    };

    // 解码
    match decode_test_data_direct(&pixels, img_w, img_h, &loc) {
        Ok(data) => {
            let text = String::from_utf8_lossy(&data);
            eprintln!("[test] 解码成功！payload = {:?}", text);
            Ok(())
        }
        Err(e) => {
            eprintln!("[test] 解码失败: {}", e);
            Err(e)
        }
    }
}

/// 保存 RGBA 像素为 BMP 文件
fn save_bmp(path: &str, rgba: &[u8], w: i32, h: i32) {
    use std::io::Write;
    let w_u = w as u32;
    let h_u = h as u32;
    // 每行像素字节数（BGR, 4 字节对齐）
    let row_bytes = (w_u * 3 + 3) & !3;
    let img_size = row_bytes * h_u;
    let file_size = 54 + img_size;

    let mut buf = Vec::with_capacity(file_size as usize);
    // BMP 文件头 (14 bytes)
    buf.extend_from_slice(b"BM");
    buf.extend_from_slice(&file_size.to_le_bytes());
    buf.extend_from_slice(&[0u8; 4]); // reserved
    buf.extend_from_slice(&54u32.to_le_bytes()); // data offset
    // DIB 头 (40 bytes)
    buf.extend_from_slice(&40u32.to_le_bytes()); // header size
    buf.extend_from_slice(&w_u.to_le_bytes());
    buf.extend_from_slice(&h_u.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // planes
    buf.extend_from_slice(&24u16.to_le_bytes()); // bpp
    buf.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    buf.extend_from_slice(&img_size.to_le_bytes());
    buf.extend_from_slice(&[0u8; 16]); // resolution + palette

    // 像素数据 (BMP 是 BGR, 自底向上)
    for y in (0..h).rev() {
        let mut row = Vec::with_capacity(row_bytes as usize);
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            row.push(rgba[idx + 2]); // B
            row.push(rgba[idx + 1]); // G
            row.push(rgba[idx]);     // R
        }
        while row.len() < row_bytes as usize {
            row.push(0);
        }
        buf.extend_from_slice(&row);
    }

    if let Ok(mut f) = std::fs::File::create(path) {
        f.write_all(&buf).ok();
    }
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
            &frame, module_pixels, modules_w, modules_h, &gray_config, false, None,
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

        // 4. 全屏扫描定位
        let loc = crate::decoder::locate::locate_frame(&screen, screen_w, screen_h)
            .expect("全屏扫描应能定位帧");
        eprintln!(
            "定位成功: tl=({:.0},{:.0}), modules=({}w×{}h), module_size={:.2}",
            loc.tl.center_x, loc.tl.center_y, loc.modules_w, loc.modules_h, loc.tl.module_size
        );

        // 5. 解码
        let data = decode_test_data_direct(&screen, screen_w, screen_h, &loc)
            .expect("解码应成功");
        assert_eq!(data, payload);
        eprintln!("解码成功: payload = {:?}", String::from_utf8_lossy(&data));
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
            &frame, module_pixels, modules_w, modules_h, &gray_config, false, None,
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

        // 4. 全屏扫描定位
        let loc = crate::decoder::locate::locate_frame(&screen, screen_w, screen_h)
            .expect("全屏扫描应能定位帧");

        // 5. 使用正式接收端的 decode_frame 解码（覆盖 extract.rs 的采样路径）
        let decoded = decode_frame(
            &screen, screen_w, screen_h, &loc, &EncodingMode::Auto,
        )
            .expect("decode_frame 应成功");

        assert_eq!(decoded.payload, payload);
        eprintln!(
            "decode_frame 解码成功: payload = {:?}",
            String::from_utf8_lossy(&decoded.payload)
        );
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

        eprintln!("满帧测试: payload={}B, RSblocks={}", payload.len(), num_blocks);

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
            &frame, module_pixels, modules_w, modules_h, &gray_config, false, None,
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

        // 5. 全屏扫描定位
        let loc = crate::decoder::locate::locate_frame(&screen, screen_w, screen_h)
            .expect("满帧测试：全屏扫描应能定位帧");
        eprintln!(
            "满帧定位: tl=({:.0},{:.0}), modules=({}w×{}h), ms={:.2}",
            loc.tl.center_x, loc.tl.center_y,
            loc.modules_w, loc.modules_h, loc.tl.module_size
        );

        // 6. decode_test_data_direct 解码
        let decoded = decode_test_data_direct(&screen, screen_w, screen_h, &loc)
            .expect("满帧测试：decode_test_data_direct 应成功");

        assert_eq!(decoded, payload, "满帧测试：解码结果应与原始 payload 一致");
        eprintln!(
            "满帧测试通过: payload={}B 完整往返 (前80字节: {:?})",
            payload.len(),
            String::from_utf8_lossy(&decoded[..80.min(decoded.len())])
        );
    }
}
