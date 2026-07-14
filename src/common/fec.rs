//! 块级 FEC（前向纠错）编解码
//!
//! 将数据块作为 RS 纠删码的分片：每组 k 个数据分片 + m 个校验分片，
//! 接收端收到任意 k 个分片即可恢复该组全部数据分片。
//!
//! 与 `encoder/reed_solomon.rs` 的帧内字节级 RS(255,217) 互补：
//! - 帧内 RS：纠正单帧内的字节错误（捕获噪声）
//! - 块级 FEC：纠正整块丢失（Horizon 协议层漏帧）
//!
//! ## 实现
//!
//! 复用 `reed-solomon` crate（无 build script，无依赖链兼容性问题）。
//! 块级 FEC 的符号是"分片的同一字节位置"：对每个字节位置 i 独立做
//! 一次 RS(k+m, k) 编码/解码。Encoder/Decoder 实例复用，避免重建生成多项式。
//!
//! RS(255,k) 上限 k+m ≤ 255，本项目 k=128 m=64 → 192 < 256（decoder.assert）。

use crate::common::constants::{FEC_DATA_SHARDS, FEC_PARITY_SHARDS};
use crate::common::error::{OptransError, OptransResult};
use reed_solomon::{Decoder, Encoder};

/// FEC 编码器：对一组等长数据分片生成校验分片
pub struct FecEncoder {
    /// 复用 reed-solomon 编码器（ecc_len = m）
    encoder: Encoder,
}

impl FecEncoder {
    /// 创建默认参数（k=128, m=64）的 FEC 编码器
    pub fn new() -> OptransResult<Self> {
        Self::with_params(FEC_DATA_SHARDS, FEC_PARITY_SHARDS)
    }

    /// 创建指定参数的 FEC 编码器
    pub fn with_params(data_shards: usize, parity_shards: usize) -> OptransResult<Self> {
        if data_shards + parity_shards > 255 {
            return Err(OptransError::InvalidArgument(format!(
                "FEC 参数超出 GF(2^8) 上限: k={} + m={} = {} > 255",
                data_shards,
                parity_shards,
                data_shards + parity_shards
            )));
        }
        Ok(FecEncoder {
            encoder: Encoder::new(parity_shards),
        })
    }

    /// 对一组数据分片生成校验分片
    ///
    /// # 参数
    /// - `data_shards`: k 个等长数据分片（每个长度 = shard_size）
    ///
    /// # 返回
    /// - m 个校验分片（每个长度 = shard_size）
    ///
    /// # 算法
    /// 逐字节位置 i 做 RS(k+m, k) 编码：取 k 个数据分片在第 i 列的字节
    /// 作为消息，encode 生成 m 个校验字节，写入 m 个校验分片的第 i 列。
    pub fn encode(&self, data_shards: &[Vec<u8>]) -> OptransResult<Vec<Vec<u8>>> {
        let k = FEC_DATA_SHARDS;
        let m = FEC_PARITY_SHARDS;

        if data_shards.len() != k {
            return Err(OptransError::InvalidArgument(format!(
                "数据分片数不匹配: 期望 {}，实际 {}",
                k,
                data_shards.len()
            )));
        }

        let shard_size = data_shards[0].len();
        for (i, s) in data_shards.iter().enumerate() {
            if s.len() != shard_size {
                return Err(OptransError::InvalidArgument(format!(
                    "数据分片长度不一致: 分片[0]={}, 分片[{}]={}",
                    shard_size,
                    i,
                    s.len()
                )));
            }
        }

        // 预分配 m 个校验分片
        let mut parity: Vec<Vec<u8>> = vec![vec![0u8; shard_size]; m];
        // 列缓冲：k 个数据字节
        let mut col = vec![0u8; k];

        for i in 0..shard_size {
            // 收集第 i 列的 k 个数据字节
            for (j, shard) in data_shards.iter().enumerate().take(k) {
                col[j] = shard[i];
            }
            // RS 编码：返回 [data(k) | ecc(m)]
            let encoded = self.encoder.encode(&col);
            // 提取后 m 字节作为该列的校验
            for (p, parity_shard) in parity.iter_mut().enumerate().take(m) {
                parity_shard[i] = encoded[k + p];
            }
        }

        Ok(parity)
    }
}

impl Default for FecEncoder {
    fn default() -> Self {
        Self::new().expect("FEC 编码器初始化失败")
    }
}

/// FEC 解码器：从已收到的分片（数据+校验混合）恢复缺失分片
pub struct FecDecoder {
    /// 复用 reed-solomon 解码器（ecc_len = m）
    decoder: Decoder,
}

impl FecDecoder {
    /// 创建默认参数（k=128, m=64）的 FEC 解码器
    pub fn new() -> OptransResult<Self> {
        Self::with_params(FEC_DATA_SHARDS, FEC_PARITY_SHARDS)
    }

    /// 创建指定参数的 FEC 解码器
    pub fn with_params(data_shards: usize, parity_shards: usize) -> OptransResult<Self> {
        if data_shards + parity_shards > 255 {
            return Err(OptransError::InvalidArgument(format!(
                "FEC 参数超出 GF(2^8) 上限: k={} + m={} = {} > 255",
                data_shards,
                parity_shards,
                data_shards + parity_shards
            )));
        }
        Ok(FecDecoder {
            decoder: Decoder::new(parity_shards),
        })
    }

    /// 从已收到的分片恢复全部数据分片
    ///
    /// # 参数
    /// - `shards`: 长度 = k+m 的 Option 数组，Some 表示已收到，None 表示缺失
    ///
    /// # 返回
    /// - 恢复后的 k 个数据分片
    ///
    /// # 算法
    /// 逐字节位置 i 做 RS(k+m, k) 解码：构造 k+m 字节的 msg（缺失位置填 0），
    /// 将缺失位置索引作为 erasures 传给 decoder.correct，恢复后取前 k 字节。
    pub fn reconstruct_data(
        &self,
        shards: &mut [Option<Vec<u8>>],
    ) -> OptransResult<Vec<Vec<u8>>> {
        let k = FEC_DATA_SHARDS;
        let m = FEC_PARITY_SHARDS;

        if shards.len() != k + m {
            return Err(OptransError::InvalidArgument(format!(
                "分片总数不匹配: 期望 {}，实际 {}",
                k + m,
                shards.len()
            )));
        }

        let present = shards.iter().filter(|s| s.is_some()).count();
        if present < k {
            return Err(OptransError::RsDecode(format!(
                "FEC 恢复失败: 已收到 {} 分片，至少需要 {} 分片",
                present, k
            )));
        }

        let shard_size = shards
            .iter()
            .find_map(|s| s.as_ref())
            .map(|s| s.len())
            .ok_or_else(|| OptransError::RsDecode("无可用分片确定 shard_size".to_string()))?;

        // 收集缺失位置索引（作为 erasures）
        let erasures: Vec<u8> = shards
            .iter()
            .enumerate()
            .filter_map(|(i, s)| if s.is_none() { Some(i as u8) } else { None })
            .collect();

        // 恢复后的 k 个数据分片
        let mut recovered: Vec<Vec<u8>> = vec![vec![0u8; shard_size]; k];

        // 若无缺失，直接复制数据分片
        if erasures.is_empty() {
            for (i, shard) in shards.iter().enumerate().take(k) {
                if let Some(s) = shard {
                    recovered[i] = s.clone();
                }
            }
            return Ok(recovered);
        }

        // 逐列解码
        let mut msg = vec![0u8; k + m];
        for i in 0..shard_size {
            // 构造 msg：已收分片取字节，缺失位置填 0
            for (j, shard) in shards.iter().enumerate() {
                msg[j] = shard.as_ref().map(|s| s[i]).unwrap_or(0);
            }
            // 解码（带已知 erasures）
            let corrected = self
                .decoder
                .correct(&msg, Some(&erasures))
                .map_err(|e| OptransError::RsDecode(format!("FEC 重构失败: {:?}", e)))?;
            // 取前 k 字节作为数据
            for (j, rec) in recovered.iter_mut().enumerate().take(k) {
                rec[i] = corrected[j];
            }
        }

        // 将恢复的分片写回 shards（便于区分已恢复与原有）
        for &pos in &erasures {
            if (pos as usize) < k {
                shards[pos as usize] = Some(recovered[pos as usize].clone());
            }
        }

        Ok(recovered)
    }
}

impl Default for FecDecoder {
    fn default() -> Self {
        Self::new().expect("FEC 解码器初始化失败")
    }
}

/// 计算给定数据块数需要的 FEC 组数
///
/// 每组最多 k 个数据分片，不足 k 的最后一组也算一组
pub fn fec_group_count(total_blocks: usize) -> u32 {
    let k = FEC_DATA_SHARDS;
    ((total_blocks + k - 1) / k) as u32
}

/// 计算数据块所属的 FEC 组号和组内索引
pub fn block_to_group(block_index: u32) -> (u16, u16) {
    let k = FEC_DATA_SHARDS as u32;
    ((block_index / k) as u16, (block_index % k) as u16)
}

/// 计算 FEC 组号和组内索引对应的数据块索引
pub fn group_to_block(group_idx: u16, in_group_idx: u16) -> u32 {
    (group_idx as u32) * (FEC_DATA_SHARDS as u32) + (in_group_idx as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fec_encode_decode_no_loss() {
        // 无丢失：直接读取数据分片
        let encoder = FecEncoder::new().unwrap();
        let k = FEC_DATA_SHARDS;
        let shard_size = 256;
        let data: Vec<Vec<u8>> = (0..k)
            .map(|i| vec![(i % 256) as u8; shard_size])
            .collect();

        let parity = encoder.encode(&data).unwrap();
        assert_eq!(parity.len(), FEC_PARITY_SHARDS);

        // 无丢失，所有分片都在
        let mut shards: Vec<Option<Vec<u8>>> = data.into_iter().map(Some).collect();
        shards.extend(parity.into_iter().map(Some));

        let decoder = FecDecoder::new().unwrap();
        let recovered = decoder.reconstruct_data(&mut shards).unwrap();
        assert_eq!(recovered.len(), k);
        for (i, s) in recovered.iter().enumerate() {
            assert_eq!(s, &vec![(i % 256) as u8; shard_size]);
        }
    }

    #[test]
    fn test_fec_recover_data_loss() {
        // 丢失部分数据分片，用校验分片恢复
        let encoder = FecEncoder::new().unwrap();
        let k = FEC_DATA_SHARDS;
        let shard_size = 128;
        let data: Vec<Vec<u8>> = (0..k)
            .map(|i| vec![((i * 7) % 256) as u8; shard_size])
            .collect();

        let parity = encoder.encode(&data).unwrap();

        // 构造全部分片
        let mut shards: Vec<Option<Vec<u8>>> = data.into_iter().map(Some).collect();
        shards.extend(parity.into_iter().map(Some));

        // 丢失 30 个数据分片（< m=64，可恢复）
        for i in [0, 1, 5, 10, 50, 100, 127] {
            shards[i] = None;
        }

        let decoder = FecDecoder::new().unwrap();
        let recovered = decoder.reconstruct_data(&mut shards).unwrap();
        for (i, s) in recovered.iter().enumerate() {
            assert_eq!(s, &vec![((i * 7) % 256) as u8; shard_size], "分片 {} 恢复错误", i);
        }
    }

    #[test]
    fn test_fec_recover_parity_loss() {
        // 丢失部分校验分片和数据分片的混合，仍可恢复
        let encoder = FecEncoder::new().unwrap();
        let k = FEC_DATA_SHARDS;
        let shard_size = 64;
        let data: Vec<Vec<u8>> = (0..k)
            .map(|i| vec![i as u8; shard_size])
            .collect();

        let parity = encoder.encode(&data).unwrap();

        let mut shards: Vec<Option<Vec<u8>>> = data.into_iter().map(Some).collect();
        shards.extend(parity.into_iter().map(Some));

        // 丢失 10 个数据 + 20 个校验 = 30 个，< k=128 可恢复
        for i in 0..10 {
            shards[i] = None;
        }
        for i in k..k + 20 {
            shards[i] = None;
        }

        let decoder = FecDecoder::new().unwrap();
        let recovered = decoder.reconstruct_data(&mut shards).unwrap();
        for (i, s) in recovered.iter().enumerate() {
            assert_eq!(s, &vec![i as u8; shard_size]);
        }
    }

    #[test]
    fn test_fec_too_many_loss() {
        // 丢失超过 m 个，无法恢复
        let encoder = FecEncoder::new().unwrap();
        let k = FEC_DATA_SHARDS;
        let shard_size = 32;
        let data: Vec<Vec<u8>> = (0..k).map(|_| vec![0u8; shard_size]).collect();
        let parity = encoder.encode(&data).unwrap();

        let mut shards: Vec<Option<Vec<u8>>> = data.into_iter().map(Some).collect();
        shards.extend(parity.into_iter().map(Some));

        // 丢失 m+1 = 65 个数据分片
        for i in 0..(FEC_PARITY_SHARDS + 1) {
            shards[i] = None;
        }

        let decoder = FecDecoder::new().unwrap();
        assert!(decoder.reconstruct_data(&mut shards).is_err());
    }

    #[test]
    fn test_fec_group_helpers() {
        // group 计算辅助函数
        assert_eq!(fec_group_count(0), 0);
        assert_eq!(fec_group_count(1), 1);
        assert_eq!(fec_group_count(128), 1);
        assert_eq!(fec_group_count(129), 2);
        assert_eq!(fec_group_count(256), 2);

        assert_eq!(block_to_group(0), (0, 0));
        assert_eq!(block_to_group(127), (0, 127));
        assert_eq!(block_to_group(128), (1, 0));
        assert_eq!(block_to_group(256), (2, 0));

        assert_eq!(group_to_block(0, 0), 0);
        assert_eq!(group_to_block(1, 0), 128);
        assert_eq!(group_to_block(2, 10), 266);
    }
}
