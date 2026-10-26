//! Win32 FFI 模块
//!
//! 手工 FFI 声明与辅助函数，不依赖外部 Windows crate：
//! - **ffi**: Win32 API 绑定（窗口创建、GDI、截屏等）
//! - **helpers**: Win32 辅助函数（字符串转换、错误处理、HDC 检查）

pub mod ffi;
pub mod helpers;
