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

/// 文件流式扫描结果（M10：第一遍读，不驻留文件内容）
///
/// 只记录文件元信息与每块长度（8B/块），块数据由第二遍读按需加载。
pub struct FileScan {
    /// 文件名（不含目录）
    pub file_name: String,
    /// 文件总字节数
    pub file_size: u64,
    /// 全文件 SHA-256
    pub sha256: [u8; 32],
    /// 每块字节数（最后一块可能小于 block_size；空文件为空表）
    pub block_sizes: Vec<usize>,
}

impl FileScan {
    /// 总块数
    pub fn total_blocks(&self) -> u32 {
        self.block_sizes.len() as u32
    }
}

/// 流式扫描文件：逐块读取计算 SHA-256 与块长度表，不驻留块数据（M10）
///
/// 与 [`read_file_with_block_size`] 的分块语义一致（`block_size == 0` 视为 0 块）。
pub fn scan_file(path: &Path, block_size: usize) -> OptransResult<FileScan> {
    if !path.exists() {
        return Err(OptransError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("文件不存在: {:?}", path),
        )));
    }

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| OptransError::InvalidArgument(format!("无法获取文件名: {:?}", path)))?;

    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut block_sizes: Vec<usize> = Vec::new();
    let mut file_size = 0u64;

    if block_size > 0 {
        let mut buf = vec![0u8; block_size];
        loop {
            // 与 chunk_data_with_size 一致：按 block_size 切块，末块取剩余
            let mut filled = 0usize;
            while filled < block_size {
                let n = {
                    use std::io::Read;
                    file.read(&mut buf[filled..])?
                };
                if n == 0 {
                    break;
                }
                filled += n;
            }
            if filled == 0 {
                break;
            }
            hasher.update(&buf[..filled]);
            block_sizes.push(filled);
            file_size += filled as u64;
            if filled < block_size {
                break;
            }
        }
    }

    let hash_result = hasher.finalize();
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&hash_result);

    Ok(FileScan {
        file_name,
        file_size,
        sha256,
        block_sizes,
    })
}

/// 按块序号范围读取块数据（第二遍读，M10）
///
/// 返回 `[start_block, start_block + count)` 的块列表；超出文件末尾的块被截断。
pub fn read_blocks_range(
    path: &Path,
    block_size: usize,
    start_block: u32,
    count: u32,
) -> OptransResult<Vec<BlockData>> {
    let mut file = fs::File::open(path)?;
    {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(start_block as u64 * block_size as u64))?;
    }

    let mut blocks = Vec::with_capacity(count as usize);
    let mut buf = vec![0u8; block_size.max(1)];
    for i in 0..count {
        let mut filled = 0usize;
        if block_size > 0 {
            use std::io::Read;
            while filled < block_size {
                let n = file.read(&mut buf[filled..])?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
        }
        if filled == 0 {
            break;
        }
        blocks.push(BlockData {
            index: start_block + i,
            data: buf[..filled].to_vec(),
        });
        if filled < block_size {
            break;
        }
    }
    Ok(blocks)
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
