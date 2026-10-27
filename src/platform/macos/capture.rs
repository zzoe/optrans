//! CoreGraphics 屏幕截取模块（macOS）
//!
//! 通过 `CGDisplayCreateImage` 截取显示器内容，转为 4 字节/像素、top-down
//! 的像素缓冲区（跨平台像素契约，见 `crate::platform` 模块文档）。
//!
//! # 屏幕录制权限（macOS 10.15+）
//!
//! 无权限时 `CGDisplayCreateImage` 返回的是壁纸/纯色而非真实屏幕内容。
//! 启动截屏前用 `CGPreflightScreenCaptureAccess` 预检并给出明确指引。
//!
//! # 多显示器
//!
//! 不使用已废弃的 `CGWindowListCreateImage` 家族。逐屏 `CGDisplayCreateImage`
//! 后按各屏独立缩放比例换算全局像素偏移，合成到一块虚拟桌面缓冲。
//! 不同缩放比例（Retina/非 Retina 混接）屏间的接缝可能有 1~2 像素误差，
//! 但每屏内部像素精确，不影响帧定位与解码。

use log::warn;

use crate::common::error::{OptransError, OptransResult};
use crate::platform::macos::ffi;

/// CGGetActiveDisplayList 单次最多枚举的显示器数
const MAX_DISPLAYS: u32 = 16;

/// CGImageRef RAII 守卫（CFType，需 CGImageRelease 释放）
struct ImageGuard(ffi::CGImageRef);

impl Drop for ImageGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { ffi::CGImageRelease(self.0) };
        }
    }
}

/// 屏幕录制权限预检：无权限时告警并触发一次系统授权弹窗
fn check_screen_recording_permission() {
    if !unsafe { ffi::CGPreflightScreenCaptureAccess() } {
        warn!(
            "缺少屏幕录制权限，截屏结果可能只是壁纸/纯色。\
             请在 系统设置 → 隐私与安全性 → 屏幕录制 中勾选当前终端/宿主应用后重启接收端"
        );
        // 触发一次系统授权弹窗（用户同意后需重启进程生效）
        unsafe { ffi::CGRequestScreenCaptureAccess() };
    }
}

/// 采样检查像素缓冲是否近乎纯色（无权限截屏的典型特征）
fn is_near_uniform(pixels: &[u8]) -> bool {
    let mut min_lum = 255u32;
    let mut max_lum = 0u32;
    for chunk in pixels.chunks(400) {
        if chunk.len() >= 3 {
            let lum = (chunk[0] as u32 * 299 + chunk[1] as u32 * 587 + chunk[2] as u32 * 114) / 1000;
            min_lum = min_lum.min(lum);
            max_lum = max_lum.max(lum);
            if max_lum - min_lum > 16 {
                return false;
            }
        }
    }
    true
}

/// 将 CGImage 转为 top-down 4 字节/像素缓冲区
///
/// CGBitmapContext 的内存首行天然对应图像顶行：用户坐标系原点在左下角，
/// identity CTM 下把整幅图像绘入 (0,0,w,h) 正好让图像顶部落在内存首行。
/// 因此**不需要**额外翻转 CTM。
///
/// 旧实现额外做了 `Translate(0,h) + Scale(1,-1)`，反而把缓冲变成 bottom-up，
/// 接收端看到上下颠倒的屏幕（实测：mac 菜单栏出现在导出截图的底部、文字倒立），
/// finder 与格式信息全部镜像 → 永不定位成功。
fn cgimage_to_topdown(img: ffi::CGImageRef) -> OptransResult<(Vec<u8>, i32, i32)> {
    let w = unsafe { ffi::CGImageGetWidth(img) };
    let h = unsafe { ffi::CGImageGetHeight(img) };
    if w == 0 || h == 0 {
        return Err(OptransError::Platform("CGImage 尺寸为 0".to_string()));
    }

    let bytes_per_row = w * 4;
    let mut buf = vec![0u8; bytes_per_row * h];

    let cs = unsafe { ffi::CGColorSpaceCreateDeviceRGB() };
    if cs.is_null() {
        return Err(OptransError::Platform("CGColorSpaceCreateDeviceRGB 失败".to_string()));
    }
    let ctx = unsafe {
        ffi::CGBitmapContextCreate(
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            w,
            h,
            8,
            bytes_per_row,
            cs,
            ffi::kCGImageAlphaNoneSkipFirst | ffi::kCGBitmapByteOrder32Little,
        )
    };
    if ctx.is_null() {
        unsafe { ffi::CGColorSpaceRelease(cs) };
        return Err(OptransError::Platform("CGBitmapContextCreate 失败".to_string()));
    }

    unsafe {
        ffi::CGContextDrawImage(
            ctx,
            ffi::CGRect {
                origin: ffi::CGPoint { x: 0.0, y: 0.0 },
                size: ffi::CGSize { width: w as f64, height: h as f64 },
            },
            img,
        );
        ffi::CFRelease(ctx as ffi::CFTypeRef);
        ffi::CGColorSpaceRelease(cs);
    }

    Ok((buf, w as i32, h as i32))
}

/// 枚举活跃显示器
fn active_displays() -> Vec<ffi::CGDirectDisplayID> {
    let mut ids = [0u32; MAX_DISPLAYS as usize];
    let mut count = 0u32;
    let err = unsafe { ffi::CGGetActiveDisplayList(MAX_DISPLAYS, ids.as_mut_ptr(), &mut count) };
    if err != 0 {
        return Vec::new();
    }
    ids[..count as usize].to_vec()
}

/// 主显示器尺寸（物理像素）
pub fn screen_size() -> (i32, i32) {
    let display = unsafe { ffi::CGMainDisplayID() };
    let w = unsafe { ffi::CGDisplayPixelsWide(display) };
    let h = unsafe { ffi::CGDisplayPixelsHigh(display) };
    (w as i32, h as i32)
}

/// 是否存在多显示器
pub fn has_multiple_monitors() -> bool {
    active_displays().len() > 1
}

/// 截取主显示器全屏（4 字节/像素，top-down）
pub fn capture_full_screen() -> OptransResult<(Vec<u8>, i32, i32)> {
    check_screen_recording_permission();

    let display = unsafe { ffi::CGMainDisplayID() };
    let img_ptr = unsafe { ffi::CGDisplayCreateImage(display) };
    if img_ptr.is_null() {
        return Err(OptransError::Platform(
            "CGDisplayCreateImage 失败（检查屏幕录制权限）".to_string(),
        ));
    }
    let img = ImageGuard(img_ptr);

    let result = cgimage_to_topdown(img.0)?;
    if is_near_uniform(&result.0) {
        warn!("截屏内容近乎纯色，可能是缺少屏幕录制权限或屏幕被纯色窗口完全覆盖");
    }
    Ok(result)
}

/// 截取整个虚拟桌面（所有显示器拼接区域，top-down）
///
/// 各屏按自身像素分辨率截取，偏移用各屏独立缩放比例
/// （pixels / bounds.point）换算；y 偏移自虚拟桌面顶部起算，
/// 与 top-down 缓冲契约一致。
pub fn capture_virtual_desktop() -> OptransResult<(Vec<u8>, i32, i32)> {
    check_screen_recording_permission();

    let displays = active_displays();
    if displays.len() <= 1 {
        return capture_full_screen();
    }

    // 各屏信息：(图像, 像素宽, 像素高, x偏移px, y偏移px)
    struct Placed {
        img: ImageGuard,
        w: usize,
        h: usize,
        off_x: usize,
        off_y: usize,
    }

    let bounds: Vec<ffi::CGRect> = displays
        .iter()
        .map(|d| unsafe { ffi::CGDisplayBounds(*d) })
        .collect();

    let min_x_pt = bounds.iter().map(|b| b.origin.x).fold(f64::INFINITY, f64::min);
    let top_pt = bounds
        .iter()
        .map(|b| b.origin.y + b.size.height)
        .fold(f64::NEG_INFINITY, f64::max);

    let mut placed: Vec<Placed> = Vec::new();
    for (display, b) in displays.iter().zip(&bounds) {
        let img_ptr = unsafe { ffi::CGDisplayCreateImage(*display) };
        if img_ptr.is_null() {
            warn!("显示器 {} 截屏失败（CGDisplayCreateImage 返回空），跳过", display);
            continue;
        }
        let w = unsafe { ffi::CGImageGetWidth(img_ptr) };
        let h = unsafe { ffi::CGImageGetHeight(img_ptr) };
        if w == 0 || h == 0 {
            unsafe { ffi::CGImageRelease(img_ptr) };
            continue;
        }
        // 每屏独立缩放（Retina 2x / 非 Retina 1x 混接）
        let scale_x = w as f64 / b.size.width.max(1.0);
        let scale_y = h as f64 / b.size.height.max(1.0);
        let off_x = ((b.origin.x - min_x_pt) * scale_x).round().max(0.0) as usize;
        let off_y = ((top_pt - (b.origin.y + b.size.height)) * scale_y).round().max(0.0) as usize;
        placed.push(Placed { img: ImageGuard(img_ptr), w, h, off_x, off_y });
    }

    if placed.is_empty() {
        return Err(OptransError::Platform("所有显示器截屏均失败".to_string()));
    }
    if placed.len() == 1 {
        let p = placed.pop().expect("placed 非空");
        return cgimage_to_topdown(p.img.0);
    }

    // 虚拟桌面像素画布（top-down：y 自顶部起算）
    let canvas_w = placed.iter().map(|p| p.off_x + p.w).max().unwrap_or(0);
    let canvas_h = placed.iter().map(|p| p.off_y + p.h).max().unwrap_or(0);
    if canvas_w == 0 || canvas_h == 0 {
        return Err(OptransError::Platform("虚拟桌面像素尺寸为 0".to_string()));
    }

    let bytes_per_row = canvas_w * 4;
    let mut buf = vec![0u8; bytes_per_row * canvas_h];
    let cs = unsafe { ffi::CGColorSpaceCreateDeviceRGB() };
    let ctx = unsafe {
        ffi::CGBitmapContextCreate(
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            canvas_w,
            canvas_h,
            8,
            bytes_per_row,
            cs,
            ffi::kCGImageAlphaNoneSkipFirst | ffi::kCGBitmapByteOrder32Little,
        )
    };
    if ctx.is_null() {
        unsafe { ffi::CGColorSpaceRelease(cs) };
        return Err(OptransError::Platform("虚拟桌面 CGBitmapContextCreate 失败".to_string()));
    }

    unsafe {
        // 不翻转 CTM（理由同 cgimage_to_topdown）。缓冲是 top-down，
        // 而用户坐标 y 轴向上，故把「距顶部 off_y」换算成用户坐标原点：
        // y_user = canvas_h - off_y - h。
        for p in &placed {
            let y_user = canvas_h as f64 - p.off_y as f64 - p.h as f64;
            ffi::CGContextDrawImage(
                ctx,
                ffi::CGRect {
                    origin: ffi::CGPoint {
                        x: p.off_x as f64,
                        y: y_user,
                    },
                    size: ffi::CGSize {
                        width: p.w as f64,
                        height: p.h as f64,
                    },
                },
                p.img.0,
            );
        }
        ffi::CFRelease(ctx as ffi::CFTypeRef);
        ffi::CGColorSpaceRelease(cs);
    }

    Ok((buf, canvas_w as i32, canvas_h as i32))
}
