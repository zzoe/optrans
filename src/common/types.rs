//! 共享数据类型定义
//!
//! 包含帧格式信息、元数据载荷、数据块、编码模式等核心数据结构。

/// 帧类型枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    /// 元数据帧：携带文件名、大小、SHA-256、总块数
    Meta = 0,
    /// 数据帧：携带文件数据块
    Data = 1,
    /// 传输结束帧：空载荷，表示发送完成
    EOT = 2,
    /// FEC 校验帧：携带块级 RS 纠删码的校验分片
    /// block_index 字段编码：高 16 位 = group_idx，低 16 位 = parity_idx
    FecParity = 3,
}

impl FrameType {
    /// 从 u8 值转换为 FrameType
    pub fn from_u8(v: u8) -> Option<FrameType> {
        match v {
            0 => Some(FrameType::Meta),
            1 => Some(FrameType::Data),
            2 => Some(FrameType::EOT),
            3 => Some(FrameType::FecParity),
            _ => None,
        }
    }
}

// ─── 编码模式 ───

/// 编码模式
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodingMode {
    /// 自动检测（从格式信息 reserved bit1 读取）
    Auto,
    /// 强制二值模式（0/255）
    Binary,
    /// 灰度模式（N 级灰度）
    Gray(u32),
}



// ─── 灰度编码配置 ───

/// 灰度编码配置
#[derive(Debug, Clone)]
pub struct GrayEncodingConfig {
    /// 灰度级数
    pub levels: u32,
    /// 灰度值表（levels 个值）
    pub gray_table: Vec<u8>,
    /// 解码阈值表（levels-1 个阈值）
    pub thresholds: Vec<u8>,
}

impl GrayEncodingConfig {
    /// 根据级数创建配置
    pub fn new(levels: u32) -> Self {
        let gray_table = Self::build_gray_table(levels);
        let thresholds = Self::build_thresholds(&gray_table);
        GrayEncodingConfig {
            levels,
            gray_table,
            thresholds,
        }
    }

    /// N 级等间距灰度值表
    pub fn build_gray_table(levels: u32) -> Vec<u8> {
        if levels == 2 {
            return vec![0, 255];
        }
        let step = 255.0 / (levels - 1) as f64;
        (0..levels)
            .map(|i| (i as f64 * step).round() as u8)
            .collect()
    }

    /// 解码阈值：相邻灰度值的中点
    pub fn build_thresholds(table: &[u8]) -> Vec<u8> {
        table
            .windows(2)
            .map(|w| ((w[0] as u16 + w[1] as u16) / 2) as u8)
            .collect()
    }

    /// 获取 bits-per-module（用于直映射编码）
    pub fn bits_per_module(&self) -> u32 {
        match self.levels {
            2 => 1,
            4 => 2,
            8 => 3,
            16 => 4,
            _ => 1,
        }
    }
}

/// 帧格式信息（104 bits 的逻辑表示）
///
/// 字段布局：
/// - frame_type:     2 bits  (bit 0-1)
/// - frame_sequence: 16 bits (bit 2-17)
/// - block_index:    32 bits (bit 18-49)
/// - total_blocks:   32 bits (bit 50-81)
/// - payload_bytes:  21 bits (bit 82-102)
///   (max 2,097,151，16 级灰度单帧可达 ~256KB)
/// - encoding_mode_bit: 1 bit (bit 103)
/// 总计：104 bits（13 bytes）
#[derive(Debug, Clone)]
pub struct FormatInfo {
    /// 帧类型：0=Meta, 1=Data, 2=EOT
    pub frame_type: FrameType,
    /// 帧序号，从 0 递增，接收端用于去重
    pub frame_sequence: u16,
    /// 数据块索引（元数据帧/EOT 帧填 0）
    pub block_index: u32,
    /// 总数据块数（仅元数据帧有效）
    pub total_blocks: u32,
    /// 载荷字节数（RS 编码前）
    pub payload_bytes: u32,
    /// 编码模式位：二值=0，灰度=1
    pub encoding_mode_bit: u8,
}

impl FormatInfo {
    /// 创建元数据帧的格式信息
    pub fn new_meta(seq: u16, total_blocks: u32, payload_bytes: u32) -> Self {
        FormatInfo {
            frame_type: FrameType::Meta,
            frame_sequence: seq,
            block_index: 0,
            total_blocks,
            payload_bytes,
            encoding_mode_bit: 0,
        }
    }

    /// 创建数据帧的格式信息
    pub fn new_data(
        seq: u16,
        block_index: u32,
        total_blocks: u32,
        payload_bytes: u32,
        encoding_mode_bit: u8,
    ) -> Self {
        FormatInfo {
            frame_type: FrameType::Data,
            frame_sequence: seq,
            block_index,
            total_blocks,
            payload_bytes,
            encoding_mode_bit,
        }
    }

    /// 创建数据帧的格式信息（便捷构造器：二值模式）
    pub fn new_data_binary(seq: u16, block_index: u32, total_blocks: u32, payload_bytes: u32) -> Self {
        Self::new_data(seq, block_index, total_blocks, payload_bytes, 0)
    }

    /// 创建 EOT 帧的格式信息
    pub fn new_eot(seq: u16) -> Self {
        FormatInfo {
            frame_type: FrameType::EOT,
            frame_sequence: seq,
            block_index: 0,
            total_blocks: 0,
            payload_bytes: 0,
            encoding_mode_bit: 0,
        }
    }

    /// 创建 FEC 校验帧的格式信息
    ///
    /// `block_index` 编码：高 16 位 = group_idx，低 16 位 = parity_idx
    pub fn new_parity(
        seq: u16,
        group_idx: u16,
        parity_idx: u16,
        total_blocks: u32,
        payload_bytes: u32,
        encoding_mode_bit: u8,
    ) -> Self {
        let block_index = ((group_idx as u32) << 16) | (parity_idx as u32);
        FormatInfo {
            frame_type: FrameType::FecParity,
            frame_sequence: seq,
            block_index,
            total_blocks,
            payload_bytes,
            encoding_mode_bit,
        }
    }
}

/// 从 FecParity 帧的 block_index 解出 (group_idx, parity_idx)
pub fn decode_parity_block_index(block_index: u32) -> (u16, u16) {
    (
        (block_index >> 16) as u16,
        (block_index & 0xFFFF) as u16,
    )
}

/// 元数据帧载荷（RS 编码前的原始数据）
#[derive(Debug, Clone)]
pub struct MetaPayload {
    /// 文件名（UTF-8 编码，最大 255 字节）
    pub file_name: String,
    /// 文件大小（字节）
    pub file_size: u64,
    /// 文件 SHA-256 哈希值
    pub sha256: [u8; 32],
    /// 总数据块数
    pub total_blocks: u32,
    /// FEC 每组数据分片数（0 表示未启用 FEC）
    pub fec_k: u16,
    /// FEC 每组校验分片数（0 表示未启用 FEC）
    pub fec_m: u16,
    /// FEC 组数（0 表示未启用 FEC）
    pub fec_groups: u32,
    /// FEC 分片字节长度（所有数据/校验分片等长，仅最后一块数据补零）
    pub fec_shard_size: u32,
}

impl MetaPayload {
    /// 是否启用 FEC
    pub fn fec_enabled(&self) -> bool {
        self.fec_k > 0 && self.fec_m > 0
    }
}

/// 单个文件数据块
#[derive(Debug, Clone)]
pub struct BlockData {
    /// 块索引，从 0 开始
    pub index: u32,
    /// 块数据（长度 ≤ block_size，block_size 由帧容量动态计算）
    pub data: Vec<u8>,
}

/// 文件清单（发送端文件读取结果）
#[derive(Debug, Clone)]
pub struct FileManifest {
    /// 文件路径
    pub path: std::path::PathBuf,
    /// 文件名（不含路径）
    pub file_name: String,
    /// 文件大小（字节）
    pub file_size: u64,
    /// SHA-256 哈希值
    pub sha256: [u8; 32],
    /// 数据块列表
    pub blocks: Vec<BlockData>,
}

/// Finder Pattern 检测结果
#[derive(Debug, Clone)]
pub struct FinderPattern {
    /// 中心坐标（像素，浮点精度）
    pub center_x: f64,
    pub center_y: f64,
    /// 估算的模块尺寸（像素）
    pub module_size: f64,
}

/// 帧定位结果：三个 Finder Pattern 在截屏中的像素坐标
///
/// v0.5: 支持长方形帧。`modules_w` 和 `modules_h` 分别为水平/垂直方向的模块数。
/// 向后兼容：`modules_per_side` 取 max(modules_w, modules_h)。
#[derive(Debug, Clone)]
pub struct FrameLocation {
    /// 左上角 Finder Pattern
    pub tl: FinderPattern,
    /// 右上角 Finder Pattern
    pub tr: FinderPattern,
    /// 左下角 Finder Pattern
    pub bl: FinderPattern,
    /// 每边模块数（向后兼容，= max(modules_w, modules_h)）
    pub modules_per_side: u32,
    /// v0.5: 水平方向模块数
    pub modules_w: u32,
    /// v0.5: 垂直方向模块数
    pub modules_h: u32,
    /// v0.20: 每模块像素数（由 send/receive 约定，非 locate 推算）
    pub module_pixels: u32,
}

/// 解码完成的帧
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    /// 帧类型
    pub frame_type: FrameType,
    /// 帧序号
    pub frame_sequence: u16,
    /// 数据块索引
    pub block_index: u32,
    /// 总数据块数（仅元数据帧有效）
    pub total_blocks: u32,
    /// 解码后的载荷数据（RS 解码后，不含 block_index 前缀）
    pub payload: Vec<u8>,
    /// 编码模式
    pub encoding_mode: EncodingMode,
}

/// 帧摄入结果
#[derive(Debug)]
pub enum IngestResult {
    /// 新的元数据帧
    NewMeta(MetaPayload),
    /// 新的数据块（块索引）
    NewBlock(u32),
    /// 新的 FEC 校验分片（组号，组内校验序号）
    NewParity(u16, u16),
    /// 收到 EOT 信号
    EotReceived,
    /// 重复帧（已见过的序号）
    Duplicate,
    /// 无效帧
    Invalid,
}

/// 最小模块像素数
const MODULE_PIXELS_MIN: u32 = 2;

/// 最大模块像素数
const MODULE_PIXELS_MAX: u32 = 8;

/// 验证模块像素参数是否合法
pub fn validate_module_pixels(pixels: u32) -> crate::common::error::OptransResult<()> {
    if !(MODULE_PIXELS_MIN..=MODULE_PIXELS_MAX).contains(&pixels) || !pixels.is_multiple_of(2) {
        return Err(crate::common::error::OptransError::InvalidArgument(format!(
            "模块像素数必须在 {}-{} 之间且为偶数，当前值: {}",
            MODULE_PIXELS_MIN, MODULE_PIXELS_MAX, pixels
        )));
    }
    Ok(())
}
