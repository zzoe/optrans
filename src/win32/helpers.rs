//! Win32 辅助函数
//!
//! 提供宽字符串转换、错误处理等工具函数。

use crate::win32::ffi;

/// 将 Rust 字符串（UTF-8）转换为 Windows 宽字符串（UTF-16 LE）
///
/// 返回以 null 结尾的 `Vec<u16>`，可直接传递给 Win32 FFI 函数。
pub fn to_wide(s: &str) -> Vec<u16> {
    let mut wide: Vec<u16> = s.encode_utf16().collect();
    wide.push(0); // null 终止符
    wide
}

/// 从 Windows 宽字符串指针（UTF-16）转换为 Rust String
///
/// # Safety
///
/// 调用者确保 `ptr` 指向有效的以 null 结尾的 UTF-16 字符串。
pub unsafe fn from_wide(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let len = {
        let mut len = 0usize;
        while unsafe { *ptr.add(len) } != 0 {
            len += 1;
        }
        len
    };
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf16_lossy(slice)
}

/// 获取最后一次 Win32 错误的可读信息
///
/// 调用 `GetLastError()` 并通过 `FormatMessageW` 获取错误描述。
pub fn last_error() -> String {
    let err_code = unsafe { ffi::GetLastError() };
    if err_code == 0 {
        return "未知错误 (错误码 0)".to_string();
    }

    let mut buf: *mut u16 = std::ptr::null_mut();
    let len = unsafe {
        ffi::FormatMessageW(
            ffi::FORMAT_MESSAGE_ALLOCATE_BUFFER
                | ffi::FORMAT_MESSAGE_FROM_SYSTEM
                | ffi::FORMAT_MESSAGE_IGNORE_INSERTS,
            std::ptr::null(),
            err_code,
            ffi::MAKELANGID(ffi::LANG_NEUTRAL, ffi::SUBLANG_DEFAULT),
            &mut buf as *mut _ as *mut u16,
            0,
            std::ptr::null(),
        )
    };

    if len == 0 || buf.is_null() {
        return format!("Win32 错误码: {}", err_code);
    }

    let msg = unsafe { from_wide(buf) };
    // 释放 FormatMessageW 分配的缓冲区
    if !buf.is_null() {
        unsafe {
            // 使用 kernel32 的 LocalFree
            // 这里简化处理：FormatMessageW 用 LocalAlloc 分配的内存
            // 在 Windows 上通过一个外部函数释放
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn LocalFree(hMem: isize) -> isize;
            }
            LocalFree(buf as isize);
        }
    }

    format!("[{}] {}", err_code, msg.trim())
}

/// 检查窗口句柄是否有效
///
/// 返回 `Ok(hwnd)` 如果句柄非空，否则返回错误。
pub fn check_hwnd(hwnd: ffi::HWND) -> crate::common::error::OptransResult<ffi::HWND> {
    if hwnd == ffi::NULL as ffi::HWND {
        return Err(crate::common::error::OptransError::WindowCreation(
            "窗口句柄为空".to_string(),
        ));
    }
    Ok(hwnd)
}

/// 检查 HDC 是否有效
pub fn check_hdc(hdc: ffi::HDC) -> crate::common::error::OptransResult<ffi::HDC> {
    if hdc == ffi::NULL as ffi::HDC {
        return Err(crate::common::error::OptransError::Win32(format!(
            "获取 DC 失败: {}",
            last_error()
        )));
    }
    Ok(hdc)
}
