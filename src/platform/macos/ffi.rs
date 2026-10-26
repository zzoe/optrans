//! CoreGraphics / CoreFoundation FFI 声明（macOS）
//!
//! 手工 `extern "C"` 声明，不依赖外部 crate、不使用 objc 消息发送。
//! 仅覆盖接收端截屏所需的 C API。

#![allow(non_snake_case)]
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(dead_code)]

use std::ffi::c_void;

// ============================================================================
// 基础类型
// ============================================================================

/// 显示器 ID
pub type CGDirectDisplayID = u32;
/// CoreGraphics 错误码（kCGErrorSuccess = 0）
pub type CGError = i32;
/// 双精度浮点坐标
pub type CGFloat = f64;
/// CGImage 引用（不透明指针）
pub type CGImageRef = *mut c_void;
/// CGBitmapContext 引用（不透明指针）
pub type CGContextRef = *mut c_void;
/// CGColorSpace 引用（不透明指针）
pub type CGColorSpaceRef = *mut c_void;
/// CFTypeRef 通用引用
pub type CFTypeRef = *const c_void;

// ============================================================================
// 几何结构体
// ============================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CGPoint {
    pub x: CGFloat,
    pub y: CGFloat,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CGSize {
    pub width: CGFloat,
    pub height: CGFloat,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

// ============================================================================
// 常量
// ============================================================================

/// 位图信息：alpha 通道不使用但保留字节（BGRX）
pub const kCGImageAlphaNoneSkipFirst: u32 = 5;
/// 位图字节序：小端 32 位（内存布局 B,G,R,X）
pub const kCGBitmapByteOrder32Little: u32 = 2 << 12;

// ============================================================================
// 外部函数
// ============================================================================

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    // 显示器枚举与尺寸
    pub fn CGMainDisplayID() -> CGDirectDisplayID;
    pub fn CGGetActiveDisplayList(
        max_displays: u32,
        active_displays: *mut CGDirectDisplayID,
        display_count: *mut u32,
    ) -> CGError;
    pub fn CGDisplayPixelsWide(display: CGDirectDisplayID) -> usize;
    pub fn CGDisplayPixelsHigh(display: CGDirectDisplayID) -> usize;
    pub fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;

    // 截屏
    pub fn CGDisplayCreateImage(display: CGDirectDisplayID) -> CGImageRef;

    // CGImage 属性与释放
    pub fn CGImageGetWidth(image: CGImageRef) -> usize;
    pub fn CGImageGetHeight(image: CGImageRef) -> usize;
    pub fn CGImageRelease(image: CGImageRef);

    // 位图上下文（像素格式转换）
    pub fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: CGColorSpaceRef,
        bitmap_info: u32,
    ) -> CGContextRef;
    pub fn CGBitmapContextGetData(ctx: CGContextRef) -> *mut c_void;
    pub fn CGContextDrawImage(ctx: CGContextRef, rect: CGRect, image: CGImageRef);
    pub fn CGContextTranslateCTM(ctx: CGContextRef, tx: CGFloat, ty: CGFloat);
    pub fn CGContextScaleCTM(ctx: CGContextRef, sx: CGFloat, sy: CGFloat);
    pub fn CGContextClearRect(ctx: CGContextRef, rect: CGRect);
    pub fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
    pub fn CGColorSpaceRelease(space: CGColorSpaceRef);

    // 屏幕录制权限（macOS 10.15+，纯 C API）
    pub fn CGPreflightScreenCaptureAccess() -> bool;
    pub fn CGRequestScreenCaptureAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub fn CFRelease(cf: CFTypeRef);
}
