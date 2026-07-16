//! 文件读取、SHA-256 计算与分块
//!
//! 读取文件并生成 FileManifest（文件元信息 + 数据块列表）。

use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::common::error::{OptransError, OptransResult};
use crate::common::types::{BlockData, FileManifest};

/// 默认文件分块大小（字节）
const BLOCK_SIZE: usize = 32768;

/// 读取文件并生成 FileManifest（使用默认 BLOCK_SIZE）
pub fn read_file(path: &Path) -> OptransResult<FileManifest> {
    read_file_with_block_size(path, BLOCK_SIZE)
}

/// 读取文件并生成 FileManifest，使用自定义块大小
///
/// 流程：
/// 1. 读取文件全部内容到内存
/// 2. 计算 SHA-256 哈希
/// 3. 按指定的 block_size 分块
///
/// # 参数
/// - `path`: 文件路径
/// - `block_size`: 每块最大字节数（payload 大小，不含 block_index 开销）
///
/// # 返回
/// 包含文件元信息和数据块列表的 FileManifest
pub fn read_file_with_block_size(path: &Path, block_size: usize) -> OptransResult<FileManifest> {
    // 检查文件是否存在
    if !path.exists() {
        return Err(OptransError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("文件不存在: {:?}", path),
        )));
    }

    // 获取文件名
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| OptransError::InvalidArgument(format!("无法获取文件名: {:?}", path)))?;

    // 读取文件内容
    let data = fs::read(path)?;
    let file_size = data.len() as u64;

    // 计算 SHA-256
    let mut hasher = Sha256::new();
    hasher.update(&data);
    let hash_result = hasher.finalize();
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&hash_result);

    // 分块（使用自定义块大小）
    let blocks = chunk_data_with_size(&data, block_size);

    Ok(FileManifest {
        path: path.to_path_buf(),
        file_name,
        file_size,
        sha256,
        blocks,
    })
}

/// 将数据按指定大小分块
fn chunk_data_with_size(data: &[u8], block_size: usize) -> Vec<BlockData> {
    if block_size == 0 {
        return Vec::new();
    }
    let mut blocks = Vec::new();
    let mut offset = 0usize;
    let mut index = 0u32;

    while offset < data.len() {
        let end = std::cmp::min(offset + block_size, data.len());
        let block_data = data[offset..end].to_vec();

        blocks.push(BlockData {
            index,
            data: block_data,
        });

        offset = end;
        index += 1;
    }

    blocks
}

/// 计算数据的 SHA-256 哈希值
pub fn compute_sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_read_file_basic() {
        // 创建临时文件（大于 BLOCK_SIZE 以测试分块）
        let dir = std::env::temp_dir();
        let file_path = dir.join("optrans_test_chunker.dat");
        let test_size = BLOCK_SIZE + 1000; // 确保产生 2 块
        let test_data = vec![0x41u8; test_size];

        {
            let mut f = fs::File::create(&file_path).unwrap();
            f.write_all(&test_data).unwrap();
        }

        let manifest = read_file(&file_path).unwrap();
        assert_eq!(manifest.file_name, "optrans_test_chunker.dat");
        assert_eq!(manifest.file_size, test_size as u64);
        assert_eq!(manifest.blocks.len(), 2); // ceil((BLOCK_SIZE+1000)/BLOCK_SIZE) = 2
        assert_eq!(manifest.blocks[0].index, 0);
        assert_eq!(manifest.blocks[0].data.len(), BLOCK_SIZE);
        assert_eq!(manifest.blocks[1].index, 1);
        assert_eq!(manifest.blocks[1].data.len(), test_size - BLOCK_SIZE);

        // SHA-256 应一致
        let expected_hash = compute_sha256(&test_data);
        assert_eq!(manifest.sha256, expected_hash);

        // 清理
        let _ = fs::remove_file(&file_path);
    }

    #[test]
    fn test_read_file_not_found() {
        let result = read_file(Path::new("nonexistent_file_xyz.dat"));
        assert!(result.is_err());
    }

    #[test]
    fn test_chunk_data_empty() {
        let blocks = chunk_data_with_size(&[], BLOCK_SIZE);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_chunk_data_exact_block() {
        let data = vec![0u8; BLOCK_SIZE];
        let blocks = chunk_data_with_size(&data, BLOCK_SIZE);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].data.len(), BLOCK_SIZE);
    }
}
