//! 全局常量定义
//!
//! 所有魔法数字集中在此文件中，禁止在其他文件中定义硬编码常量。

/// 默认每模块像素数（正方形）
pub const MODULE_PIXELS_DEFAULT: u32 = 4;

/// 最小模块像素数
pub const MODULE_PIXELS_MIN: u32 = 2;

/// 最大模块像素数
pub const MODULE_PIXELS_MAX: u32 = 8;

/// 默认每边模块数
pub const MODULES_PER_SIDE_DEFAULT: u32 = 200;

/// 文件分块大小（字节）
///
/// v0.6: payload_bytes 字段扩展到 16 bits 后，单块最大可达 65535 字节。
/// 取 32768 (32KB) 作为默认块大小：
/// - payload_bytes = 4(block_index) + 32768 = 32772 ≤ 65535 ✓
/// - RS 编码：32772 字节 → 152 个 RS 块 (152×217=32984 ≥ 32772)，冗余率 17%
/// - 标准 2560×1600 全屏 module_pixels=4 时单帧容量约 27KB（二值）或更大（灰度）
///   32768 可能超过二值模式单帧容量，但灰度模式可容纳
/// - 动态帧尺寸会自动调整窗口大小以容纳数据
/// - 注意：小窗口场景（如 Horizon client 窗口较小时）单帧容量可能不足，
///   动态帧尺寸计算会限制在屏幕容量内
pub const BLOCK_SIZE: usize = 32768;

/// RS 编码：数据码字数量
pub const RS_DATA_LEN: usize = 217;

/// RS 编码：校验码字数量
pub const RS_PARITY_LEN: usize = 38;

/// RS 编码：总码字数量
pub const RS_TOTAL_LEN: usize = 255;

/// 默认帧间隔（毫秒）
pub const FRAME_INTERVAL_MS: u64 = 100;

/// 最大文件名长度（UTF-8 字节）
pub const MAX_FILE_NAME_LEN: usize = 255;

/// 格式信息打包字节数（97 bits → 13 bytes，最后一个字节高7位为0）
pub const FORMAT_INFO_BYTES: usize = 13;

/// CRC-16 追加后的字节数
pub const FORMAT_INFO_WITH_CRC: usize = 15;

/// RGBA 像素通道数
pub const RGBA_CHANNELS: usize = 4;

/// BitBlt 光栅操作码：SRCCOPY（直接拷贝）
pub const SRCCOPY: u32 = 0x00CC0020;

/// 发送端编码图案四周的空白边距（像素）。
///
/// 默认 20px：提供少量边缘分离，避免 finder 紧贴屏幕边缘时因亚像素
/// 抖动导致的边界效应。远程桌面场景（Horizon/Citrix）需通过 --frame-margin 增大。
pub const FRAME_MARGIN_PX_DEFAULT: u32 = 20;

// ─── v0.2 新增：差分编码参数 ───

/// 默认 I 帧间隔（连续 P 帧数达到此值后强制 I 帧）
pub const I_FRAME_INTERVAL_DEFAULT: u32 = 10;

// ─── v0.2 新增：心跳参数 ───
pub const HEARTBEAT_PIXEL_SIZE: u32 = 2;

/// 心跳变化幅度
pub const HEARTBEAT_DELTA: u8 = 1;

/// 心跳基准灰度值
pub const HEARTBEAT_BASE_GRAY: u8 = 128;

// ─── v0.2 新增：采样参数 ───

/// 二值模式采样半径（3×3，radius=1）
pub const SAMPLE_RADIUS_BINARY: i32 = 1;

/// 灰度模式采样半径（5×5，radius=2）
pub const SAMPLE_RADIUS_GRAY: i32 = 2;

/// 默认灰度级数（2 = 二值，最稳定；4+ = 灰度，待调通）
pub const GRAY_LEVELS_DEFAULT: u32 = 2;

/// 有效的灰度级数列表
pub const VALID_GRAY_LEVELS: [u32; 5] = [2, 4, 5, 8, 16];
