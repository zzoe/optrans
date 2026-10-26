//! 离线诊断：加载真实全屏截图 1.png，跑接收端定位+解码管线，输出每步诊断。

use optrans::decoder::decode::{decode_format_info, decode_format_info_with_geometry, decode_frame};
use optrans::decoder::locate::locate_frame_all_with_diag;
use optrans::common::types::EncodingMode;

fn load_png(path: &str) -> (Vec<u8>, i32, i32) {
    let data = std::fs::read(path).expect("读取 png 失败");
    let decoder = png::Decoder::new(std::io::Cursor::new(data));
    let mut reader = decoder.read_info().expect("png 解码失败");
    let (iw, ih) = {
        let i = reader.info();
        (i.width as usize, i.height as usize)
    };
    let mut buf = vec![0u8; iw * ih * 4];
    let info = reader.next_frame(&mut buf).expect("png 帧解码失败");
    let w = info.width as i32;
    let h = info.height as i32;
    let pixels = match info.color_type {
        png::ColorType::Rgb => {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for px in buf[..(w * h * 3) as usize].chunks(3) {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
            out
        }
        png::ColorType::Rgba => buf[..(w * h * 4) as usize].to_vec(),
        png::ColorType::Grayscale => {
            let mut out = Vec::with_capacity((w * h * 4) as usize);
            for &g in &buf[..(w * h) as usize] {
                out.extend_from_slice(&[g, g, g, 255]);
            }
            out
        }
        other => panic!("不支持的 png 颜色类型: {:?}", other),
    };
    (pixels, w, h)
}

/// 打印指定行的游程（run-length）序列，观察 finder 1:1:3:1:1 结构
fn dump_runs_h(pixels: &[u8], w: i32, y: i32, x0: i32, x1: i32) {
    let mut runs: Vec<(u8, i32)> = Vec::new();
    let mut prev = 255u8;
    let mut len = 0i32;
    for x in x0..x1 {
        let idx = (y as usize * w as usize + x as usize) * 4;
        let lum = (pixels[idx] as u32 * 299 + pixels[idx + 1] as u32 * 587 + pixels[idx + 2] as u32 * 114) / 1000;
        let bit = if lum < 128 { 1u8 } else { 0u8 };
        if bit == prev {
            len += 1;
        } else {
            runs.push((prev, len));
            prev = bit;
            len = 1;
        }
    }
    runs.push((prev, len));
    let s: Vec<String> = runs.iter().map(|(b, l)| format!("{}{}", if *b == 1 { 'B' } else { 'W' }, l)).collect();
    eprintln!("[runs] y={}: {}", y, s.join(" "));
}

/// 打印指定列的游程序列
fn dump_runs_v(pixels: &[u8], w: i32, x: i32, y0: i32, y1: i32) {
    let mut runs: Vec<(u8, i32)> = Vec::new();
    let mut prev = 255u8;
    let mut len = 0i32;
    for y in y0..y1 {
        let idx = (y as usize * w as usize + x as usize) * 4;
        let lum = (pixels[idx] as u32 * 299 + pixels[idx + 1] as u32 * 587 + pixels[idx + 2] as u32 * 114) / 1000;
        let bit = if lum < 128 { 1u8 } else { 0u8 };
        if bit == prev {
            len += 1;
        } else {
            runs.push((prev, len));
            prev = bit;
            len = 1;
        }
    }
    runs.push((prev, len));
    let s: Vec<String> = runs.iter().map(|(b, l)| format!("{}{}", if *b == 1 { 'B' } else { 'W' }, l)).collect();
    eprintln!("[runv] x={}: {}", x, s.join(" "));
}

/// 粗粒度 ASCII 地图：每 block 像素采样一次，# = 黑，. = 白
#[test]
fn diag_ascii_map() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/1.png");
    if !std::path::Path::new(path).exists() {
        return;
    }
    let (pixels, w, h) = load_png(path);
    eprintln!("[map] {}x{}", w, h);
    let block = 32;
    let mut y = 0;
    while y < h {
        let mut line = String::new();
        let mut x = 0;
        while x < w {
            let idx = (y as usize * w as usize + x as usize) * 4;
            let lum = (pixels[idx] as u32 * 299 + pixels[idx + 1] as u32 * 587 + pixels[idx + 2] as u32 * 114) / 1000;
            line.push(if lum < 128 { '#' } else { '.' });
            x += block;
        }
        eprintln!("[map y={:4}] {}", y, line);
        y += block;
    }
}

/// 纹理地图：每 64px 块统计黑像素占比，输出灰度分级
/// 帧区域（黑白棋盘）应呈现 30-70% 混合；纯黑=窗口背景，纯白=空白
#[test]
fn diag_texture_map() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/1.png");
    if !std::path::Path::new(path).exists() {
        return;
    }
    let (pixels, w, h) = load_png(path);
    eprintln!("[tex] {}x{} 每格=64px 字符=黑像素占比(0:' ' 1-24:'.' 25-49:':' 50-74:'+' 75-99:'*' 100:'#')", w, h);
    let block = 64;
    let mut y = 0;
    while y < h {
        let mut line = String::new();
        let mut x = 0;
        while x < w {
            let mut black = 0u32;
            let mut total = 0u32;
            let mut yy = y;
            while yy < (y + block).min(h) {
                let mut xx = x;
                while xx < (x + block).min(w) {
                    let idx = (yy as usize * w as usize + xx as usize) * 4;
                    let lum = (pixels[idx] as u32 * 299 + pixels[idx + 1] as u32 * 587 + pixels[idx + 2] as u32 * 114) / 1000;
                    if lum < 128 { black += 1; }
                    total += 1;
                    xx += 4;
                }
                yy += 4;
            }
            let pct = black * 100 / total.max(1);
            line.push(match pct {
                0 => ' ',
                1..=24 => '.',
                25..=49 => ':',
                50..=74 => '+',
                75..=99 => '*',
                _ => '#',
            });
            x += block;
        }
        eprintln!("[tex y={:4}] {}", y, line);
        y += block;
    }
}

#[test]
fn diag_pixel_structure() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/1.png");
    if !std::path::Path::new(path).exists() {
        return;
    }
    let (pixels, w, h) = load_png(path);
    eprintln!("[diag] 尺寸 {}x{}", w, h);
    // TL finder 区域行游程
    for y in [90, 100, 110, 117, 125, 135, 145, 160] {
        dump_runs_h(&pixels, w, y, 0, 320);
    }
    // TL finder 区域列游程
    for x in [28, 40, 50, 60, 80] {
        dump_runs_v(&pixels, w, x, 60, 320);
    }
}

#[test]
fn diag_real_screenshot() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/1.png");
    if !std::path::Path::new(path).exists() {
        eprintln!("[diag] 1.png 不存在，跳过");
        return;
    }
    let (pixels, w, h) = load_png(path);
    eprintln!("[diag] 截图尺寸: {}x{}", w, h);

    let cands = locate_frame_all_with_diag(&pixels, w, h);
    eprintln!("[diag] 候选数: {}", cands.len());
    for (i, (loc, diag)) in cands.iter().enumerate().take(8) {
        eprintln!(
            "[diag] cand{}: tl=({:.1},{:.1}) tr=({:.1},{:.1}) bl=({:.1},{:.1}) ms={:.2} modules={}x{} found={} verified={}",
            i, loc.tl.center_x, loc.tl.center_y, loc.tr.center_x, loc.tr.center_y,
            loc.bl.center_x, loc.bl.center_y, loc.tl.module_size, loc.modules_w, loc.modules_h,
            diag.candidates_found, diag.candidates_verified
        );
        match decode_format_info_with_geometry(&pixels, w, h, loc) {
            Some((info, refined)) => {
                eprintln!(
                    "[diag]   几何精修通过: {}x{} → {}x{} fmt: type={:?} seq={} block={} total={} payload_bytes={}",
                    loc.modules_w, loc.modules_h, refined.modules_w, refined.modules_h,
                    info.frame_type, info.frame_sequence, info.block_index,
                    info.total_blocks, info.payload_bytes
                );
                match decode_frame(&pixels, w, h, &refined, &EncodingMode::Auto) {
                    Ok(f) => eprintln!("[diag]   payload 解码成功 len={}", f.payload.len()),
                    Err(e) => eprintln!("[diag]   payload 解码失败: {}", e),
                }
            }
            None => eprintln!("[diag]   fmt+几何精修均失败（原始: {}）",
                decode_format_info(&pixels, w, h, loc).err().map(|e| e.to_string()).unwrap_or_default()),
        }
    }
}
