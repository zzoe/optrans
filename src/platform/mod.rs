//! 平台抽象层
//!
//! 统一封装截屏、屏幕尺寸与 DPI 感知等平台相关能力：
//! - **win32**: Windows 实现（GDI BitBlt 截屏 + Win32 窗口），另有 FFI 与辅助函数
//! - **macos**: macOS 接收端实现（CoreGraphics 截屏，纯 C FFI，零 objc）
//!
//! # 像素契约（跨平台）
//!
//! 截屏函数返回 4 字节/像素、top-down（首行=屏幕顶部）的像素缓冲区。
//! 编码图案为灰度（R=G=B），解码端只取亮度，因此 BGRA/RGBA 通道序不敏感。

#[cfg(target_os = "windows")]
pub mod win32;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
compile_error!("optrans 仅支持 Windows 与 macOS");

use crate::common::error::OptransResult;

/// 截屏结果：(像素缓冲, 宽, 高)
pub type ScreenBuffer = (Vec<u8>, i32, i32);

/// 截取主显示器全屏（4 字节/像素，top-down）
pub fn capture_full_screen() -> OptransResult<ScreenBuffer> {
    #[cfg(target_os = "windows")]
    {
        win32::capture::capture_full_screen()
    }
    #[cfg(target_os = "macos")]
    {
        macos::capture::capture_full_screen()
    }
}

/// 截取整个虚拟桌面（所有显示器拼接区域，多显示器环境）
pub fn capture_virtual_desktop() -> OptransResult<ScreenBuffer> {
    #[cfg(target_os = "windows")]
    {
        win32::capture::capture_virtual_desktop()
    }
    #[cfg(target_os = "macos")]
    {
        macos::capture::capture_virtual_desktop()
    }
}

/// 是否存在多显示器（虚拟桌面尺寸/范围与主屏不同）
pub fn has_multiple_monitors() -> bool {
    #[cfg(target_os = "windows")]
    {
        win32::capture::has_multiple_monitors()
    }
    #[cfg(target_os = "macos")]
    {
        macos::capture::has_multiple_monitors()
    }
}

/// 主显示器尺寸（物理像素）
pub fn screen_size() -> (i32, i32) {
    #[cfg(target_os = "windows")]
    {
        win32::capture::screen_size()
    }
    #[cfg(target_os = "macos")]
    {
        macos::capture::screen_size()
    }
}

/// 声明进程 DPI 感知，使截屏/尺寸均为物理像素。返回 false 表示设置失败。
pub fn set_process_dpi_aware() -> bool {
    #[cfg(target_os = "windows")]
    {
        win32::helpers::set_process_dpi_aware()
    }
    #[cfg(target_os = "macos")]
    {
        // macOS CG API 天然以物理像素工作，无需 DPI 感知声明
        true
    }
}
