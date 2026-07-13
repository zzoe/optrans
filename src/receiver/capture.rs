//! GDI BitBlt 屏幕截取模块
//!
//! 通过 Win32 GDI 从屏幕截取指定区域为 RGBA 像素缓冲区。
//!
//! # RDP 截屏策略
//!
//! 采用多级回退策略以兼容各种 RDP 实现（微软 RDP、Horizon、Citrix 等）：
//! 1. `GetWindowDC(GetDesktopWindow())` — 桌面窗口 DC，包含所有子窗口内容
//! 2. `GetDC(NULL)` — 经典屏幕 DC，微软 RDP 下稳定
//! 3. `GetDC(NULL)` + `CAPTUREBLT` — 含分层窗口（实验性）

use crate::common::constants::SRCCOPY;
use crate::common::error::{OptransError, OptransResult};
use crate::win32::ffi;
use crate::win32::helpers::{check_hdc, last_error};

use log::warn;

/// CAPTUREBLT 光栅操作：BitBlt 时包含所有通过分层窗口覆盖的内容
const CAPTUREBLT: u32 = 0x40000000;

/// 执行一次 BitBlt 截屏（内部辅助）
fn do_capture(
    src_dc: ffi::HDC,
    x: i32, y: i32, w: i32, h: i32,
    rop: u32,
) -> OptransResult<(Vec<u8>, i32, i32)> {
    let mem_dc = unsafe { ffi::CreateCompatibleDC(src_dc) };
    if mem_dc == ffi::NULL as ffi::HDC {
        return Err(OptransError::Win32(format!("CreateCompatibleDC 失败: {}", last_error())));
    }
    let bitmap = unsafe { ffi::CreateCompatibleBitmap(src_dc, w, h) };
    if bitmap == ffi::NULL as ffi::HBITMAP {
        unsafe { ffi::DeleteDC(mem_dc) };
        return Err(OptransError::Win32(format!("CreateCompatibleBitmap 失败: {}", last_error())));
    }
    let old_bitmap = unsafe { ffi::SelectObject(mem_dc, bitmap as ffi::HGDIOBJ) };
    let result = unsafe { ffi::BitBlt(mem_dc, 0, 0, w, h, src_dc, x, y, rop) };
    if result == 0 {
        unsafe {
            ffi::SelectObject(mem_dc, old_bitmap);
            ffi::DeleteObject(bitmap as ffi::HGDIOBJ);
            ffi::DeleteDC(mem_dc);
        }
        return Err(OptransError::Win32(format!("BitBlt 失败: {}", last_error())));
    }

    let bmi_header = ffi::BITMAPINFOHEADER {
        biSize: std::mem::size_of::<ffi::BITMAPINFOHEADER>() as ffi::DWORD,
        biWidth: w,
        biHeight: -h,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: ffi::BI_RGB,
        ..unsafe { std::mem::zeroed() }
    };
    let bmi = ffi::BITMAPINFO { bmiHeader: bmi_header, bmiColors: [ffi::RGBQUAD { rgbBlue: 0, rgbGreen: 0, rgbRed: 0, rgbReserved: 0 }] };
    let pixels_size = (w as usize) * (h as usize) * 4;
    let mut pixels = vec![0u8; pixels_size];
    let scan_lines = unsafe {
        ffi::GetDIBits(mem_dc, bitmap, 0, h as ffi::UINT,
            pixels.as_mut_ptr() as *mut std::ffi::c_void,
            &bmi as *const ffi::BITMAPINFO as *mut ffi::BITMAPINFO,
            ffi::DIB_RGB_COLORS)
    };
    unsafe {
        ffi::SelectObject(mem_dc, old_bitmap);
        ffi::DeleteObject(bitmap as ffi::HGDIOBJ);
        ffi::DeleteDC(mem_dc);
    }
    if scan_lines == 0 {
        return Err(OptransError::Win32(format!("GetDIBits 失败: {}", last_error())));
    }
    Ok((pixels, w, h))
}

/// 检查 RGBA 像素缓冲区是否全部接近白色（采样检查，亮度 > 250）
fn is_all_white(pixels: &[u8]) -> bool {
    for chunk in pixels.chunks(400) {
        if chunk.len() >= 3 {
            let lum = (chunk[0] as u32 * 299 + chunk[1] as u32 * 587 + chunk[2] as u32 * 114) / 1000;
            if lum < 250 { return false; }
        }
    }
    true
}

/// 截取屏幕指定区域的 RGBA 像素（RDP 兼容，三级回退）
///
/// # 返回
/// `(pixels_rgba, width, height, method)` — method: 1=GetWindowDC, 2=GetDC, 3=CAPTUREBLT
pub fn capture_screen(x: i32, y: i32, w: i32, h: i32) -> OptransResult<(Vec<u8>, i32, i32, u32)> {
    // 策略 1: GetWindowDC(GetDesktopWindow()) — 桌面窗口 DC
    let desktop_hwnd = unsafe { ffi::GetDesktopWindow() };
    if desktop_hwnd != ffi::NULL as ffi::HWND {
        let dc = unsafe { ffi::GetWindowDC(desktop_hwnd) };
        if dc != ffi::NULL as ffi::HDC {
            let r = do_capture(dc, x, y, w, h, SRCCOPY);
            unsafe { ffi::ReleaseDC(desktop_hwnd, dc) };
            if let Ok(ref p) = r {
                if !is_all_white(&p.0) {
                    return Ok((p.0.clone(), p.1, p.2, 1));
                }
                warn!("策略1(GetWindowDC) 全白，尝试策略2");
            }
        }
    }

    // 策略 2: GetDC(NULL) — 经典桌面 DC
    let dc = unsafe { ffi::GetDC(ffi::NULL as ffi::HWND) };
    if dc != ffi::NULL as ffi::HDC {
        let r = do_capture(dc, x, y, w, h, SRCCOPY);
        unsafe { ffi::ReleaseDC(ffi::NULL as ffi::HWND, dc) };
        if let Ok(ref p) = r {
            if !is_all_white(&p.0) {
                return Ok((p.0.clone(), p.1, p.2, 2));
            }
            warn!("策略2(GetDC) 全白，尝试策略3(CAPTUREBLT)");
        }
    }

    // 策略 3: GetDC(NULL) + CAPTUREBLT — 包含分层窗口
    let dc = unsafe { ffi::GetDC(ffi::NULL as ffi::HWND) };
    check_hdc(dc)?;
    let r = do_capture(dc, x, y, w, h, SRCCOPY | CAPTUREBLT);
    unsafe { ffi::ReleaseDC(ffi::NULL as ffi::HWND, dc) };
    r.map(|p| (p.0, p.1, p.2, 3))
}

/// 截取整个屏幕的 RGBA 像素
pub fn capture_full_screen() -> OptransResult<(Vec<u8>, i32, i32)> {
    let screen_w = unsafe { ffi::GetSystemMetrics(ffi::SM_CXSCREEN) };
    let screen_h = unsafe { ffi::GetSystemMetrics(ffi::SM_CYSCREEN) };
    if screen_w <= 0 || screen_h <= 0 {
        return Err(OptransError::Win32(format!(
            "GetSystemMetrics 返回无效屏幕尺寸: {}×{}", screen_w, screen_h)));
    }
    let (pixels, w, h, _method) = capture_screen(0, 0, screen_w, screen_h)?;
    Ok((pixels, w, h))
}
