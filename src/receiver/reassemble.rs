//! 文件重组与 SHA-256 校验模块
//!
//! 将收集到的数据块按索引排序拼接，通过 SHA-256 校验后写入文件。

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::common::error::{OptransError, OptransResult};
use crate::common::types::MetaPayload;

use log::debug;

/// 文件组装器：按块索引重组文件
pub struct FileAssembler {
    /// 文件元数据
    meta: MetaPayload,
    /// 数据块集合：block_index → block_data
    blocks: HashMap<u32, Vec<u8>>,
}

impl FileAssembler {
    /// 使用元数据创建新的文件组装器
    pub fn new(meta: MetaPayload) -> Self {
        FileAssembler {
            meta,
            blocks: HashMap::new(),
        }
    }

    /// 添加一个数据块
    pub fn add_block(&mut self, index: u32, data: Vec<u8>) {
        self.blocks.insert(index, data);
    }

    /// 完成组装：排序拼接 → SHA-256 校验 → 返回完整文件内容
    ///
    /// # 返回
    /// 验证通过的文件内容（字节向量）
    pub fn finalize(self) -> OptransResult<Vec<u8>> {
        let total_blocks = self.meta.total_blocks as usize;

        // 检查块完整性
        if self.blocks.len() < total_blocks {
            let mut missing = Vec::new();
            for i in 0..(total_blocks as u32) {
                if !self.blocks.contains_key(&i) {
                    missing.push(i);
                }
            }
            return Err(OptransError::IncompleteTransfer {
                missing,
            });
        }

        // 按索引排序拼接
        let mut data = Vec::with_capacity(self.meta.file_size as usize);

        for i in 0..(total_blocks as u32) {
            if let Some(block_data) = self.blocks.get(&i) {
                data.extend_from_slice(block_data);
            }
        }

        // 按 file_size 截断：末块可能因 FEC 恢复而补零到 block_size，
        // 或块本身带 padding，必须截掉尾部填充后再校验
        let file_size = self.meta.file_size as usize;
        if data.len() < file_size {
            return Err(OptransError::IncompleteTransfer {
                missing: (0..(total_blocks as u32))
                    .filter(|i| !self.blocks.contains_key(i))
                    .collect(),
            });
        }
        data.truncate(file_size);

        // SHA-256 校验
        let mut hasher = Sha256::new();
        hasher.update(&data);
        let hash_result = hasher.finalize();

        let mut computed_hash = [0u8; 32];
        computed_hash.copy_from_slice(&hash_result);

        if computed_hash != self.meta.sha256 {
            return Err(OptransError::ChecksumMismatch {
                expected: hex_str(&self.meta.sha256),
                actual: hex_str(&computed_hash),
            });
        }

        Ok(data)
    }

    /// 完成组装并写入文件
    pub fn finalize_to_file(self, output_path: &Path) -> OptransResult<()> {
        let data = self.finalize()?;
        fs::write(output_path, &data)?;
        debug!(
            "文件已写入: {:?} ({} bytes)",
            output_path,
            data.len()
        );
        Ok(())
    }
}

/// 将字节切片格式化为十六进制字符串
fn hex_str(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<_>>()
        .join("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::types::MetaPayload;

    fn meta_for(file_size: u64, total_blocks: u32, data: &[u8]) -> MetaPayload {
        let mut hasher = Sha256::new();
        hasher.update(data);
        let hash = hasher.finalize();
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&hash);
        MetaPayload {
            file_name: "test.bin".to_string(),
            file_size,
            sha256,
            total_blocks,
            fec_k: 128,
            fec_m: 64,
            fec_groups: 1,
            fec_shard_size: 8,
        }
    }

    #[test]
    fn test_finalize_truncates_last_block_padding() {
        // block_size=8、file_size=13：末块含 3 字节补零（FEC 恢复场景）
        let content = b"hello world!!"; // 13 bytes
        let mut block0 = Vec::from(&content[..8]);
        block0.resize(8, 0);
        let mut block1 = Vec::from(&content[8..]);
        block1.resize(8, 0); // 补零到 block_size

        let meta = meta_for(13, 2, content);
        let mut asm = FileAssembler::new(meta);
        asm.add_block(0, block0);
        asm.add_block(1, block1);
        let out = asm.finalize().expect("补零末块必须按 file_size 截断后通过 SHA");
        assert_eq!(out, content.to_vec());
    }

    #[test]
    fn test_finalize_short_data_fails() {
        // 拼接后长度 < file_size：视为缺块
        let content = b"hello world!!";
        let meta = meta_for(20, 2, content);
        let mut asm = FileAssembler::new(meta);
        asm.add_block(0, b"hello wo".to_vec());
        asm.add_block(1, b"rld!!".to_vec());
        assert!(asm.finalize().is_err());
    }
}
