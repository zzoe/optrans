//! 帧收集与去重模块
//!
//! 管理接收到的帧，按序号去重，分类存储元数据和数据块。
//! v0.2 新增：I 帧缓存、P 帧合并、base_frame_seq 校验。

use std::collections::{HashMap, HashSet};

use crate::common::constants::{RS_DATA_LEN, RS_TOTAL_LEN};
use crate::common::error::{OptransError, OptransResult};
use crate::common::types::{
    DecodedFrame, DiffEntry, EncodingMode, FrameKind, FrameType, GrayEncodingConfig, IngestResult,
    MetaPayload,
};
use crate::decoder::decode::{decode_p_frame_payload, parse_meta_payload};
use crate::decoder::reed_solomon::rs_decode_data;

use log::warn;

/// 接收端 I 帧缓存
#[derive(Debug, Clone)]
pub struct ReceiverCache {
    /// 缓存的 I 帧完整数据模块灰度值序列
    pub i_frame_modules: Vec<u8>,
    /// 缓存的 I 帧的 frame_sequence
    pub i_frame_seq: u16,
    /// I 帧的 payload_bytes（用于 P 帧合并时计算解码长度）
    pub i_frame_payload_bytes: u32,
}

/// 帧收集器：去重、分类存储
pub struct FrameCollector {
    seen_seqs: HashSet<u16>,
    meta: Option<MetaPayload>,
    data_blocks: HashMap<u32, Vec<u8>>,
    total_blocks: u32,
    /// v0.2: I 帧缓存（用于 P 帧合并）
    cache: Option<ReceiverCache>,
}

impl FrameCollector {
    /// 创建新的帧收集器
    pub fn new() -> Self {
        FrameCollector {
            seen_seqs: HashSet::new(),
            meta: None,
            data_blocks: HashMap::new(),
            total_blocks: 0,
            cache: None,
        }
    }

    /// 摄入一个新的解码帧
    pub fn ingest(&mut self, frame: DecodedFrame) -> OptransResult<IngestResult> {
        let seq = frame.frame_sequence;

        if self.seen_seqs.contains(&seq) {
            return Ok(IngestResult::Duplicate);
        }

        // 先尝试摄入，成功后再标记为已处理
        // 如果摄入失败（如元数据载荷为空），不占用 seq，允许后续重试
        let result = match frame.frame_kind {
            FrameKind::I => self.ingest_i_frame(frame),
            FrameKind::P => self.ingest_p_frame(frame),
        }?;

        self.seen_seqs.insert(seq);
        Ok(result)
    }

    /// 摄入 I 帧：正常处理 + 缓存模块值
    fn ingest_i_frame(&mut self, frame: DecodedFrame) -> OptransResult<IngestResult> {
        let result = match frame.frame_type {
            FrameType::Meta => {
                let meta = parse_meta_payload(&frame.payload)?;
                self.total_blocks = meta.total_blocks;
                self.meta = Some(meta.clone());
                IngestResult::NewMeta(meta)
            }
            FrameType::Data => {
                // v0.9: 尝试多块合并格式 [count: u16 LE][entries...]
                // 如果 payload 以 count 开头且可以成功解析，则按多块处理；
                // 否则回退到单块格式 [block_index: u32 LE][data]
                if let Some(result) = self.try_ingest_multi_block(&frame) {
                    return Ok(result);
                }

                if frame.payload.len() < 4 {
                    return Err(OptransError::FrameDecode(
                        "数据帧载荷不足，无法提取 block_index".to_string(),
                    ));
                }
                let block_index = u32::from_le_bytes(
                    frame.payload[..4]
                        .try_into()
                        .map_err(|_| {
                            OptransError::FrameDecode("block_index 解析失败".to_string())
                        })?,
                );
                let block_data_len = frame.payload[4..].len();
                let block_data = frame.payload[4..].to_vec();
                self.data_blocks.insert(block_index, block_data);

                // 缓存 I 帧数据模块值（用于后续 P 帧合并）
                if !frame.data_module_values.is_empty() {
                    self.cache_i_frame(
                        frame.data_module_values,
                        frame.frame_sequence,
                        (block_data_len + 4) as u32,
                    );
                }

                IngestResult::NewBlock(block_index)
            }
            FrameType::EOT => IngestResult::EotReceived,
        };

        Ok(result)
    }

    /// v0.9: 尝试解析多块合并数据帧格式 [count: u16 LE][entries...]
    /// v0.10: 支持元数据嵌入（block_index == 0xFFFFFFFF 表示元数据块）
    /// 成功返回 Some(IngestResult)，格式不匹配返回 None
    fn try_ingest_multi_block(&mut self, frame: &DecodedFrame) -> Option<IngestResult> {
        let payload = &frame.payload;
        if payload.len() < 2 {
            return None;
        }

        let count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
        if count == 0 || count > 1000 {
            return None;
        }

        let mut offset = 2usize;
        let mut last_result = IngestResult::NewBlock(0);
        let mut blocks_ingested = 0usize;

        const META_BLOCK_INDEX: u32 = 0xFFFF_FFFF;

        for _ in 0..count {
            if offset + 6 > payload.len() {
                return None;
            }
            let block_index = u32::from_le_bytes(
                payload[offset..offset+4].try_into().ok()?
            );
            let data_len = u16::from_le_bytes(
                payload[offset+4..offset+6].try_into().ok()?
            ) as usize;
            offset += 6;
            if offset + data_len > payload.len() {
                return None;
            }
            let block_data = payload[offset..offset+data_len].to_vec();
            offset += data_len;

            if block_index == META_BLOCK_INDEX {
                // 嵌入的元数据块：解析并设置 collector 元数据
                match parse_meta_payload(&block_data) {
                    Ok(meta) => {
                        self.total_blocks = meta.total_blocks;
                        self.meta = Some(meta);
                        last_result = IngestResult::NewMeta(self.meta.clone().unwrap());
                    }
                    Err(_) => {
                        // 元数据解析失败，跳过（可能为假阳性）
                        continue;
                    }
                }
            } else {
                self.data_blocks.insert(block_index, block_data);
                last_result = IngestResult::NewBlock(block_index);
            }
            blocks_ingested += 1;
        }

        if blocks_ingested == 0 {
            return None;
        }

        // 缓存 I 帧数据模块值（用于后续 P 帧合并）
        if !frame.data_module_values.is_empty() {
            self.cache_i_frame(
                frame.data_module_values.clone(),
                frame.frame_sequence,
                payload.len() as u32,
            );
        }

        Some(last_result)
    }

    /// 摄入 P 帧：校验 → merge → 灰度解码 → RS 解码 → 模拟 I 帧
    fn ingest_p_frame(&mut self, frame: DecodedFrame) -> OptransResult<IngestResult> {
        let (base_frame_seq, diff_entries) = decode_p_frame_payload(&frame.payload)?;

        if !self.check_base_frame(base_frame_seq) {
            warn!(
                "丢弃 P 帧 seq={}: base_frame_seq={} 与缓存不匹配 (缓存={})",
                frame.frame_sequence,
                base_frame_seq,
                self.cache.as_ref().map(|c| c.i_frame_seq).unwrap_or(0)
            );
            return Ok(IngestResult::Invalid);
        }

        // 合并 P 帧差异到缓存模块
        let merged_modules = self.merge_p_frame(&diff_entries);

        // 计算 I 帧原始 payload 的编码信息
        let cache = self.cache.as_ref().unwrap();
        let i_payload_bytes = cache.i_frame_payload_bytes as usize;
        let gray_levels = match &frame.encoding_mode {
            EncodingMode::Gray(n) => *n,
            _ => 2,
        };

        // 计算 RS 编码后的字节数
        let num_rs_blocks = i_payload_bytes.div_ceil(RS_DATA_LEN);
        let encoded_bytes = 4 + num_rs_blocks * RS_TOTAL_LEN;

        // 计算需要的灰度模块数
        let needed_modules = match gray_levels {
            2 => encoded_bytes * 8,
            4 => encoded_bytes * 4,
            5 => encoded_bytes.div_ceil(2) * 7,
            8 => (encoded_bytes * 8).div_ceil(3),
            16 => encoded_bytes * 2,
            _ => encoded_bytes * 8,
        };

        // 灰度解码 → RS 编码字节
        let config = GrayEncodingConfig::new(gray_levels);
        let relevant_modules = &merged_modules[..needed_modules.min(merged_modules.len())];
        let rs_encoded = crate::decoder::extract::decode_gray_n(
            relevant_modules,
            gray_levels,
            &config.thresholds,
            encoded_bytes,
        );

        // v0.6: 反向应用数据掩码（XOR 自反），恢复原始 RS 编码字节
        let rs_encoded = crate::common::mask::apply_mask(&rs_encoded);

        // RS 解码 → 原始 payload (block_index + data)
        let decoded_payload = rs_decode_data(&rs_encoded)?;

        // 构造虚拟 I 帧并摄入
        let simulated_i = DecodedFrame {
            frame_type: frame.frame_type,
            frame_sequence: frame.frame_sequence,
            block_index: frame.block_index,
            total_blocks: frame.total_blocks,
            payload: decoded_payload,
            frame_kind: FrameKind::I,
            encoding_mode: frame.encoding_mode,
            base_frame_seq: None,
            data_module_values: Vec::new(),
        };

        self.ingest_i_frame(simulated_i)
    }

    /// 校验 base_frame_seq 是否匹配缓存
    pub fn check_base_frame(&self, base_frame_seq: u16) -> bool {
        match &self.cache {
            Some(c) => c.i_frame_seq == base_frame_seq,
            None => {
                warn!("收到 P 帧但无 I 帧缓存，丢弃");
                false
            }
        }
    }

    /// 合并 P 帧的 diff entries 到缓存的 I 帧模块中
    fn merge_p_frame(&mut self, diff_entries: &[DiffEntry]) -> Vec<u8> {
        let cache = self.cache.as_ref().expect("merge_p_frame 需要 I 帧缓存");
        let mut merged = cache.i_frame_modules.clone();

        for entry in diff_entries {
            let idx = entry.module_index as usize;
            if idx < merged.len() {
                merged[idx] = entry.module_value;
            }
        }

        merged
    }

    /// 缓存 I 帧数据模块值
    fn cache_i_frame(&mut self, modules: Vec<u8>, seq: u16, payload_bytes: u32) {
        self.cache = Some(ReceiverCache {
            i_frame_modules: modules,
            i_frame_seq: seq,
            i_frame_payload_bytes: payload_bytes,
        });
    }

    pub fn has_meta(&self) -> bool {
        self.meta.is_some()
    }

    pub fn is_complete(&self) -> bool {
        if self.total_blocks == 0 {
            return false;
        }
        self.data_blocks.len() as u32 >= self.total_blocks
    }

    pub fn missing_blocks(&self) -> Vec<u32> {
        let mut missing = Vec::new();
        for i in 0..self.total_blocks {
            if !self.data_blocks.contains_key(&i) {
                missing.push(i);
            }
        }
        missing
    }

    pub fn meta(&self) -> Option<&MetaPayload> {
        self.meta.as_ref()
    }

    pub fn take_blocks(self) -> HashMap<u32, Vec<u8>> {
        self.data_blocks
    }

    pub fn blocks_received(&self) -> usize {
        self.data_blocks.len()
    }
}

impl Default for FrameCollector {
    fn default() -> Self {
        Self::new()
    }
}
