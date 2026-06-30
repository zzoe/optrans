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
pub const FRAME_INTERVAL_MS: u64 = 150;

/// 默认发送轮数
pub const DEFAULT_ROUNDS: u32 = 3;

/// EOT 帧重复次数
pub const EOT_REPEAT: u32 = 3;

/// 接收端超时（秒）
pub const RECEIVE_TIMEOUT_SECS: u64 = 60;

/// Finder Pattern 尺寸（模块）
pub const FINDER_SIZE: u32 = 7;

/// 格式信息区行数
pub const FORMAT_ROWS: u32 = 2;

/// 最大文件名长度（UTF-8 字节）
pub const MAX_FILE_NAME_LEN: usize = 255;

/// 格式信息原始字段位数
pub const FORMAT_INFO_BITS: usize = 97;

/// 格式信息打包字节数（97 bits → 13 bytes，最后一个字节高7位为0）
pub const FORMAT_INFO_BYTES: usize = 13;

/// CRC-16 追加后的字节数
pub const FORMAT_INFO_WITH_CRC: usize = 15;

/// 三重重复后的字节数
pub const FORMAT_INFO_ENCODED_BYTES: usize = 45;

/// 三重重复后的位数
pub const FORMAT_INFO_ENCODED_BITS: usize = 360;

/// 格式信息区可用位数（2 行 × (N-14) 模块，N=200 时为 372）
pub const FORMAT_INFO_AREA_BITS: usize = 372;

/// RGBA 像素通道数
pub const RGBA_CHANNELS: usize = 4;

/// CRC-16/XMODEM 多项式
pub const CRC16_POLY: u16 = 0x1021;

/// CRC-16 初始值
pub const CRC16_INIT: u16 = 0x0000;

/// BitBlt 光栅操作码：SRCCOPY（直接拷贝）
pub const SRCCOPY: u32 = 0x00CC0020;

/// 截屏默认起始 X 坐标
pub const CAPTURE_X_DEFAULT: i32 = 0;

/// 截屏默认起始 Y 坐标
pub const CAPTURE_Y_DEFAULT: i32 = 0;

/// 发送端编码图案四周的空白边距（像素）。
/// 设为 0：finder pattern 自身的 7×7 黑色边框已提供视觉分隔，
/// 额外黑边会让接收端扫描时第一段 B 与 finder 边框合并，破坏 1:1:3:1:1 比例。
pub const FRAME_MARGIN_PX: u32 = 0;

// ─── v0.2 新增：灰度值表 ───

/// 2 级灰度值表（同 v0.1 二值）
pub const GRAY_2_VALUES: [u8; 2] = [0, 255];

/// 2 级解码阈值
pub const GRAY_2_THRESHOLDS: [u8; 1] = [128];

/// 4 级灰度值表
pub const GRAY_4_VALUES: [u8; 4] = [0, 85, 170, 255];

/// 4 级解码阈值
pub const GRAY_4_THRESHOLDS: [u8; 3] = [42, 127, 212];

/// 5 级灰度值表
pub const GRAY_5_VALUES: [u8; 5] = [0, 64, 128, 192, 255];

/// 5 级解码阈值（相邻灰度值的中点）
pub const GRAY_5_THRESHOLDS: [u8; 4] = [32, 96, 160, 224];

/// 8 级灰度值表
pub const GRAY_8_VALUES: [u8; 8] = [0, 36, 73, 109, 146, 182, 219, 255];

/// 8 级解码阈值
pub const GRAY_8_THRESHOLDS: [u8; 7] = [18, 54, 91, 127, 164, 200, 237];

/// 16 级灰度值表
pub const GRAY_16_VALUES: [u8; 16] = [
    0, 17, 34, 51, 68, 85, 102, 119, 136, 153, 170, 187, 204, 221, 238, 255,
];

/// 16 级解码阈值
pub const GRAY_16_THRESHOLDS: [u8; 15] = [
    8, 25, 42, 59, 76, 93, 110, 128, 144, 161, 178, 195, 212, 229, 246,
];

// ─── v0.2 新增：5 进制编码参数 ───

/// 5 进制编码组大小（源字节数）
pub const BASE5_CHUNK_SRC: usize = 2;

/// 5 进制编码组大小（模块数）
pub const BASE5_CHUNK_MOD: usize = 7;

/// 5⁷ = 78,125
pub const BASE5_MAX_VALUE: u32 = 78125;

// ─── v0.2 新增：差分编码参数 ───

/// 默认 I 帧间隔（连续 P 帧数达到此值后强制 I 帧）
pub const I_FRAME_INTERVAL_DEFAULT: u32 = 10;

/// P 帧退化为 I 帧的 diff 比例阈值
pub const P_FRAME_DIFF_RATIO_THRESHOLD: f64 = 0.80;

// ─── v0.2 新增：心跳参数 ───

/// 心跳像素位置（从右下角算起的模块坐标偏移）
pub const HEARTBEAT_MODULE_OFFSET: u32 = 3;

/// 心跳像素块尺寸（像素）
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

/// 默认灰度级数
pub const GRAY_LEVELS_DEFAULT: u32 = 5;

/// 有效的灰度级数列表
pub const VALID_GRAY_LEVELS: [u32; 5] = [2, 4, 5, 8, 16];

// ─── v0.2 新增：P 帧背景色 ───

/// P 帧背景色（中灰，RGB 各通道）
pub const P_FRAME_BG_R: u8 = 128;
pub const P_FRAME_BG_G: u8 = 128;
pub const P_FRAME_BG_B: u8 = 128;
