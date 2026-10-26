//! 帧收集、去重与 FEC 恢复模块
//!
//! 管理接收到的帧，按数据内容幂等摄入，分类存储元数据、数据块和校验分片。
//! 收到 EOT 后，若数据块完整则直接重组；若有缺失但 FEC 分片充足，则恢复。
//!
//! 去重策略（v0.30）：不按帧序号去重——seq 是 u16，长传输会回绕，
//! 且解码失败的帧重试后会复用同一 seq，按 seq 去重会把重试帧误判为重复。
//! 改为块级 first-write-wins：`entry().or_insert()`，重复内容幂等，
//! 首次写入的块永不被后续帧覆盖（避免重传噪声毒化好数据）。

use std::collections::HashMap;

use crate::common::error::{OptransError, OptransResult};
use crate::common::fec::FecDecoder;
use crate::common::types::{
    DecodedFrame, FrameType, IngestResult, MetaPayload,
};
use crate::decoder::decode::parse_meta_payload;

/// 帧收集器：幂等摄入、分类存储、FEC 恢复
pub struct FrameCollector {
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
            meta: None,
            data_blocks: HashMap::new(),
            parity_shards: HashMap::new(),
            total_blocks: 0,
        }
    }

    /// 摄入一个新的解码帧
    ///
    /// 幂等：同一块/分片重复到达返回 [`IngestResult::Duplicate`]，
    /// 首次写入的数据不会被覆盖。
    pub fn ingest(&mut self, frame: DecodedFrame) -> OptransResult<IngestResult> {
        self.ingest_frame(frame)
    }

    /// 摄入帧
    fn ingest_frame(&mut self, frame: DecodedFrame) -> OptransResult<IngestResult> {
        let result = match frame.frame_type {
            FrameType::Meta => {
                if self.meta.is_some() {
                    return Ok(IngestResult::Duplicate);
                }
                let meta = parse_meta_payload(&frame.payload)?;
                self.total_blocks = meta.total_blocks;
                self.meta = Some(meta.clone());
                IngestResult::NewMeta(meta)
            }
            FrameType::Data => {
                // 多块合并格式 [count: u16 LE][entries...]
                match Self::try_parse_multi_block(&frame.payload) {
                    // 格式损坏：丢弃整帧（不回退单块，避免毒化 FEC）
                    Some(entries) if entries.is_empty() => IngestResult::Invalid,
                    Some(entries) => self.commit_blocks(entries),
                    None => {
                        // 格式不匹配才回退单块格式 [block_index: u32 LE][data]
                        if frame.payload.len() < 4 {
                            return Err(OptransError::FrameDecode(
                                "数据帧载荷不足，无法提取 block_index".to_string(),
                            ));
                        }
                        let block_index = u32::from_le_bytes(
                            frame.payload[..4].try_into().map_err(|_| {
                                OptransError::FrameDecode("block_index 解析失败".to_string())
                            })?,
                        );
                        self.commit_blocks(vec![(block_index, frame.payload[4..].to_vec())])
                    }
                }
            }
            FrameType::FecParity => {
                // FEC 校验帧：block_index 编码 (group_idx, parity_idx)
                let (group_idx, parity_idx) =
                    crate::common::types::decode_parity_block_index(frame.block_index);
                use std::collections::hash_map::Entry;
                match self.parity_shards.entry((group_idx, parity_idx)) {
                    Entry::Occupied(_) => IngestResult::Duplicate,
                    Entry::Vacant(e) => {
                        e.insert(frame.payload);
                        IngestResult::NewParity(group_idx, parity_idx)
                    }
                }
            }
            FrameType::EOT => IngestResult::EotReceived,
        };

        Ok(result)
    }

    /// 提交一批数据块（first-write-wins）
    ///
    /// 全部块都已存在时返回 [`IngestResult::Duplicate`]；
    /// 否则返回第一个新块的索引。
    fn commit_blocks(&mut self, entries: Vec<(u32, Vec<u8>)>) -> IngestResult {
        use std::collections::hash_map::Entry;
        let mut first_new: Option<u32> = None;
        for (block_index, block_data) in entries {
            match self.data_blocks.entry(block_index) {
                Entry::Occupied(_) => {}
                Entry::Vacant(e) => {
                    e.insert(block_data);
                    if first_new.is_none() {
                        first_new = Some(block_index);
                    }
                }
            }
        }
        match first_new {
            Some(idx) => IngestResult::NewBlock(idx),
            None => IngestResult::Duplicate,
        }
    }

    /// 纯解析多块合并数据帧格式 [count: u16 LE][entries...]，不做任何提交
    ///
    /// 返回 `Some(entries)` 表示整个载荷严格匹配多块格式（all-or-nothing）：
    /// - `None`：不像多块格式（count 非法等）→ 调用方回退单块格式
    /// - `Some(_)` 后由 [`FrameCollector::commit_blocks`] 一次性提交，
    ///   绝不部分提交（半截帧毒化 FEC：坏块占位后 FEC 永不覆盖）
    ///
    /// 判定规则：count 合法（1..=1000）即认定为多块格式意图，
    /// entry 损坏或尾部有多余字节返回空 entries（由调用方丢弃为 Invalid）。
    fn try_parse_multi_block(payload: &[u8]) -> Option<Vec<(u32, Vec<u8>)>> {
        if payload.len() < 2 {
            return None;
        }

        let count = u16::from_le_bytes([payload[0], payload[1]]) as usize;
        if count == 0 || count > 1000 {
            return None;
        }

        let mut offset = 2usize;
        let mut entries = Vec::with_capacity(count);

        for _ in 0..count {
            if offset + 8 > payload.len() {
                // count 合法但 entry 损坏：认定为多块格式意图但已损坏，
                // 返回 Some(空) 让调用方丢弃本帧（不回退单块，避免毒化）
                return Some(Vec::new());
            }
            let block_index = u32::from_le_bytes([payload[offset], payload[offset + 1], payload[offset + 2], payload[offset + 3]]);
            let data_len = u32::from_le_bytes([
                payload[offset + 4],
                payload[offset + 5],
                payload[offset + 6],
                payload[offset + 7],
            ]) as usize;
            offset += 8;
            if offset + data_len > payload.len() {
                return Some(Vec::new());
            }
            entries.push((block_index, payload[offset..offset + data_len].to_vec()));
            offset += data_len;
        }

        // 尾部校验：多块格式必须精确占满载荷，多余字节 = 格式损坏
        if offset != payload.len() {
            return Some(Vec::new());
        }

        Some(entries)
    }

    pub fn has_meta(&self) -> bool {
        self.meta.is_some()
    }

    /// 数据块是否全部收到（不考虑 FEC 恢复）
    ///
    /// 以 meta 的 total_blocks 为准逐块核对：
    /// - 空文件（total_blocks == 0）自然成立
    /// - 越界/垃圾块索引不计入（HashMap len 不能代表进度）
    pub fn is_complete(&self) -> bool {
        self.has_meta() && self.missing_blocks().is_empty()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::types::EncodingMode;

    fn data_frame(seq: u16, payload: Vec<u8>) -> DecodedFrame {
        DecodedFrame {
            frame_type: FrameType::Data,
            frame_sequence: seq,
            block_index: 0,
            total_blocks: 0,
            payload,
            encoding_mode: EncodingMode::Binary,
        }
    }

    /// 构造多块合并载荷 [count: u16 LE][entries...]
    fn multi_payload(entries: &[(u32, &[u8])]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (idx, data) in entries {
            payload.extend_from_slice(&idx.to_le_bytes());
            payload.extend_from_slice(&(data.len() as u32).to_le_bytes());
            payload.extend_from_slice(data);
        }
        payload
    }

    /// 构造 meta 载荷（空文件名省略，直接最小字段）
    fn meta_payload(total_blocks: u32) -> Vec<u8> {
        let mut payload = Vec::new();
        let name = b"a.bin";
        payload.push(name.len() as u8);
        payload.extend_from_slice(name);
        payload.extend_from_slice(&0u64.to_le_bytes()); // file_size
        payload.extend_from_slice(&[0u8; 32]); // sha256
        payload.extend_from_slice(&total_blocks.to_le_bytes());
        payload.extend_from_slice(&0u16.to_le_bytes()); // fec_k
        payload.extend_from_slice(&0u16.to_le_bytes()); // fec_m
        payload.extend_from_slice(&0u32.to_le_bytes()); // fec_groups
        payload.extend_from_slice(&0u32.to_le_bytes()); // fec_shard_size
        payload
    }

    fn meta_frame(total_blocks: u32) -> DecodedFrame {
        DecodedFrame {
            frame_type: FrameType::Meta,
            frame_sequence: 0,
            block_index: 0,
            total_blocks,
            payload: meta_payload(total_blocks),
            encoding_mode: EncodingMode::Binary,
        }
    }

    #[test]
    fn test_multi_block_roundtrip() {
        let mut c = FrameCollector::new();
        let payload = multi_payload(&[(0, b"aaa"), (1, b"bbbb")]);
        let r = c.ingest(data_frame(1, payload)).unwrap();
        assert!(matches!(r, IngestResult::NewBlock(0)));
        assert_eq!(c.blocks_received(), 2);
    }

    #[test]
    fn test_multi_block_corrupt_entry_rejected_wholesale() {
        // M6: count 合法但 entry 损坏 → 整帧丢弃（不回退单块、不部分提交）
        let mut c = FrameCollector::new();
        let mut payload = multi_payload(&[(0, b"aaa"), (1, b"bbbb")]);
        // 截掉尾部字节，模拟 entry 损坏
        payload.truncate(payload.len() - 3);
        let r = c.ingest(data_frame(1, payload)).unwrap();
        assert!(matches!(r, IngestResult::Invalid));
        assert_eq!(c.blocks_received(), 0, "损坏帧不得部分提交");
    }

    #[test]
    fn test_multi_block_trailing_garbage_rejected() {
        // M6: 尾部多余字节 = 格式损坏，整帧丢弃
        let mut c = FrameCollector::new();
        let mut payload = multi_payload(&[(0, b"aaa")]);
        payload.push(0xFF);
        let r = c.ingest(data_frame(1, payload)).unwrap();
        assert!(matches!(r, IngestResult::Invalid));
        assert_eq!(c.blocks_received(), 0);
    }

    #[test]
    fn test_single_block_fallback() {
        // M6: count 非法（=0）→ 回退单块格式
        let mut c = FrameCollector::new();
        let mut payload = Vec::new();
        payload.extend_from_slice(&0u32.to_le_bytes()); // block_index 0
        payload.extend_from_slice(b"hello");
        // 该载荷前 2 字节是 index 低 16 位 = 0 → count=0 → 回退单块
        let r = c.ingest(data_frame(1, payload)).unwrap();
        assert!(matches!(r, IngestResult::NewBlock(0)));
        assert_eq!(c.blocks_received(), 1);
    }

    #[test]
    fn test_first_write_wins_no_overwrite() {
        // M3: 块重复到达幂等，首次写入不被覆盖
        let mut c = FrameCollector::new();
        c.ingest(data_frame(1, multi_payload(&[(0, b"original")])))
            .unwrap();
        let r = c
            .ingest(data_frame(2, multi_payload(&[(0, b"corrupted")])))
            .unwrap();
        assert!(matches!(r, IngestResult::Duplicate));
        let blocks = c.take_blocks();
        assert_eq!(blocks[&0], b"original".to_vec());
    }

    #[test]
    fn test_seq_wraparound_not_deduped() {
        // M3: u16 seq 回绕（或重试复用同 seq）不再导致数据丢弃
        let mut c = FrameCollector::new();
        let r1 = c
            .ingest(data_frame(65535, multi_payload(&[(0, b"a")])))
            .unwrap();
        let r2 = c
            .ingest(data_frame(0, multi_payload(&[(1, b"b")])))
            .unwrap();
        assert!(matches!(r1, IngestResult::NewBlock(0)));
        assert!(matches!(r2, IngestResult::NewBlock(1)));
        assert_eq!(c.blocks_received(), 2);
    }

    #[test]
    fn test_empty_file_is_complete() {
        // M4: 空文件（total_blocks=0）收到 meta 即完整
        let mut c = FrameCollector::new();
        assert!(!c.is_complete());
        c.ingest(meta_frame(0)).unwrap();
        assert!(c.is_complete());
        assert!(c.missing_blocks().is_empty());
    }

    #[test]
    fn test_out_of_range_blocks_not_counted() {
        // M4: 垃圾块索引不计入进度
        let mut c = FrameCollector::new();
        c.ingest(meta_frame(2)).unwrap();
        c.ingest(data_frame(1, multi_payload(&[(99, b"junk")])))
            .unwrap();
        assert!(!c.is_complete());
        assert_eq!(c.missing_blocks(), vec![0, 1]);
    }

    #[test]
    fn test_meta_duplicate_ignored() {
        let mut c = FrameCollector::new();
        let r1 = c.ingest(meta_frame(3)).unwrap();
        assert!(matches!(r1, IngestResult::NewMeta(_)));
        let r2 = c.ingest(meta_frame(3)).unwrap();
        assert!(matches!(r2, IngestResult::Duplicate));
    }
}
