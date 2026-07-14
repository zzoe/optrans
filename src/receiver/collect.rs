//! 帧收集与去重模块
//!
//! 管理接收到的帧，按序号去重，分类存储元数据和数据块。

use std::collections::{HashMap, HashSet};

use crate::common::error::{OptransError, OptransResult};
use crate::common::types::{
    DecodedFrame, FrameType, IngestResult, MetaPayload,
};
use crate::decoder::decode::parse_meta_payload;

/// 帧收集器：去重、分类存储
pub struct FrameCollector {
    seen_seqs: HashSet<u16>,
    meta: Option<MetaPayload>,
    data_blocks: HashMap<u32, Vec<u8>>,
    total_blocks: u32,
}

impl FrameCollector {
    /// 创建新的帧收集器
    pub fn new() -> Self {
        FrameCollector {
            seen_seqs: HashSet::new(),
            meta: None,
            data_blocks: HashMap::new(),
            total_blocks: 0,
        }
    }

    /// 摄入一个新的解码帧
    pub fn ingest(&mut self, frame: DecodedFrame) -> OptransResult<IngestResult> {
        let seq = frame.frame_sequence;

        if self.seen_seqs.contains(&seq) {
            return Ok(IngestResult::Duplicate);
        }

        // 协议保留 P 帧枚举值，但本程序不产生 P 帧。
        // 若收到 P 帧（来自不兼容的发送端），直接拒绝。
        if frame.frame_kind == crate::common::types::FrameKind::P {
            return Ok(IngestResult::Invalid);
        }

        // 先尝试摄入，成功后再标记为已处理
        // 如果摄入失败（如元数据载荷为空），不占用 seq，允许后续重试
        let result = self.ingest_frame(frame)?;

        self.seen_seqs.insert(seq);
        Ok(result)
    }

    /// 摄入帧
    fn ingest_frame(&mut self, frame: DecodedFrame) -> OptransResult<IngestResult> {
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
                let block_data = frame.payload[4..].to_vec();
                self.data_blocks.insert(block_index, block_data);

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
            if offset + 8 > payload.len() {
                return None;
            }
            let block_index = u32::from_le_bytes(
                payload[offset..offset+4].try_into().ok()?
            );
            let data_len = u32::from_le_bytes(
                payload[offset+4..offset+8].try_into().ok()?
            ) as usize;
            offset += 8;
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

        Some(last_result)
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
