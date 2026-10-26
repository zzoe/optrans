//! macOS 平台实现（仅接收端）
//!
//! - **ffi**: CoreGraphics/CoreFoundation 纯 C FFI 绑定（零 objc）
//! - **capture**: CGDisplayCreateImage 屏幕截取（单屏 + 多屏合成）

pub mod ffi;
pub mod capture;
