//! Reed-Solomon RS(255,217) 解码封装
//!
//! 将 RS 编码的数据块解码还原为原始数据。
//! 输入格式：`[4 bytes total_data_len LE] + [ceil(len/217) × 255 bytes RS blocks]`

use crate::common::constants::{RS_DATA_LEN, RS_PARITY_LEN, RS_TOTAL_LEN};
use crate::common::error::{OptransError, OptransResult};
use crate::common::reed_solomon_utils::rs_block_count;
use reed_solomon::Decoder;

/// RS(255,217) 解码器
pub struct RsDecoder {
    /// reed-solomon crate 的解码器实例（38 个校验字节）
    decoder: Decoder,
}

impl RsDecoder {
    /// 创建新的 RS(255,217) 解码器
    pub fn new() -> Self {
        RsDecoder {
            decoder: Decoder::new(RS_PARITY_LEN),
        }
    }

    /// 解码 RS 编码的数据
    ///
    /// # 参数
    /// - `encoded`: RS 编码后的数据，格式为 `[4 bytes total_len LE] + [RS blocks...]`
    ///
    /// # 返回
    /// 解码后的原始数据
    ///
    /// 任一 RS 块纠错失败即返回 `Err`——绝不把未纠错的原始块字节混入结果。
    /// 坏块混入会流入 FEC 层并阻断块级恢复（坏块被视为"已收到"永不覆盖），
    /// 使本可恢复的传输降级为失败。宁可让本帧缺席，交给块级 FEC 兜底。
    ///
    /// 几何精修裁决（v0.29）也依赖这一语义：采样网格错位时中部数据行的
    /// RS 块必然大面积纠错失败，失败被显式暴露出来作为裁决信号；
    /// 解码成功即可反证几何正确。
    pub fn decode(&self, encoded: &[u8]) -> OptransResult<Vec<u8>> {
        self.decode_inner(encoded)
    }

    fn decode_inner(&self, encoded: &[u8]) -> OptransResult<Vec<u8>> {
        if encoded.len() < 4 {
            return Err(OptransError::RsDecode(
                "编码数据太短，无法读取长度头".to_string(),
            ));
        }

        // 读取总数据长度
        let total_len = u32::from_le_bytes(encoded[..4].try_into().map_err(|_| {
            OptransError::RsDecode("读取长度头失败".to_string())
        })?) as usize;

        if total_len == 0 {
            return Ok(Vec::new());
        }

        // 计算 RS 块数
        let block_data = &encoded[4..];
        let num_blocks = block_data.len() / RS_TOTAL_LEN;

        if !block_data.len().is_multiple_of(RS_TOTAL_LEN) {
            return Err(OptransError::RsDecode(format!(
                "编码数据大小不正确: {} bytes（应为 RS 块的整数倍）",
                block_data.len()
            )));
        }

        if num_blocks == 0 {
            return Ok(Vec::new());
        }

        let expected_blocks = rs_block_count(total_len);
        if num_blocks != expected_blocks {
            return Err(OptransError::RsDecode(format!(
                "RS 块数不匹配: 期望 {} 块，实际 {} 块",
                expected_blocks, num_blocks
            )));
        }

        // 逐块解码
        let mut result = Vec::with_capacity(total_len);

        for block_idx in 0..num_blocks {
            let block_start = block_idx * RS_TOTAL_LEN;
            let block_end = block_start + RS_TOTAL_LEN;

            let block = &block_data[block_start..block_end];

            // 尝试纠错解码：decoder.correct() 返回 Result<Buffer, DecoderError>
            // Buffer 实现了 Deref<Target=[u8]>，包含 [已纠正数据 | ECC]
            // 纠错失败直接报错，绝不混入未纠错字节（见 decode 文档）
            let corrected = self.decoder.correct(block, None).map_err(|e| {
                OptransError::RsDecode(format!(
                    "RS 块 {} 纠错失败（共 {} 块）: {:?}",
                    block_idx, num_blocks, e
                ))
            })?;

            // 提取纠正后的数据部分
            let data_len = std::cmp::min(RS_DATA_LEN, total_len - block_idx * RS_DATA_LEN);
            result.extend_from_slice(&corrected[..data_len]);
        }

        Ok(result)
    }
}

impl Default for RsDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// 便捷函数：RS(255,217) 解码
pub fn rs_decode_data(encoded: &[u8]) -> OptransResult<Vec<u8>> {
    let decoder = RsDecoder::new();
    decoder.decode(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::reed_solomon::RsEncoder;

    #[test]
    fn test_rs_roundtrip_empty() {
        let data = vec![];
        let encoder = RsEncoder::new();
        let encoded = encoder.encode(&data).unwrap();

        let decoder = RsDecoder::new();
        let decoded = decoder.decode(&encoded).unwrap();

        assert_eq!(decoded, data);
    }

    #[test]
    fn test_rs_roundtrip_small() {
        let data = b"Hello, RS(255,217) encoding test!".to_vec();
        let encoder = RsEncoder::new();
        let encoded = encoder.encode(&data).unwrap();

        let decoder = RsDecoder::new();
        let decoded = decoder.decode(&encoded).unwrap();

        assert_eq!(decoded, data);
    }

    #[test]
    fn test_rs_roundtrip_exact_block() {
        let data = vec![0x55u8; RS_DATA_LEN];
        let encoder = RsEncoder::new();
        let encoded = encoder.encode(&data).unwrap();

        let decoder = RsDecoder::new();
        let decoded = decoder.decode(&encoded).unwrap();

        assert_eq!(decoded, data);
    }

    #[test]
    fn test_rs_roundtrip_multi_block() {
        let data = vec![0xCCu8; RS_DATA_LEN * 2 + 50];
        let encoder = RsEncoder::new();
        let encoded = encoder.encode(&data).unwrap();

        let decoder = RsDecoder::new();
        let decoded = decoder.decode(&encoded).unwrap();

        assert_eq!(decoded, data);
    }

    #[test]
    fn test_rs_decode_invalid_input() {
        let decoder = RsDecoder::new();
        // 不足 4 字节
        assert!(decoder.decode(&[1, 2, 3]).is_err());
    }
}
