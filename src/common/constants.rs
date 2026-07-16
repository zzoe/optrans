//! 跨模块共享常量
//!
//! 仅收录被多个模块引用的协议/调参常量。单模块使用的常量应就地定义在对应模块中，
//! 避免 constants 沦为"杂物间"——集中化的目的是共享，不是强制收纳。

/// 默认每模块像素数（正方形）
pub const MODULE_PIXELS_DEFAULT: u32 = 4;

/// RGBA 像素通道数
pub(crate) const RGBA_CHANNELS: usize = 4;

/// 默认帧间隔（毫秒）
pub const FRAME_INTERVAL_MS: u64 = 100;

/// 发送端编码图案四周的空白边距（像素）。
///
/// 默认 20px：提供边缘分离，帮助接收端定位时区分屏幕边缘与帧边界。
/// 接收端 `locate_by_corners` 使用相同值计算 finder pattern 的预期位置。
pub const FRAME_MARGIN_PX_DEFAULT: u32 = 20;

/// 默认灰度级数（16 = 最高密度，帧数最少 → 漏帧风险最低）
pub const GRAY_LEVELS_DEFAULT: u32 = 16;

// ─── 帧内 RS(255,217) 编码参数 ───

/// RS 编码：数据码字数量
pub(crate) const RS_DATA_LEN: usize = 217;

/// RS 编码：校验码字数量
pub(crate) const RS_PARITY_LEN: usize = 38;

/// RS 编码：总码字数量
pub(crate) const RS_TOTAL_LEN: usize = 255;

// ─── 块级 FEC（前向纠错）参数 ───
//
// 远程桌面（Horizon/Citrix）协议层会因带宽自适应、变化检测合并等机制丢帧，
// sender 无法干预。采用块级 RS 纠删码做容错：每组 k 个数据块 + m 个校验块，
// 接收端收到任意 k 个分片即可恢复该组全部数据块。
//
// 实测 Horizon 漏帧率约 28%，取 k=128 m=64（容错 33%）留充足余量。
// RS(255,k) 上限 k+m ≤ 255，192 远低于上限，编译期校验。

/// FEC 每组数据分片数
pub(crate) const FEC_DATA_SHARDS: usize = 128;

/// FEC 每组校验分片数
pub(crate) const FEC_PARITY_SHARDS: usize = 64;
