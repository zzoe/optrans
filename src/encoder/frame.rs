//! 帧构建模块
//!
//! 组装元数据帧、数据帧和 EOT（传输结束）帧。
//! 每帧包含格式信息（FormatInfo）和 RS 编码后的载荷数据。

use crate::common::constants::MAX_FILE_NAME_LEN;
use crate::common::error::{OptransError, OptransResult};
use crate::common::types::{
    BlockData, FormatInfo, FrameType, MetaPayload,
};
use crate::encoder::reed_solomon::RsEncoder;

/// 构建完成的帧（含格式信息 + RS 编码后的数据）
#[derive(Debug, Clone)]
pub struct Frame {
    /// 帧格式信息
    pub format_info: FormatInfo,
    /// RS 编码后的载荷数据
    pub encoded_payload: Vec<u8>,
}

impl Frame {
    /// 构建元数据帧
    ///
    /// 载荷格式：name_len(1B) + name + file_size(8B LE) + sha256(32B) + total_blocks(4B LE)
    ///          + fec_k(2B LE) + fec_m(2B LE) + fec_groups(4B LE) + fec_shard_size(4B LE)
    /// 元数据帧始终为二值模式。
    pub fn new_metadata(meta: &MetaPayload, seq: u16) -> OptransResult<Frame> {
        let file_name_bytes = meta.file_name.as_bytes();
        if file_name_bytes.len() > MAX_FILE_NAME_LEN {
            return Err(OptransError::InvalidArgument(format!(
                "文件名过长：{} 字节（最大 {} 字节）",
                file_name_bytes.len(),
                MAX_FILE_NAME_LEN
            )));
        }

        // 载荷：name_len(1) + name + file_size(8) + sha256(32) + total_blocks(4) + FEC(12)
        let payload_len = 1 + file_name_bytes.len() + 8 + 32 + 4 + 12;
        let mut payload = Vec::with_capacity(payload_len);

        payload.push(file_name_bytes.len() as u8);
        payload.extend_from_slice(file_name_bytes);
        payload.extend_from_slice(&meta.file_size.to_le_bytes());
        payload.extend_from_slice(&meta.sha256);
        payload.extend_from_slice(&meta.total_blocks.to_le_bytes());
        // FEC 字段
        payload.extend_from_slice(&meta.fec_k.to_le_bytes());
        payload.extend_from_slice(&meta.fec_m.to_le_bytes());
        payload.extend_from_slice(&meta.fec_groups.to_le_bytes());
        payload.extend_from_slice(&meta.fec_shard_size.to_le_bytes());

        let info = FormatInfo::new_meta(seq, meta.total_blocks, payload.len() as u32);

        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(&payload)?;

        Ok(Frame {
            format_info: info,
            encoded_payload,
        })
    }

    /// 构建数据帧
    ///
    /// 数据帧载荷包含：block_index(4B LE) + block_data
    pub fn new_data(
        block: &BlockData,
        seq: u16,
        total_blocks: u32,
        encoding_mode_bit: u8,
    ) -> OptransResult<Frame> {
        let payload_len = 4 + block.data.len();
        let mut payload = Vec::with_capacity(payload_len);
        payload.extend_from_slice(&block.index.to_le_bytes());
        payload.extend_from_slice(&block.data);

        let info = FormatInfo::new_data(
            seq,
            block.index,
            total_blocks,
            payload.len() as u32,
            encoding_mode_bit,
        );

        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(&payload)?;

        Ok(Frame {
            format_info: info,
            encoded_payload,
        })
    }

    /// 构建多块合并数据帧（v0.9）
    ///
    /// 将多个数据块打包到一个帧中，填满 payload 容量。
    /// 载荷格式：[count: u16 LE][entries...]
    /// 每个 entry：[block_index: u32 LE][data_len: u32 LE][data: data_len bytes]
    pub fn new_data_merged(
        blocks: &[&BlockData],
        seq: u16,
        total_blocks: u32,
        encoding_mode_bit: u8,
    ) -> OptransResult<Frame> {
        let payload_len: usize = 2 + blocks.iter()
            .map(|b| 4 + 4 + b.data.len())
            .sum::<usize>();

        let mut payload = Vec::with_capacity(payload_len);
        let count = blocks.len() as u16;
        payload.extend_from_slice(&count.to_le_bytes());

        for block in blocks {
            payload.extend_from_slice(&block.index.to_le_bytes());
            let data_len = block.data.len() as u32;
            payload.extend_from_slice(&data_len.to_le_bytes());
            payload.extend_from_slice(&block.data);
        }

        let first_index = blocks.first().map(|b| b.index).unwrap_or(0);
        let info = FormatInfo::new_data(
            seq, first_index, total_blocks,
            payload.len() as u32, encoding_mode_bit,
        );

        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(&payload)?;

        Ok(Frame { format_info: info, encoded_payload })
    }

    /// 构建 EOT（传输结束）帧
    ///
    /// EOT 帧没有载荷数据。
    pub fn new_eot(seq: u16) -> OptransResult<Frame> {
        let info = FormatInfo::new_eot(seq);
        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(&[])?;

        Ok(Frame {
            format_info: info,
            encoded_payload,
        })
    }

    /// 构建 FEC 校验帧
    ///
    /// 载荷为单个校验分片的原始字节（不加 entry header）。
    /// `block_index` 编码：高 16 位 = group_idx，低 16 位 = parity_idx
    pub fn new_parity(
        seq: u16,
        group_idx: u16,
        parity_idx: u16,
        total_blocks: u32,
        parity_data: &[u8],
        encoding_mode_bit: u8,
    ) -> OptransResult<Frame> {
        let info = FormatInfo::new_parity(
            seq,
            group_idx,
            parity_idx,
            total_blocks,
            parity_data.len() as u32,
            encoding_mode_bit,
        );

        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(parity_data)?;

        Ok(Frame {
            format_info: info,
            encoded_payload,
        })
    }

    /// 获取帧类型
    pub fn frame_type(&self) -> FrameType {
        self.format_info.frame_type
    }

    /// 获取帧序号
    pub fn sequence(&self) -> u16 {
        self.format_info.frame_sequence
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::types::MetaPayload;

    fn make_test_meta() -> MetaPayload {
        MetaPayload {
            file_name: "test.dat".to_string(),
            file_size: 4096,
            sha256: [0xAB; 32],
            total_blocks: 1,
            fec_k: 0,
            fec_m: 0,
            fec_groups: 0,
            fec_shard_size: 0,
        }
    }

    #[test]
    fn test_new_metadata_frame() {
        let meta = make_test_meta();
        let frame = Frame::new_metadata(&meta, 0).unwrap();
        assert_eq!(frame.format_info.frame_type, FrameType::Meta);
        assert_eq!(frame.format_info.frame_sequence, 0);
        assert_eq!(frame.format_info.total_blocks, 1);
        assert!(frame.encoded_payload.len() >= 4);
    }

    #[test]
    fn test_new_data_frame() {
        let block = BlockData {
            index: 5,
            data: vec![0x42; 100],
        };
        let frame = Frame::new_data(&block, 3, 10, 1).unwrap();
        assert_eq!(frame.format_info.frame_type, FrameType::Data);
        assert_eq!(frame.format_info.frame_sequence, 3);
        assert_eq!(frame.format_info.block_index, 5);
        assert_eq!(frame.format_info.total_blocks, 10);
        assert_eq!(frame.format_info.encoding_mode_bit, 1);
    }

    #[test]
    fn test_new_eot_frame() {
        let frame = Frame::new_eot(99).unwrap();
        assert_eq!(frame.format_info.frame_type, FrameType::EOT);
        assert_eq!(frame.format_info.frame_sequence, 99);
        assert_eq!(frame.format_info.block_index, 0);
        assert_eq!(frame.format_info.total_blocks, 0);
        assert_eq!(frame.format_info.payload_bytes, 0);
    }

    #[test]
    fn test_new_parity_frame() {
        let parity_data = vec![0x77u8; 1024];
        let frame = Frame::new_parity(5, 2, 7, 100, &parity_data, 1).unwrap();
        assert_eq!(frame.format_info.frame_type, FrameType::FecParity);
        assert_eq!(frame.format_info.frame_sequence, 5);
        // block_index = (group=2) << 16 | (parity_idx=7) = 0x00020007
        assert_eq!(frame.format_info.block_index, 0x00020007);
        assert_eq!(frame.format_info.total_blocks, 100);
        assert_eq!(frame.format_info.payload_bytes, 1024);
        assert_eq!(frame.format_info.encoding_mode_bit, 1);
    }

    #[test]
    fn test_new_data_merged_large_block_preserves_len() {
        // Regression: block.data.len() > u16::MAX (65535) must not be truncated.
        let big_data = vec![0xCDu8; 70000]; // > 65535
        let block = BlockData { index: 0, data: big_data.clone() };
        let blocks: Vec<&BlockData> = vec![&block];
        let frame = Frame::new_data_merged(&blocks, 1, 1, 1).unwrap();
        let decoded = crate::decoder::reed_solomon::rs_decode_data(&frame.encoded_payload).unwrap();
        assert_eq!(decoded.len(), 2 + 4 + 4 + 70000);
        let data_len = u32::from_le_bytes(decoded[6..10].try_into().unwrap());
        assert_eq!(data_len as usize, 70000);
        // 校验数据内容完整
        assert_eq!(&decoded[10..], &big_data[..]);
    }
}
