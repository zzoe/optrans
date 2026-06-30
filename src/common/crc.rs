//! CRC 校验算法
//!
//! 提供 CRC-16/XMODEM 校验算法实现。

/// 计算 CRC-16/XMODEM 校验值
///
/// 多项式：0x1021，初始值：0x0000
pub fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc: u16 = 0x0000;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc16_xmodem() {
        assert_eq!(crc16_xmodem(&[]), 0x0000);
        assert_eq!(crc16_xmodem(&[0x31]), 0x2672);
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3);
    }
}
