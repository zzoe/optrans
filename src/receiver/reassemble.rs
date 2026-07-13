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
