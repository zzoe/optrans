//! 数据掩码模块
//!
//! 对 RS 编码后的字节序列应用 XOR 掩码，打散大片同色区域，
//! 提升采样稳定性。掩码图案基于字节序号的伪随机函数，
//! 编解码双方使用相同公式，无需传输掩码信息。
//!
//! 掩码只作用于数据区（RS 编码后的 payload），
//! 不影响 finder pattern 和格式信息区。

/// 掩码种子（固定值，编解码双方必须一致）
const MASK_SEED: u32 = 0x0F_12_34_56;

/// 简单的 32 位伪随机数生成器（xorshift32）
///
/// 给定序号 n，生成一个确定的伪随机字节。
/// 编解码双方使用相同公式，保证可逆。
fn mask_byte_at(index: usize) -> u8 {
    let mut state = MASK_SEED.wrapping_add(index as u32);
    // xorshift32
    state ^= state << 13;
    state ^= state >> 17;
    state ^= state << 5;
    (state & 0xFF) as u8
}

/// 对字节序列应用掩码（XOR）
///
/// 编码端调用：`let masked = apply_mask(&encoded_payload);`
/// 解码端调用：`let unmasked = apply_mask(&sampled_payload);` （XOR 自反）
pub fn apply_mask(data: &[u8]) -> Vec<u8> {
    data.iter()
        .enumerate()
        .map(|(i, &b)| b ^ mask_byte_at(i))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_roundtrip() {
        let original = vec![0x00, 0xFF, 0xAA, 0x55, 0x12, 0x34, 0x56, 0x78];
        let masked = apply_mask(&original);
        // 掩码后应与原始数据不同（除非极巧合）
        assert_ne!(masked, original);
        // 反向 XOR 应恢复原始数据
        let restored = apply_mask(&masked);
        assert_eq!(restored, original);
    }

    #[test]
    fn test_mask_empty() {
        let empty: Vec<u8> = vec![];
        let masked = apply_mask(&empty);
        assert!(masked.is_empty());
    }

    #[test]
    fn test_mask_deterministic() {
        // 相同输入应产生相同掩码（编解码双方一致性）
        let data = vec![0x42; 100];
        let masked1 = apply_mask(&data);
        let masked2 = apply_mask(&data);
        assert_eq!(masked1, masked2);
    }

    #[test]
    fn test_mask_disperses_uniform() {
        // 全 0 数据掩码后应分散为不同值，避免大片同色
        let data = vec![0x00; 32];
        let masked = apply_mask(&data);
        let unique_count = masked.iter().collect::<std::collections::HashSet<_>>().len();
        // 32 个字节应至少有 10 个不同值（伪随机性）
        assert!(unique_count >= 10, "掩码分散性不足: 仅 {} 个不同值", unique_count);
    }
}
