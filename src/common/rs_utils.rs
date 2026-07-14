use crate::common::constants::{RS_DATA_LEN, RS_TOTAL_LEN};

#[inline]
pub fn rs_block_count(payload_len: usize) -> usize {
    payload_len.div_ceil(RS_DATA_LEN)
}

#[inline]
pub fn rs_encoded_size(payload_len: usize) -> usize {
    let blocks = rs_block_count(payload_len);
    4 + blocks * RS_TOTAL_LEN
}

#[inline]
pub fn rs_payload_capacity(encoded_size: usize) -> usize {
    if encoded_size <= 4 {
        return 0;
    }
    let blocks = (encoded_size - 4) / RS_TOTAL_LEN;
    blocks * RS_DATA_LEN
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_payload() {
        assert_eq!(rs_encoded_size(0), 4);
        assert_eq!(rs_payload_capacity(4), 0);
    }

    #[test]
    fn test_single_block() {
        assert_eq!(rs_encoded_size(100), 4 + 255);
        assert_eq!(rs_encoded_size(217), 4 + 255);
        assert_eq!(rs_payload_capacity(259), 217);
    }

    #[test]
    fn test_multi_block() {
        assert_eq!(rs_encoded_size(218), 4 + 2 * 255);
        assert_eq!(rs_encoded_size(434), 4 + 2 * 255);
        assert_eq!(rs_payload_capacity(514), 434);
    }
}