//! Reed-Solomon RS(255,217) 编码封装
//!
//! 将原始数据编码为带纠错码的数据块。
//! 输出格式：`[4 bytes total_data_len LE] + [ceil(len/217) × 255 bytes RS blocks]`

use crate::common::constants::{RS_DATA_LEN, RS_PARITY_LEN};
use crate::common::error::OptransResult;
use crate::common::reed_solomon_utils::rs_block_count;
use reed_solomon::Encoder;

/// RS(255,217) 编码器
pub struct RsEncoder {
    /// reed-solomon crate 的编码器实例（38 个校验字节）
    encoder: Encoder,
}

impl RsEncoder {
    /// 创建新的 RS(255,217) 编码器
    pub fn new() -> Self {
        RsEncoder {
            encoder: Encoder::new(RS_PARITY_LEN),
        }
    }

    /// 对数据进行 RS(255,217) 编码
    ///
    /// reed-solomon crate 的 Encoder::encode(data) 返回 Buffer，
    /// Buffer 包含 [原始数据 | ECC 校验数据]，总长度 = data.len() + ecc_len。
    /// 我们需要将每块填充到 217 字节数据，使总输出为 255 字节。
    ///
    /// # 参数
    /// - `data`: 原始数据字节切片
    ///
    /// # 返回
    /// 编码后的数据，格式为 `[4 bytes total_len LE] + [RS blocks...]`
    pub fn encode(&self, data: &[u8]) -> OptransResult<Vec<u8>> {
        let total_len = data.len() as u32;
        let num_blocks = rs_block_count(data.len());
        if num_blocks == 0 {
            let mut output = Vec::with_capacity(4);
            output.extend_from_slice(&total_len.to_le_bytes());
            return Ok(output);
        }

        let output_size = crate::common::reed_solomon_utils::rs_encoded_size(data.len());
        let mut output = Vec::with_capacity(output_size);

        // 写入总长度头（小端序）
        output.extend_from_slice(&total_len.to_le_bytes());

        // 逐块编码
        for block_idx in 0..num_blocks {
            let start = block_idx * RS_DATA_LEN;
            let end = std::cmp::min(start + RS_DATA_LEN, data.len());

            // 构建 217 字节的数据块（不足部分填 0）
            let mut block_data = vec![0u8; RS_DATA_LEN];
            let copy_len = end - start;
            block_data[..copy_len].copy_from_slice(&data[start..end]);

            // encoder.encode() 返回 Buffer = [data(217) | ecc(38)] = 255 bytes
            let buffer = self.encoder.encode(&block_data);
            // buffer 实现了 Deref<Target=[u8]>，直接扩展
            output.extend_from_slice(&buffer[..]);
        }

        Ok(output)
    }
}

impl Default for RsEncoder {
    fn default() -> Self {
        Self::new()
    }
}

/// 便捷函数：对数据进行 RS(255,217) 编码
pub fn rs_encode(data: &[u8]) -> OptransResult<Vec<u8>> {
    let encoder = RsEncoder::new();
    encoder.encode(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::RS_TOTAL_LEN;

    #[test]
    fn test_rs_encode_empty() {
        let result = rs_encode(&[]).unwrap();
        // 应该只有 4 字节长度头（值为 0）
        assert_eq!(result.len(), 4);
        assert_eq!(u32::from_le_bytes(result[..4].try_into().unwrap()), 0);
    }

    #[test]
    fn test_rs_encode_small() {
        let data = b"Hello, World!";
        let result = rs_encode(data).unwrap();
        // 4 + ceil(13/217) * 255 = 4 + 255 = 259
        assert_eq!(result.len(), 4 + RS_TOTAL_LEN);
        let total_len = u32::from_le_bytes(result[..4].try_into().unwrap());
        assert_eq!(total_len, data.len() as u32);
    }

    #[test]
    fn test_rs_encode_exact_block() {
        let data = vec![0xAAu8; RS_DATA_LEN];
        let result = rs_encode(&data).unwrap();
        assert_eq!(result.len(), 4 + RS_TOTAL_LEN);
        let total_len = u32::from_le_bytes(result[..4].try_into().unwrap());
        assert_eq!(total_len, RS_DATA_LEN as u32);
    }

    #[test]
    fn test_rs_encode_multi_block() {
        let data = vec![0xBBu8; RS_DATA_LEN * 2 + 100];
        let result = rs_encode(&data).unwrap();
        let expected_blocks = 3; // ceil((217*2+100)/217) = ceil(534/217) = 3
        assert_eq!(result.len(), 4 + expected_blocks * RS_TOTAL_LEN);
        let total_len = u32::from_le_bytes(result[..4].try_into().unwrap());
        assert_eq!(total_len, data.len() as u32);
    }
}
