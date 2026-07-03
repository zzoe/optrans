//! 帧构建模块
//!
//! 组装元数据帧、数据帧、P 帧和 EOT（传输结束）帧。
//! 每帧包含格式信息（FormatInfo）和 RS 编码后的载荷数据。
//! v0.2 新增：P 帧构建、FrameKind 支持。

use crate::common::constants::MAX_FILE_NAME_LEN;
use crate::common::error::{OptransError, OptransResult};
use crate::common::types::{
    BlockData, DiffEntry, FileManifest, FormatInfo, FrameKind, FrameType,
};
use crate::encoder::reed_solomon::RsEncoder;

/// 构建完成的帧（含格式信息 + RS 编码后的数据）
#[derive(Debug, Clone)]
pub struct Frame {
    /// 帧格式信息
    pub format_info: FormatInfo,
    /// RS 编码后的载荷数据
    pub encoded_payload: Vec<u8>,
    /// v0.2: 帧种类（I 或 P）
    pub frame_kind: FrameKind,
}

impl Frame {
    /// 构建元数据帧
    ///
    /// 元数据帧载荷包含：文件名长度(1B) + 文件名(UTF-8) + 文件大小(8B LE) + SHA-256(32B) + 总块数(4B LE)
    /// 元数据帧始终为 I 帧、二值模式。
    pub fn new_metadata(manifest: &FileManifest, seq: u16) -> OptransResult<Frame> {
        let file_name_bytes = manifest.file_name.as_bytes();
        if file_name_bytes.len() > MAX_FILE_NAME_LEN {
            return Err(OptransError::InvalidArgument(format!(
                "文件名过长：{} 字节（最大 {} 字节）",
                file_name_bytes.len(),
                MAX_FILE_NAME_LEN
            )));
        }

        // 构建载荷：name_len(1B) + name + file_size(8B LE) + sha256(32B) + total_blocks(4B LE)
        let payload_len = 1 + file_name_bytes.len() + 8 + 32 + 4;
        let mut payload = Vec::with_capacity(payload_len);

        payload.push(file_name_bytes.len() as u8);
        payload.extend_from_slice(file_name_bytes);
        payload.extend_from_slice(&manifest.file_size.to_le_bytes());
        payload.extend_from_slice(&manifest.sha256);
        let total_blocks = manifest.blocks.len() as u32;
        payload.extend_from_slice(&total_blocks.to_le_bytes());

        let info = FormatInfo::new_meta(seq, total_blocks, payload.len() as u16);

        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(&payload)?;

        Ok(Frame {
            format_info: info,
            encoded_payload,
            frame_kind: FrameKind::I,
        })
    }

    /// 构建数据帧（I 帧）
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
            payload.len() as u16,
            FrameKind::I,
            encoding_mode_bit,
        );

        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(&payload)?;

        Ok(Frame {
            format_info: info,
            encoded_payload,
            frame_kind: FrameKind::I,
        })
    }

    /// 构建多块合并数据帧（I 帧，v0.9）
    ///
    /// 将多个数据块打包到一个帧中，填满 payload 容量。
    /// 载荷格式：[count: u16 LE][entries...]
    /// 每个 entry：[block_index: u32 LE][data_len: u16 LE][data: data_len bytes]
    pub fn new_data_merged(
        blocks: &[&BlockData],
        seq: u16,
        total_blocks: u32,
        encoding_mode_bit: u8,
    ) -> OptransResult<Frame> {
        let payload_len: usize = 2 + blocks.iter()
            .map(|b| 4 + 2 + b.data.len())
            .sum::<usize>();

        let mut payload = Vec::with_capacity(payload_len);
        let count = blocks.len() as u16;
        payload.extend_from_slice(&count.to_le_bytes());

        for block in blocks {
            payload.extend_from_slice(&block.index.to_le_bytes());
            let data_len = block.data.len() as u16;
            payload.extend_from_slice(&data_len.to_le_bytes());
            payload.extend_from_slice(&block.data);
        }

        let first_index = blocks.first().map(|b| b.index).unwrap_or(0);
        let info = FormatInfo::new_data(
            seq, first_index, total_blocks,
            payload.len() as u16, FrameKind::I, encoding_mode_bit,
        );

        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(&payload)?;

        Ok(Frame { format_info: info, encoded_payload, frame_kind: FrameKind::I })
    }

    /// 构建元数据+数据合并帧（v0.10）
    ///
    /// 首条 entry 为元数据（block_index = META_BLOCK_INDEX = 0xFFFFFFFF），
    /// 后续 entries 为正常数据块。用于小文件单帧传输。
    pub fn new_merged_with_meta(
        seq: u16,
        total_blocks: u32,
        encoding_mode_bit: u8,
        meta_payload: &[u8],
        blocks: &[&BlockData],
    ) -> OptransResult<Frame> {
        let meta_entry_len = 4 + 2 + meta_payload.len(); // index(4) + len(2) + data
        let data_len: usize = blocks.iter().map(|b| 4 + 2 + b.data.len()).sum();
        let payload_len = 2 + meta_entry_len + data_len; // 2B count

        let mut payload = Vec::with_capacity(payload_len);
        let count = 1u16 + blocks.len() as u16;
        payload.extend_from_slice(&count.to_le_bytes());

        // 元数据 entry: block_index = 0xFFFFFFFF
        payload.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes());
        payload.extend_from_slice(&(meta_payload.len() as u16).to_le_bytes());
        payload.extend_from_slice(meta_payload);

        // 数据 entries
        for block in blocks {
            payload.extend_from_slice(&block.index.to_le_bytes());
            let dlen = block.data.len() as u16;
            payload.extend_from_slice(&dlen.to_le_bytes());
            payload.extend_from_slice(&block.data);
        }

        let info = FormatInfo::new_data(
            seq, 0, total_blocks,
            payload.len() as u16, FrameKind::I, encoding_mode_bit,
        );

        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(&payload)?;

        Ok(Frame { format_info: info, encoded_payload, frame_kind: FrameKind::I })
    }

    /// 构建 P 帧（差分帧）
    ///
    /// P 帧 payload 结构（RS 编码前）：
    /// ```text
    /// base_frame_seq: 2 bytes LE
    /// module_count:   2 bytes LE
    /// module_entries: module_count × 3 bytes (module_index:2B LE + module_value:1B)
    /// ```
    pub fn new_p_frame(
        diff_entries: &[DiffEntry],
        seq: u16,
        base_frame_seq: u16,
        block_index: u32,
        total_blocks: u32,
        encoding_mode_bit: u8,
    ) -> OptransResult<Frame> {
        let module_count = diff_entries.len() as u16;
        let payload_len = 2 + 2 + module_count as usize * 3;
        let mut payload = Vec::with_capacity(payload_len);

        // base_frame_seq (2 bytes LE)
        payload.extend_from_slice(&base_frame_seq.to_le_bytes());
        // module_count (2 bytes LE)
        payload.extend_from_slice(&module_count.to_le_bytes());
        // diff entries
        for entry in diff_entries {
            payload.extend_from_slice(&entry.module_index.to_le_bytes());
            payload.push(entry.module_value);
        }

        let info = FormatInfo::new_data(
            seq,
            block_index,
            total_blocks,
            payload.len() as u16,
            FrameKind::P,
            encoding_mode_bit,
        );

        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(&payload)?;

        Ok(Frame {
            format_info: info,
            encoded_payload,
            frame_kind: FrameKind::P,
        })
    }

    /// 构建 EOT（传输结束）帧
    ///
    /// EOT 帧没有载荷数据，始终为 I 帧。
    pub fn new_eot(seq: u16) -> OptransResult<Frame> {
        let info = FormatInfo::new_eot(seq);
        let encoder = RsEncoder::new();
        let encoded_payload = encoder.encode(&[])?;

        Ok(Frame {
            format_info: info,
            encoded_payload,
            frame_kind: FrameKind::I,
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
    use crate::common::constants::BLOCK_SIZE;
    use std::path::PathBuf;

    fn make_test_manifest() -> FileManifest {
        FileManifest {
            path: PathBuf::from("test.dat"),
            file_name: "test.dat".to_string(),
            file_size: 4096,
            sha256: [0xAB; 32],
            blocks: vec![BlockData {
                index: 0,
                data: vec![0x00; BLOCK_SIZE],
            }],
        }
    }

    #[test]
    fn test_new_metadata_frame() {
        let manifest = make_test_manifest();
        let frame = Frame::new_metadata(&manifest, 0).unwrap();
        assert_eq!(frame.format_info.frame_type, FrameType::Meta);
        assert_eq!(frame.format_info.frame_sequence, 0);
        assert_eq!(frame.format_info.total_blocks, 1);
        assert_eq!(frame.frame_kind, FrameKind::I);
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
        assert_eq!(frame.frame_kind, FrameKind::I);
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
        assert_eq!(frame.frame_kind, FrameKind::I);
    }

    #[test]
    fn test_new_p_frame() {
        let diff_entries = vec![
            DiffEntry {
                module_index: 10,
                module_value: 128,
            },
            DiffEntry {
                module_index: 20,
                module_value: 64,
            },
        ];
        let frame = Frame::new_p_frame(&diff_entries, 5, 3, 0, 10, 1).unwrap();

        assert_eq!(frame.frame_kind, FrameKind::P);
        assert_eq!(frame.format_info.frame_kind, FrameKind::P);
        assert_eq!(frame.format_info.encoding_mode_bit, 1);
        assert_eq!(frame.format_info.frame_sequence, 5);
        // P 帧 payload: 2B(base) + 2B(count) + 2×3B(entries) = 10B
        // RS 编码后: 4B header + 255B = 259B
        assert_eq!(frame.encoded_payload.len(), 4 + crate::common::constants::RS_TOTAL_LEN);
    }
}
