//! 统一错误类型定义
//!
//! 所有公共 API 返回 `OptransResult<T>`，禁止在生产路径中使用 `.unwrap()`。

use std::fmt;

/// 统一的操作结果类型别名
pub type OptransResult<T> = Result<T, OptransError>;

/// optrans 项目中所有可能的错误类型
#[derive(Debug)]
pub enum OptransError {
    /// IO 错误
    Io(std::io::Error),

    /// Win32 API 调用错误
    Win32(String),

    /// 帧解码错误
    FrameDecode(String),

    /// Reed-Solomon 解码错误
    RsDecode(String),

    /// SHA-256 校验和不匹配
    ChecksumMismatch {
        expected: String,
        actual: String,
    },

    /// 传输未完成（缺少某些块）
    IncompleteTransfer { missing: Vec<u32> },

    /// 格式信息校验失败
    InvalidFormatInfo(String),

    /// 未找到帧
    NoFrameFound,

    /// 接收超时
    Timeout,

    /// 无效参数
    InvalidArgument(String),

    /// 窗口创建失败
    WindowCreation(String),
}

impl fmt::Display for OptransError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OptransError::Io(e) => write!(f, "IO 错误: {}", e),
            OptransError::Win32(msg) => write!(f, "Win32 错误: {}", msg),
            OptransError::FrameDecode(msg) => write!(f, "帧解码错误: {}", msg),
            OptransError::RsDecode(msg) => write!(f, "RS 解码错误: {}", msg),
            OptransError::ChecksumMismatch { expected, actual } => {
                write!(f, "校验和不匹配: 期望 {}, 实际 {}", expected, actual)
            }
            OptransError::IncompleteTransfer { missing } => {
                write!(f, "传输未完成，缺失块: {:?}", missing)
            }
            OptransError::InvalidFormatInfo(msg) => {
                write!(f, "格式信息无效: {}", msg)
            }
            OptransError::NoFrameFound => write!(f, "未在截屏中找到有效帧"),
            OptransError::Timeout => write!(f, "接收超时"),
            OptransError::InvalidArgument(msg) => write!(f, "无效参数: {}", msg),
            OptransError::WindowCreation(msg) => write!(f, "窗口创建失败: {}", msg),
        }
    }
}

impl std::error::Error for OptransError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            OptransError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for OptransError {
    fn from(e: std::io::Error) -> Self {
        OptransError::Io(e)
    }
}

impl From<&str> for OptransError {
    fn from(s: &str) -> Self {
        OptransError::FrameDecode(s.to_string())
    }
}
