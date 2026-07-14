//! 帧收集、去重与 FEC 恢复模块
//!
//! 管理接收到的帧，按序号去重，分类存储元数据、数据块和校验分片。
//! 收到 EOT 后，若数据块完整则直接重组；若有缺失但 FEC 分片充足，则恢复。

use std::collections::{HashMap, HashSet};

use crate::common::error::{OptransError, OptransResult};
use crate::common::fec::FecDecoder;
use crate::common::types::{
    DecodedFrame, FrameType, IngestResult, MetaPayload,
};
use crate::decoder::decode::parse_meta_payload;

/// 帧收集器：去重、分类存储、FEC 恢复
pub struct FrameCollector {
    seen_seqs: HashSet<u16>,
    meta: Option<MetaPayload>,
    /// 数据块：block_index → block_data
    data_blocks: HashMap<u32, Vec<u8>>,
    /// FEC 校验分片：(group_idx, parity_idx) → shard_data
    parity_shards: HashMap<(u16, u16), Vec<u8>>,
    total_blocks: u32,
}

impl FrameCollector {
    /// 创建新的帧收集器
    pub fn new() -> Self {
        FrameCollector {
            seen_seqs: HashSet::new(),
            meta: None,
            data_blocks: HashMap::new(),
            parity_shards: HashMap::new(),
            total_blocks: 0,
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
                // 多块合并格式 [count: u16 LE][entries...]
                // 若解析失败则回退到单块格式 [block_index: u32 LE][data]
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
            FrameType::FecParity => {
                // FEC 校验帧：block_index 编码 (group_idx, parity_idx)
                let (group_idx, parity_idx) =
                    crate::common::types::decode_parity_block_index(frame.block_index);
                self.parity_shards
                    .insert((group_idx, parity_idx), frame.payload);
                IngestResult::NewParity(group_idx, parity_idx)
            }
            FrameType::EOT => IngestResult::EotReceived,
        };

        Ok(result)
    }

    /// 尝试解析多块合并数据帧格式 [count: u16 LE][entries...]
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

            self.data_blocks.insert(block_index, block_data);
            last_result = IngestResult::NewBlock(block_index);
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

    /// 数据块是否全部收到（不考虑 FEC 恢复）
    pub fn is_complete(&self) -> bool {
        if self.total_blocks == 0 {
            return false;
        }
        self.data_blocks.len() as u32 >= self.total_blocks
    }

    /// 缺失的数据块索引列表
    pub fn missing_blocks(&self) -> Vec<u32> {
        let mut missing = Vec::new();
        for i in 0..self.total_blocks {
            if !self.data_blocks.contains_key(&i) {
                missing.push(i);
            }
        }
        missing
    }

    /// 尝试用 FEC 校验分片恢复缺失的数据块
    ///
    /// 对每个不完整的 FEC 组：
    /// 1. 收集该组已收到的数据分片和校验分片
    /// 2. 若总分片数 ≥ k，调用 RS 解码恢复
    /// 3. 将恢复的数据分片填入 data_blocks
    ///
    /// 返回恢复的块数
    pub fn try_fec_recover(&mut self) -> OptransResult<usize> {
        let meta = match &self.meta {
            Some(m) if m.fec_enabled() => m.clone(),
            _ => return Ok(0),
        };

        let k = meta.fec_k as usize;
        let m = meta.fec_m as usize;
        let shard_size = meta.fec_shard_size as usize;
        let num_groups = meta.fec_groups as usize;
        let total_blocks = meta.total_blocks as usize;

        let mut recovered_count = 0usize;
        let decoder = FecDecoder::with_params(k, m)?;

        for g in 0..num_groups {
            let group_start = g * k;
            let group_end = std::cmp::min(group_start + k, total_blocks);

            // 检查该组是否已完整
            let group_missing: Vec<u32> = (group_start as u32..group_end as u32)
                .filter(|i| !self.data_blocks.contains_key(i))
                .collect();
            if group_missing.is_empty() {
                continue;
            }

            // 构造分片数组：前 k 个是数据分片，后 m 个是校验分片
            let mut shards: Vec<Option<Vec<u8>>> = Vec::with_capacity(k + m);

            for i in 0..k {
                let block_idx = group_start + i;
                if block_idx < group_end {
                    if let Some(data) = self.data_blocks.get(&(block_idx as u32)) {
                        // 数据分片需补零到 shard_size
                        let mut shard = data.clone();
                        shard.resize(shard_size, 0u8);
                        shards.push(Some(shard));
                    } else {
                        shards.push(None);
                    }
                } else {
                    // 超出实际块数的分片，用零分片填充（发送端也是用零分片编码的）
                    shards.push(Some(vec![0u8; shard_size]));
                }
            }

            for p in 0..m {
                let shard = self.parity_shards.get(&(g as u16, p as u16));
                shards.push(shard.cloned());
            }

            // RS 解码恢复
            let recovered = match decoder.reconstruct_data(&mut shards) {
                Ok(data) => data,
                Err(e) => {
                    return Err(OptransError::FrameDecode(format!(
                        "FEC 恢复组 {} 失败: {}（缺失 {} 块）",
                        g,
                        e,
                        group_missing.len()
                    )));
                }
            };

            // 填入恢复的数据分片（仅填缺失的，已存在的不覆盖）
            for i in 0..k {
                let block_idx = group_start + i;
                if block_idx < group_end && !self.data_blocks.contains_key(&(block_idx as u32)) {
                    // 恢复的分片长度 = shard_size，需要按实际块长度截断
                    // 但我们不知道实际块长度，所以保留 shard_size
                    // 重组文件时会按 file_size 截断，不影响正确性
                    self.data_blocks.insert(block_idx as u32, recovered[i].clone());
                    recovered_count += 1;
                }
            }
        }

        Ok(recovered_count)
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

    pub fn parity_received(&self) -> usize {
        self.parity_shards.len()
    }
}

impl Default for FrameCollector {
    fn default() -> Self {
        Self::new()
    }
}
