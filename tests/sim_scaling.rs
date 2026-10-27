//! 离线仿真：发送端渲染 Meta 帧 → 按不同缩放合成"截屏" → 跑接收端定位+解码管线。
//! 用于验证分辨率/缩放比例不一致是否导致接收端无法识别 Meta 帧。

use optrans::common::types::{EncodingMode, FrameType, GrayEncodingConfig, MetaPayload};
use optrans::decoder::decode::{
    decode_format_info, decode_frame, decode_meta_origin_sweep,
};
use optrans::decoder::locate::locate_frame_all_with_diag;
use optrans::encoder::frame::Frame;
use optrans::encoder::render::render_frame_default;

const MP: u32 = 4;
const MARGIN: usize = 20;

fn meta() -> MetaPayload {
    MetaPayload {
        file_name: "sim.bin".into(),
        file_size: 1234,
        sha256: [7u8; 32],
        total_blocks: 1,
        fec_k: 128,
        fec_m: 64,
        fec_groups: 1,
        fec_shard_size: 100,
    }
}

/// 发送端渲染（与 transmit.rs 相同的 Meta 二值渲染）
fn render_sender(modules_w: u32, modules_h: u32) -> Vec<u8> {
    let frame = Frame::new_metadata(&meta(), 0).unwrap();
    render_frame_default(&frame, MP, modules_w, modules_h, &GrayEncodingConfig::new(2).unwrap(), false)
}

/// 合成"截屏"：黑底屏幕，帧图案从 (MARGIN, MARGIN) 开始，按 (scale_x, scale_y) 最近邻缩放
fn compose_screen(
    frame_px: &[u8],
    frame_w: usize,
    frame_h: usize,
    screen_w: usize,
    screen_h: usize,
    scale_x: f64,
    scale_y: f64,
) -> Vec<u8> {
    let mut screen = vec![0u8; screen_w * screen_h * 4];
    let disp_w = ((frame_w as f64) * scale_x) as usize;
    let disp_h = ((frame_h as f64) * scale_y) as usize;
    for y in 0..disp_h.min(screen_h.saturating_sub(MARGIN)) {
        for x in 0..disp_w.min(screen_w.saturating_sub(MARGIN)) {
            let sx = ((x as f64) / scale_x) as usize;
            let sy = ((y as f64) / scale_y) as usize;
            if sx >= frame_w || sy >= frame_h {
                continue;
            }
            let si = (sy * frame_w + sx) * 4;
            let di = ((y + MARGIN) * screen_w + (x + MARGIN)) * 4;
            screen[di..di + 4].copy_from_slice(&frame_px[si..si + 4]);
        }
    }
    screen
}

/// 接收端管线：全屏扫描 → 逐候选解码格式信息 → Meta 判定
fn receiver_sees_meta(screen: &[u8], w: i32, h: i32) -> bool {
    receiver_sees_meta_diag(screen, w, h, false)
}

fn receiver_sees_meta_diag(screen: &[u8], w: i32, h: i32, verbose: bool) -> bool {
    let cands = locate_frame_all_with_diag(screen, w, h);
    if verbose {
        println!("[diag] candidates={}", cands.len());
    }
    for (i, (loc, diag)) in cands.iter().enumerate() {
        if verbose && i < 5 {
            println!(
                "[diag]   cand{}: tl=({:.1},{:.1}) tr=({:.1},{:.1}) bl=({:.1},{:.1}) ms={:.2} modules={}x{} found={} verified={}",
                i, loc.tl.center_x, loc.tl.center_y, loc.tr.center_x, loc.tr.center_y,
                loc.bl.center_x, loc.bl.center_y, loc.tl.module_size, loc.modules_w, loc.modules_h,
                diag.candidates_found, diag.candidates_verified
            );
        }
        match decode_format_info(screen, w, h, loc) {
            Ok(info) => {
                if info.frame_type == FrameType::Meta {
                    match decode_frame(screen, w, h, loc, &EncodingMode::Auto) {
                        Ok(f) if !f.payload.is_empty() => {
                            if verbose {
                                println!("[diag]   cand{} => META decoded", i);
                            }
                            return true;
                        }
                        Ok(_) => {
                            if verbose {
                                println!("[diag]   cand{} => META payload empty", i);
                            }
                        }
                        Err(e) => {
                            if verbose {
                                println!("[diag]   cand{} => META decode err: {}", i, e);
                            }
                        }
                    }
                } else if verbose && i < 5 {
                    println!("[diag]   cand{} => format ok but type={:?}", i, info.frame_type);
                }
            }
            Err(e) => {
                if verbose && i < 5 {
                    println!("[diag]   cand{} => format err: {}", i, e);
                }
            }
        }
    }
    false
}

/// 场景 runner：发送端屏幕 (sw×sh) → 接收端截屏 (sw×sh) 按 scale 缩放
fn scenario(name: &str, sw: usize, sh: usize, scale_x: f64, scale_y: f64, cap_w: usize, cap_h: usize) -> bool {
    let modules_w = ((sw - MARGIN * 2) / MP as usize) as u32;
    let modules_h = ((sh - MARGIN * 2) / MP as usize) as u32;
    let frame = render_sender(modules_w, modules_h);
    let frame_w = modules_w as usize * MP as usize;
    let frame_h = modules_h as usize * MP as usize;
    let screen = compose_screen(&frame, frame_w, frame_h, cap_w, cap_h, scale_x, scale_y);
    let ok = receiver_sees_meta(&screen, cap_w as i32, cap_h as i32);
    println!(
        "[sim] {name}: sender {sw}x{sh} modules {modules_w}x{modules_h}, capture {cap_w}x{cap_h} scale ({scale_x},{scale_y}) => meta_decoded={ok}"
    );
    ok
}

#[test]
fn sim_baseline_1to1() {
    // 两端分辨率一致（1:1）——必须能识别
    assert!(scenario("1:1", 2048, 1280, 1.0, 1.0, 2048, 1280));
}

#[test]
fn sim_current_env_vertical_09625() {
    // 当前环境：远程 2048x1280，Horizon 客户区 2048x1232 → 垂直 0.9625 非整数缩放
    assert!(scenario("vert0.9625", 2048, 1280, 1.0, 0.9625, 2048, 1232));
}

#[test]
fn sim_uniform_08() {
    // 统一 0.8 缩放（如 2560x1600 → 2048x1280 捕发送端图案）
    let modules_w = ((2560 - MARGIN * 2) / MP as usize) as u32;
    let modules_h = ((1600 - MARGIN * 2) / MP as usize) as u32;
    let frame = render_sender(modules_w, modules_h);
    let screen = compose_screen(
        &frame,
        modules_w as usize * MP as usize,
        modules_h as usize * MP as usize,
        2048, 1280, 0.8, 0.8,
    );
    let ok = receiver_sees_meta_diag(&screen, 2048, 1280, true);
    println!("[sim] uniform0.8 => meta_decoded={ok}");
    assert!(ok);
}

#[test]
fn sim_data_frame_nonuniform() {
    // 非均匀缩放（垂直 0.9625，当前环境 Horizon 客户区场景）+ 大数据帧（16 级灰度）。
    // 验证 modules_h 用垂直模块尺寸估算后，满帧数据采样网格对齐。
    use optrans::common::types::BlockData;

    let modules_w = ((2048 - MARGIN * 2) / MP as usize) as u32; // 502
    let modules_h = ((1280 - MARGIN * 2) / MP as usize) as u32; // 310

    let block = BlockData { index: 0, data: vec![0xA5u8; 40_000] };
    let frame = optrans::encoder::frame::Frame::new_data_merged(
        &[&block], 1, 1, 1, // seq=1, total_blocks=1, gray mode
    )
    .unwrap();
    let pixels = render_frame_default(
        &frame, MP, modules_w, modules_h, &GrayEncodingConfig::new(16).unwrap(), false,
    );

    let screen = compose_screen(
        &pixels,
        modules_w as usize * MP as usize,
        modules_h as usize * MP as usize,
        2048, 1232, 1.0, 0.9625,
    );

    let mut decoded_payload: Option<Vec<u8>> = None;
    for (i, (loc, _)) in locate_frame_all_with_diag(&screen, 2048, 1232).iter().enumerate() {
        let fmt = decode_format_info(&screen, 2048, 1232, loc);
        eprintln!(
            "[dbg] cand{}: modules={}x{} tl=({:.2},{:.2}) bl=({:.2},{:.2}) fmt={:?}",
            i, loc.modules_w, loc.modules_h,
            loc.tl.center_x, loc.tl.center_y, loc.bl.center_x, loc.bl.center_y,
            fmt.as_ref().map(|f| (f.frame_type, f.payload_bytes)).map_err(|e| e.to_string())
        );
        if let Ok(f) = decode_frame(&screen, 2048, 1232, loc, &EncodingMode::Auto) {
            if f.frame_type == FrameType::Data && !f.payload.is_empty() {
                eprintln!("[dbg] cand{} decoded data payload len={}", i, f.payload.len());
                decoded_payload = Some(f.payload);
                break;
            }
        }
    }
    let payload = decoded_payload.expect("数据帧应能解码");
    // 多块合并格式：count(2) + index(4) + len(4) + data
    assert_eq!(&payload[0..2], &1u16.to_le_bytes());
    assert_eq!(&payload[2..6], &0u32.to_le_bytes());
    assert_eq!(&payload[6..10], &(40_000u32).to_le_bytes());
    assert_eq!(&payload[10..10 + 40_000], &[0xA5u8; 40_000][..]);
}

#[test]
fn sim_uniform_125() {
    // 统一 1.25 缩放（整数模块 5px）——理论上可自适应
    assert!(scenario("uniform1.25", 2048, 1280, 1.25, 1.25, 2560, 1600));
}

/// 回归：发送端用 4 级灰度时，接收端 Auto 模式应自动识别级数
/// （格式信息只携带 1 bit 模式位，不含级数）。
#[test]
fn sim_gray4_auto_detect_levels() {
    use optrans::common::types::BlockData;

    let modules_w = 502u32;
    let modules_h = 304u32;
    let block = BlockData {
        index: 0,
        data: vec![0x5Au8; 20_000],
    };
    let frame = Frame::new_data_merged(&[&block], 1, 1, 1).unwrap();
    let pixels = render_frame_default(
        &frame,
        MP,
        modules_w,
        modules_h,
        &GrayEncodingConfig::new(4).unwrap(),
        false,
    );
    let screen = compose_screen(
        &pixels,
        modules_w as usize * MP as usize,
        modules_h as usize * MP as usize,
        2048, 1280, 1.0, 1.0,
    );

    let mut decoded = None;
    for (loc, _) in locate_frame_all_with_diag(&screen, 2048, 1280) {
        if let Ok(f) = decode_frame(&screen, 2048, 1280, &loc, &EncodingMode::Auto) {
            if f.frame_type == FrameType::Data && !f.payload.is_empty() {
                decoded = Some(f);
                break;
            }
        }
    }
    let f = decoded.expect("4 级灰度数据帧应能被 Auto 模式解码");
    assert_eq!(f.encoding_mode, EncodingMode::Gray(4), "应自动识别为 4 级灰度");
    assert_eq!(f.payload.len(), frame.format_info.payload_bytes as usize);
}

/// 回归：模式位被抹成 0 的 4 级灰度帧仍能解码。
///
/// 现场症状：远端视频编码把格式信息里的模式位量化成 0，接收端按二值解，
/// 直接报「需要 N modules，实际 99961 modules」——灰度帧永远进不了 RS 裁决。
/// Auto 必须在「容量不足」时回退到灰度级数重试。
#[test]
fn sim_gray4_with_binary_mode_bit_falls_back() {
    use optrans::common::types::BlockData;

    let modules_w = 502u32;
    let modules_h = 304u32;
    let block = BlockData {
        index: 0,
        data: vec![0xA5u8; 20_000],
    };
    // 故意写 encoding_mode_bit = 0（二值），但按 4 级灰度渲染
    let frame = Frame::new_data_merged(&[&block], 1, 1, 0).unwrap();
    assert_eq!(frame.format_info.encoding_mode_bit, 0);
    let pixels = render_frame_default(
        &frame,
        MP,
        modules_w,
        modules_h,
        &GrayEncodingConfig::new(4).unwrap(),
        false,
    );
    let screen = compose_screen(
        &pixels,
        modules_w as usize * MP as usize,
        modules_h as usize * MP as usize,
        2048, 1280, 1.0, 1.0,
    );

    let mut decoded = None;
    for (loc, _) in locate_frame_all_with_diag(&screen, 2048, 1280) {
        if let Ok(f) = decode_frame(&screen, 2048, 1280, &loc, &EncodingMode::Auto) {
            if f.frame_type == FrameType::Data && !f.payload.is_empty() {
                decoded = Some(f);
                break;
            }
        }
    }
    let f = decoded.expect("模式位为 0 的 4 级灰度帧应回退到灰度解码");
    assert_eq!(f.encoding_mode, EncodingMode::Gray(4));
    assert_eq!(f.payload.len(), frame.format_info.payload_bytes as usize);
}

/// 回归：链路把黑电平抬到 76（限制范围 YUV 未展开）时，4 级灰度仍能解码。
///
/// 现场症状：理想阈值（43/128/213）下最暗一档（76）> 43 被判成第二档，
/// 全帧整体错位，四个级数全部 RS 失败。自适应标定应把阈值拉回实测的
/// 簇心中点（≈106/166/225）。
#[test]
fn sim_gray4_with_lifted_black_level() {
    use optrans::common::types::BlockData;

    let modules_w = 502u32;
    let modules_h = 304u32;
    let block = BlockData {
        index: 0,
        data: vec![0x3Cu8; 20_000],
    };
    let frame = Frame::new_data_merged(&[&block], 1, 1, 1).unwrap();
    let mut pixels = render_frame_default(
        &frame,
        MP,
        modules_w,
        modules_h,
        &GrayEncodingConfig::new(4).unwrap(),
        false,
    );
    // 模拟远程桌面：黑 0 → 76，白 255 → 255，中间线性压缩
    for p in pixels.chunks_exact_mut(4) {
        for c in 0..3 {
            p[c] = (76.0 + p[c] as f64 * 179.0 / 255.0).round() as u8;
        }
    }
    let screen = compose_screen(
        &pixels,
        modules_w as usize * MP as usize,
        modules_h as usize * MP as usize,
        2048, 1280, 1.0, 1.0,
    );

    let mut decoded = None;
    for (loc, _) in locate_frame_all_with_diag(&screen, 2048, 1280) {
        if let Ok(f) = decode_frame(&screen, 2048, 1280, &loc, &EncodingMode::Auto) {
            if f.frame_type == FrameType::Data && !f.payload.is_empty() {
                decoded = Some(f);
                break;
            }
        }
    }
    let f = decoded.expect("黑电平抬升后 4 级灰度帧应能自适应标定解码");
    assert_eq!(f.encoding_mode, EncodingMode::Gray(4));
    assert_eq!(f.payload.len(), frame.format_info.payload_bytes as usize);
}

/// 确定性伪随机噪声（LCG），用于模拟远程桌面的像素级噪声
fn add_noise(pixels: &mut [u8], amp: i32, seed: u32) {
    let mut s = seed ^ 0x9E37_79B9;
    for p in pixels.chunks_exact_mut(4) {
        for c in 0..3 {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = ((s >> 16) as i32).rem_euclid(2 * amp + 1) - amp;
            p[c] = (p[c] as i32 + n).clamp(0, 255) as u8;
        }
    }
}

/// 回归：16 级灰度 + 像素级噪声（±10）仍能解码。
///
/// 16 级的级距只有 12（179 动态范围 / 15），单像素采样下 ±10 的噪声
/// 会让 ~40% 的模块判错，远超 RS(255,217) 的 19/255 纠错上限。
/// 采样改为模块内 3×3 均值后，噪声标准差降到 ~1/3，误判率落到 RS 能兜住的范围。
#[test]
fn sim_gray16_with_pixel_noise() {
    use optrans::common::types::BlockData;

    // 模块 4px（对应现场实测），采样窗口走 2×2
    let modules_w = 502u32;
    let modules_h = 304u32;
    let block = BlockData {
        index: 0,
        data: vec![0x6Du8; 40_000],
    };
    let frame = Frame::new_data_merged(&[&block], 1, 1, 1).unwrap();
    let mut pixels = render_frame_default(
        &frame,
        MP,
        modules_w,
        modules_h,
        &GrayEncodingConfig::new(16).unwrap(),
        false,
    );
    // 链路特征：黑电平抬到 76、白压到 255（级距 179/15 ≈ 12），
    // 再叠加 ±10 的像素噪声
    for p in pixels.chunks_exact_mut(4) {
        for c in 0..3 {
            p[c] = (76.0 + p[c] as f64 * 179.0 / 255.0).round() as u8;
        }
    }
    add_noise(&mut pixels, 10, 0xC0FF_EE01);
    let screen = compose_screen(
        &pixels,
        modules_w as usize * MP as usize,
        modules_h as usize * MP as usize,
        2048, 1280, 1.0, 1.0,
    );

    let mut decoded = None;
    for (loc, _) in locate_frame_all_with_diag(&screen, 2048, 1280) {
        if let Ok(f) = decode_frame(&screen, 2048, 1280, &loc, &EncodingMode::Auto) {
            if f.frame_type == FrameType::Data && !f.payload.is_empty() {
                decoded = Some(f);
                break;
            }
        }
    }
    let f = decoded.expect("16 级灰度帧在 ±10 像素噪声下应能解码");
    assert_eq!(f.encoding_mode, EncodingMode::Gray(16));
    assert_eq!(f.payload.len(), frame.format_info.payload_bytes as usize);
}

/// 回归：TL 中心测量偏低半个模块时，格式区（锚定 BL）仍可解码，
/// 但数据区（从帧顶部起算）全错 → 载荷 RS 失败。
/// 原点微扫应能找回载荷，并修正 finder 位置。
#[test]
fn sim_meta_origin_sweep_recovers_tl_offset() {
    let modules_w = 502u32;
    let modules_h = 304u32;
    let frame = Frame::new_metadata(&meta(), 0).unwrap();
    let pixels = render_frame_default(
        &frame,
        MP,
        modules_w,
        modules_h,
        &GrayEncodingConfig::new(2).unwrap(),
        false,
    );
    let screen = compose_screen(
        &pixels,
        modules_w as usize * MP as usize,
        modules_h as usize * MP as usize,
        2048, 1280, 1.0, 1.0,
    );

    let cands = locate_frame_all_with_diag(&screen, 2048, 1280);
    assert!(!cands.is_empty(), "应能定位到帧");
    let loc = cands[0].0.clone();

    // 模拟 TL 中心偏低 0.55 模块（BL 准确 → 格式区不受影响）
    let mut perturbed = loc.clone();
    perturbed.tl.center_y += 0.55 * loc.tl.module_size;

    assert!(
        decode_frame(&screen, 2048, 1280, &perturbed, &EncodingMode::Auto).is_err(),
        "TL 偏移后载荷应解不出"
    );
    let info = decode_format_info(&screen, 2048, 1280, &perturbed)
        .expect("格式区锚定 BL，偏移后仍应可解码");

    let (fixed, decoded) = decode_meta_origin_sweep(&screen, 2048, 1280, &perturbed, &info)
        .expect("原点微扫应找回载荷");
    assert_eq!(
        decoded.payload.len(),
        frame.format_info.payload_bytes as usize,
        "载荷长度应与发送端一致"
    );
    assert!(
        (fixed.tl.center_y - loc.tl.center_y).abs() < 0.25 * loc.tl.module_size,
        "微扫应把 TL 拉回真实位置附近"
    );
}
