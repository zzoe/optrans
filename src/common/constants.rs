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
/// payload_bytes 字段为 21 bits，单块最大可达 2,097,151 字节。
/// 取 32768 (32KB) 作为默认块大小（`read_file` 使用）：
/// - 实际发送时 block_size 由帧容量动态计算（`read_file_with_block_size`）
/// - RS 编码：32772 字节 → 152 个 RS 块 (152×217=32984 ≥ 32772)，冗余率 17%
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

/// 格式信息打包字节数（104 bits → 13 bytes）
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

// ─── 心跳参数 ───
pub const HEARTBEAT_PIXEL_SIZE: u32 = 2;

/// 心跳变化幅度
pub const HEARTBEAT_DELTA: u8 = 1;

/// 心跳基准灰度值
pub const HEARTBEAT_BASE_GRAY: u8 = 128;

// ─── 采样参数 ───

/// 二值模式采样半径（3×3，radius=1）
pub const SAMPLE_RADIUS_BINARY: i32 = 1;

/// 默认灰度级数（16 = 最高密度，帧数最少 → 漏帧风险最低）
pub const GRAY_LEVELS_DEFAULT: u32 = 16;

/// 有效的灰度级数（2 的幂，bit 直映射）
pub const VALID_GRAY_LEVELS: [u32; 4] = [2, 4, 8, 16];

// ─── 块级 FEC（前向纠错）参数 ───
//
// 远程桌面（Horizon/Citrix）协议层会因带宽自适应、变化检测合并等机制丢帧，
// sender 无法干预。采用块级 RS 纠删码做容错：每组 k 个数据块 + m 个校验块，
// 接收端收到任意 k 个分片即可恢复该组全部数据块。
//
// 实测 Horizon 漏帧率约 28%，取 k=128 m=64（容错 33%）留充足余量。
// RS(255,k) 上限 k+m ≤ 255，192 远低于上限，编译期校验。

/// FEC 每组数据分片数
pub const FEC_DATA_SHARDS: usize = 128;

/// FEC 每组校验分片数
pub const FEC_PARITY_SHARDS: usize = 64;

/// FEC 每组总分片数
pub const FEC_TOTAL_SHARDS: usize = FEC_DATA_SHARDS + FEC_PARITY_SHARDS;
