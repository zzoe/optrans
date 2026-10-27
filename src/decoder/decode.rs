//! 帧解码模块
//!
//! 从截屏像素中完整解码一帧：
//! 1. 提取格式信息并解码（三重投票 + CRC 校验）
//! 2. 提取数据载荷并 RS 解码

use crate::common::crc::crc16_xmodem;
use crate::common::error::{OptransError, OptransResult};
use crate::common::reed_solomon_utils::rs_encoded_size;
use crate::common::types::{
    DecodedFrame, EncodingMode, FinderPattern, FormatInfo, FrameLocation,
    FrameType, GrayEncodingConfig, MetaPayload,
};
use crate::decoder::extract;
use crate::decoder::reed_solomon::RsDecoder;

/// 从截屏中解码格式信息
///
/// 主路径用 3×3 采样（radius=1）。CRC 失败时降级重试：
/// 1. 整体平移 ±1 像素（8 种组合）——纠正 finder 中心亚像素抖动导致的整体偏移
/// 2. 单点采样（radius=0）——定位精准时最稳，避开相邻模块
/// 3. 5×5 采样（radius=2）——抗随机噪声
///
/// v0.13: 动态 N 路多数投票（3× 或 5× 重复），抵抗远程桌面视频压缩噪声。
pub fn decode_format_info(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
) -> OptransResult<FormatInfo> {
    let raw_bits = extract::extract_format_modules_with_radius(pixels, img_width, img_height, loc, 0);

    // v0.13: 根据帧宽动态计算重复次数
    let (repetitions, min_bits) = format_info_params(loc.modules_w);

    // 比特不足（定位到极小假帧）：微调无意义，直接返回
    if raw_bits.len() < min_bits {
        return Err(OptransError::InvalidFormatInfo(format!(
            "格式信息比特不足: 需要 {} bits，实际 {} bits",
            min_bits,
            raw_bits.len()
        )));
    }

    // 假帧快速拒绝
    if raw_bits.iter().all(|&b| b == 1) || raw_bits.iter().all(|&b| b == 0) {
        return Err(OptransError::InvalidFormatInfo(
            "假帧: 格式信息区全白/全黑，定位到空白区域".to_string(),
        ));
    }

    // 原始尝试
    match vote_and_check(&raw_bits, repetitions) {
        Ok(info) => Ok(info),
        Err(first_err) => {
            let is_fake = matches!(&first_err, OptransError::InvalidFormatInfo(m) if m.starts_with("假帧"));
            if is_fake {
                return Err(first_err);
            }
            // CRC 失败：降级重试
            for dx in [-1i32, 0, 1] {
                for dy in [-1i32, 0, 1] {
                    if dx == 0 && dy == 0 { continue; }
                    let adj = shift_loc(loc, dx as f64, dy as f64);
                    let raw2 = extract::extract_format_modules_with_radius(pixels, img_width, img_height, &adj, 0);
                    if raw2.len() >= min_bits {
                        if let Ok(info) = vote_and_check(&raw2, repetitions) {
                            return Ok(info);
                        }
                        // v0.13: 平移后帧宽不变，但为防御性也尝试不同重复次数
                        let r2 = format_info_params(adj.modules_w).0;
                        if r2 != repetitions && raw2.len() >= format_info_params(adj.modules_w).1
                            && let Ok(info) = vote_and_check(&raw2, r2) {
                                return Ok(info);
                            }
                    }
                }
            }
            // 2. 不同采样半径（先试 3×3 默认）
            for radius in [1i32, 2] {
                let raw2 = extract::extract_format_modules_with_radius(
                    pixels, img_width, img_height, loc, radius,
                );
                if raw2.len() >= min_bits
                    && let Ok(info) = vote_and_check(&raw2, repetitions) {
                        return Ok(info);
                    }
            }
            Err(first_err)
        }
    }
}

/// v0.13: 根据帧模块宽度计算格式信息重复次数和所需最小比特数
fn format_info_params(modules_w: u32) -> (usize, usize) {
    // L5: saturating_sub 防小尺寸（modules_w < 14）下溢
    let format_area = 2 * (modules_w as usize).saturating_sub(14);
    let copy_bits = 15 * 8; // 120 bits per copy
    let max_reps = (format_area / copy_bits).max(1);
    let reps = max_reps.min(5);
    (reps, reps * copy_bits)
}

/// N 路多数投票 + CRC 校验 + 解包 (v0.13: 支持 3×/5× 重复)
fn vote_and_check(raw_bits: &[u8], repetitions: usize) -> OptransResult<FormatInfo> {
    let copy_bits = 15 * 8; // 120 bits per copy
    let min_required = copy_bits * repetitions;

    if raw_bits.len() < min_required {
        return Err(OptransError::InvalidFormatInfo(format!(
            "格式信息比特不足: 需要 {} bits，实际 {} bits",
            min_required, raw_bits.len()
        )));
    }

    let encoded_bits = &raw_bits[..min_required];

    let mut decoded_15bytes = [0u8; 15];
    for byte_idx in 0..15 {
        let mut byte_val: u8 = 0;
        for bit in 0..8 {
            let bit_pos = byte_idx * 8 + bit;
            let mut sum: u32 = 0;
            for rep in 0..repetitions {
                let idx = bit_pos + rep * copy_bits;
                if idx < encoded_bits.len() {
                    sum += encoded_bits[idx] as u32;
                }
            }
            // 多数投票：超过半数则为 1
            if sum * 2 > repetitions as u32 {
                byte_val |= 1 << bit;
            }
        }
        decoded_15bytes[byte_idx] = byte_val;
    }

    let data_part = &decoded_15bytes[..13];
    let crc_received = u16::from_le_bytes(
        decoded_15bytes[13..15]
            .try_into()
            .map_err(|_| OptransError::InvalidFormatInfo("CRC 提取失败".to_string()))?,
    );

    let crc_computed = crc16_xmodem(data_part);
    // 假帧判定必须在 CRC 比较之前：CRC-16/XMODEM 对全零输入的校验值就是 0，
    // 若先判断「计算值 != 收到值」再查全0/全1，全零数据会因 0 == 0 而放行，
    // 解包成 Meta(0) + seq=0 + pb=0 的假帧（屏幕大片暗区的实测症状）。
    if crc_received == 0x0000 || crc_received == 0xFFFF {
        return Err(OptransError::InvalidFormatInfo(format!(
            "假帧: CRC 全0/全1 (收到 0x{:04X})，定位到空白区域",
            crc_received
        )));
    }
    if crc_computed != crc_received {
        return Err(OptransError::InvalidFormatInfo(format!(
            "CRC 校验失败: 计算 0x{:04X}, 收到 0x{:04X}",
            crc_computed, crc_received
        )));
    }

    let info = unpack_format_info(data_part)?;
    // 语义校验：除 EOT 外，合法帧的载荷必然非空（Meta 至少含文件名字段，
    // Data/FecParity 至少含块头或分片）。CRC 偶然通过的全零/低熵假帧在此拦下。
    if info.frame_type != FrameType::EOT && info.payload_bytes == 0 {
        return Err(OptransError::InvalidFormatInfo(format!(
            "假帧: {:?} 帧载荷长度为 0",
            info.frame_type
        )));
    }
    Ok(info)
}

/// 生成整体平移后的 FrameLocation（不依赖 Clone，逐字段构造）
fn shift_loc(loc: &FrameLocation, dx: f64, dy: f64) -> FrameLocation {
    FrameLocation {
        tl: FinderPattern {
            center_x: loc.tl.center_x + dx,
            center_y: loc.tl.center_y + dy,
            module_size: loc.tl.module_size,
        },
        tr: FinderPattern {
            center_x: loc.tr.center_x + dx,
            center_y: loc.tr.center_y + dy,
            module_size: loc.tr.module_size,
        },
        bl: FinderPattern {
            center_x: loc.bl.center_x + dx,
            center_y: loc.bl.center_y + dy,
            module_size: loc.bl.module_size,
        },
        modules_per_side: loc.modules_per_side,
        modules_w: loc.modules_w,
        modules_h: loc.modules_h,
        module_pixels: loc.module_pixels,
    }
}

/// 从格式信息中检测编码模式
pub fn detect_encoding_mode(format_info: &FormatInfo) -> EncodingMode {
    if format_info.encoding_mode_bit == 0 {
        EncodingMode::Binary
    } else {
        EncodingMode::Gray(crate::common::constants::GRAY_LEVELS_DEFAULT)
    }
}

/// 计算灰度编码模式下的模块数
fn calc_gray_module_count(byte_count: usize, levels: u32) -> usize {
    match levels {
        2 => byte_count * 8,
        4 => byte_count * 4,
        8 => (byte_count * 8).div_ceil(3),
        16 => byte_count * 2,
        _ => byte_count * 8,
    }
}

/// 几何搜索半径绝对上限（模块数），防止极端情况搜索失控
const MAX_GEOMETRY_DELTA_CAP: i32 = 120;

/// 自适应几何搜索半径（模块数）
///
/// 误差来源：finder 中间 3 段（5 模块）的游程边界在整数像素位置，
/// 非整数缩放下总长度存在 ±1px 量化误差，模块尺寸相对误差 ≈ 1/(5×m)，
/// 跨整个跨度累积：Δmodules ≈ span_modules/(5×m)。模块越小误差越大：
/// - m=3.3px → 实测 ±15 模块（1.png：真值 758 估成 743）
/// - m=2.0px（检测下界）→ 最坏可达 ±75 模块
///
/// 因此搜索半径随模块尺寸缩小而增大，并留安全余量；
/// 上限由 MAX_GEOMETRY_DELTA_CAP 约束。
fn geometry_search_delta(modules: u32, module_size: f64) -> i32 {
    let m = module_size.max(2.0);
    let span_modules = modules.saturating_sub(7) as f64;
    let est = (span_modules * 0.2 / m).ceil() as i32 + 10;
    est.min(MAX_GEOMETRY_DELTA_CAP)
}

/// 格式 CRC 裁决的 modules_w 几何精修入口
///
/// 先按原始几何解码；失败时按 |Δ| 从小到大搜索 modules_w，返回首个通过
/// 格式信息 CRC 的 (FormatInfo, 修正后位置)。
///
/// 只搜 modules_w 不搜 modules_h 的原因：格式信息区锚定在帧底部两行，
/// 其物理采样位置由 TL/BL finder 跨度决定，对 modules_h 几乎不敏感
/// （多个 modules_h 候选都能通过 CRC，无法裁决）；对 modules_w 敏感
/// （modules_w 决定格式区模块行距，错误值使行尾采到 TR finder 黑区）。
/// modules_h 误差由首个大载荷数据帧上的严格 RS 解码精修裁决
/// （见 `refine_frame_geometry`）。
///
/// 假阳性概率：单候选 CRC 误过 ≈ 1/65536，且解包后帧类型必须合法，
/// 搜索范围 60 个候选，实际误报可忽略。
pub fn decode_format_info_with_geometry(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
) -> Option<(FormatInfo, FrameLocation)> {
    match decode_format_info(pixels, img_width, img_height, loc) {
        Ok(info) => return Some((info, loc.clone())),
        // 空白区域（全黑/全白）无论 modules_w 怎么调都是空白，搜索纯属浪费。
        // 实测高噪声桌面下单次扫描有上百个此类候选，几何搜索可占 90% 扫描耗时，
        // 而 Meta 帧仅在屏 100ms——扫描慢了直接错过真帧。
        Err(e) if is_blank_region_error(&e) => return None,
        Err(_) => {}
    }
    // 过小候选不可能是真帧（真实传输帧 ≥100 模块宽），无精修价值
    if loc.modules_w < 100 {
        return None;
    }
    let max_delta = geometry_search_delta(loc.modules_w, loc.tl.module_size);
    for delta in 1..=max_delta {
        for sign in [1i32, -1] {
            let cand_w = loc.modules_w as i32 + sign * delta;
            if !(25..=1000).contains(&cand_w) {
                continue;
            }
            let cand = with_geometry(loc, cand_w as u32, loc.modules_h);
            if let Ok(info) = decode_format_info_fast(pixels, img_width, img_height, &cand) {
                return Some((info, cand));
            }
        }
    }
    None
}

/// 是否为「空白区域」类错误（全黑/全白采样），此类失败调几何无用
fn is_blank_region_error(e: &OptransError) -> bool {
    matches!(e, OptransError::InvalidFormatInfo(m) if m.starts_with("假帧"))
}

/// 廉价格式信息解码（无降级重试），供几何搜索批量裁决使用
///
/// 单点采样（radius=0）：比 3×3 快约 9 倍。几何搜索每候选最多上百次提取，
/// 必须用最廉价的模式；命中后的载荷由 RS 校验兜底裁决，不会因单点采样引入误判。
fn decode_format_info_fast(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
) -> OptransResult<FormatInfo> {
    let raw_bits =
        extract::extract_format_modules_with_radius(pixels, img_width, img_height, loc, 0);
    let (repetitions, min_bits) = format_info_params(loc.modules_w);
    if raw_bits.len() < min_bits {
        return Err(OptransError::InvalidFormatInfo(format!(
            "格式信息比特不足: 需要 {} bits，实际 {} bits",
            min_bits,
            raw_bits.len()
        )));
    }
    vote_and_check(&raw_bits, repetitions)
}

/// 构造替换 modules_w / modules_h 的 FrameLocation
fn with_geometry(loc: &FrameLocation, modules_w: u32, modules_h: u32) -> FrameLocation {
    FrameLocation {
        tl: FinderPattern {
            center_x: loc.tl.center_x,
            center_y: loc.tl.center_y,
            module_size: loc.tl.module_size,
        },
        tr: FinderPattern {
            center_x: loc.tr.center_x,
            center_y: loc.tr.center_y,
            module_size: loc.tr.module_size,
        },
        bl: FinderPattern {
            center_x: loc.bl.center_x,
            center_y: loc.bl.center_y,
            module_size: loc.bl.module_size,
        },
        modules_per_side: modules_w.max(modules_h),
        modules_w,
        modules_h,
        module_pixels: loc.module_pixels,
    }
}

/// 触发几何精修的最小载荷字节数
///
/// modules_h 错误导致的采样漂移随行数线性累积，载荷必须覆盖足够多的行
/// 才能让 RS 严格解码暴露错位；小载荷帧顶部漂移小，无法作为裁决信号。
pub const MIN_REFINE_PAYLOAD: u32 = 8192;

/// 解码帧的数据载荷
///
/// v0.29: 先严格解码（任一 RS 块纠错失败即失败）；失败时做 modules_h
/// 几何搜索——格式信息区锚定在帧底部，其 CRC 对 modules_h 误差几乎不敏感，
/// 唯一可靠的裁决信号是中部数据行的 RS 纠错成败。
/// 全部失败则整帧报错（由块级 FEC 兜底），绝不混入未纠错数据。
pub fn decode_payload(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
    info: &FormatInfo,
    encoding_mode: &EncodingMode,
) -> OptransResult<Vec<u8>> {
    if info.payload_bytes == 0 {
        return Ok(Vec::new());
    }

    // Auto 必须先解析为实际模式（采样位数由灰度级数决定）
    let actual_mode = match encoding_mode {
        EncodingMode::Auto => detect_encoding_mode(info),
        other => other.clone(),
    };

    // 1. 原始位置解码（正常路径，无额外开销）
    let first_err = match decode_payload_at(pixels, img_width, img_height, loc, info, &actual_mode)
    {
        Ok(v) => return Ok(v),
        Err(e) => e,
    };

    // 2. 几何搜索修正 modules_h 量化误差
    if let Some((_, payload)) =
        search_refined_geometry(pixels, img_width, img_height, loc, info, &actual_mode)
    {
        return Ok(payload);
    }

    // 3. 全部失败：整帧丢弃交给 FEC，失败计数周期告警（M12）
    note_decode_failure(&first_err);
    Err(first_err)
}

/// 几何精修（引擎侧入口）：返回修正后的 FrameLocation
///
/// Meta 锁定后由接收引擎在首个大载荷数据帧上调用一次，修正 modules_h 后
/// 后续所有帧复用正确位置，避免每帧重复搜索。
/// 载荷过小（裁决信号不足）或搜索失败时返回 None。
pub fn refine_frame_geometry(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
    info: &FormatInfo,
    encoding_mode: &EncodingMode,
) -> Option<FrameLocation> {
    if info.payload_bytes < MIN_REFINE_PAYLOAD {
        return None;
    }
    let actual_mode = match encoding_mode {
        EncodingMode::Auto => detect_encoding_mode(info),
        other => other.clone(),
    };
    // 原始位置已正确
    if decode_payload_at(pixels, img_width, img_height, loc, info, &actual_mode).is_ok() {
        return Some(loc.clone());
    }
    search_refined_geometry(pixels, img_width, img_height, loc, info, &actual_mode)
        .map(|(loc, _)| loc)
}

/// modules_h 几何搜索：由小到大尝试 ±1..=MAX_REFINE_DELTA 偏移
///
/// 每个候选先过格式信息 CRC（廉价过滤），再做严格载荷解码裁决。
/// 候选需同时通过两者才可信——假阳性概率可忽略。
fn search_refined_geometry(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
    info: &FormatInfo,
    encoding_mode: &EncodingMode,
) -> Option<(FrameLocation, Vec<u8>)> {
    let max_delta = geometry_search_delta(loc.modules_h, loc.tl.module_size);
    for delta in 1..=max_delta {
        for sign in [1i32, -1] {
            let cand_h = loc.modules_h as i32 + sign * delta;
            if !(25..=1000).contains(&cand_h) {
                continue;
            }
            let cand = with_modules_h(loc, cand_h as u32);
            // 格式信息 CRC 过滤：候选 modules_h 下格式区位置偏移，
            // 绝大多数错误候选在此被廉价拒绝
            if decode_format_info(pixels, img_width, img_height, &cand).is_err() {
                continue;
            }
            if let Ok(v) =
                decode_payload_at(pixels, img_width, img_height, &cand, info, encoding_mode)
            {
                return Some((cand, v));
            }
        }
    }
    None
}

/// 构造仅替换 modules_h 的 FrameLocation
fn with_modules_h(loc: &FrameLocation, modules_h: u32) -> FrameLocation {
    FrameLocation {
        tl: FinderPattern {
            center_x: loc.tl.center_x,
            center_y: loc.tl.center_y,
            module_size: loc.tl.module_size,
        },
        tr: FinderPattern {
            center_x: loc.tr.center_x,
            center_y: loc.tr.center_y,
            module_size: loc.tr.module_size,
        },
        bl: FinderPattern {
            center_x: loc.bl.center_x,
            center_y: loc.bl.center_y,
            module_size: loc.bl.module_size,
        },
        modules_per_side: loc.modules_w.max(modules_h),
        modules_w: loc.modules_w,
        modules_h,
        module_pixels: loc.module_pixels,
    }
}

/// 在指定位置解码载荷
///
/// 任一 RS 块纠错失败即整体报错（[`RsDecoder::decode`]），不返回半损坏数据。
fn decode_payload_at(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
    info: &FormatInfo,
    encoding_mode: &EncodingMode,
) -> OptransResult<Vec<u8>> {
    if info.payload_bytes == 0 {
        return Ok(Vec::new());
    }

    let payload_bytes = info.payload_bytes as usize;
    let encoded_bytes = rs_encoded_size(payload_bytes);

    let encoded_bits = match encoding_mode {
        EncodingMode::Gray(levels) => calc_gray_module_count(encoded_bytes, *levels),
        _ => encoded_bytes * 8,
    };

    // 提取数据模块原始值
    let module_values = extract::extract_data_modules(
        pixels,
        img_width,
        img_height,
        loc,
        encoded_bits,
        encoding_mode,
    );

    if module_values.len() < encoded_bits {
        return Err(OptransError::FrameDecode(format!(
            "数据模块值不足: 需要 {} modules，实际 {} modules",
            encoded_bits,
            module_values.len()
        )));
    }

    // 模块值 → 字节（仅使用需要的模块数）
    let relevant_modules = &module_values[..encoded_bits];
    let encoded_data = match encoding_mode {
        EncodingMode::Gray(levels) => {
            // 非法级数（非 2/4/8/16）不可能来自合法发送端，报错而非静默降级
            let config = GrayEncodingConfig::new(*levels).map_err(|e| {
                OptransError::FrameDecode(format!("非法灰度级数 {}: {}", levels, e))
            })?;
            extract::decode_gray_n(relevant_modules, *levels, &config.thresholds, encoded_bytes)
        }
        _ => {
            let mut data = Vec::with_capacity(encoded_bytes);
            for byte_idx in 0..encoded_bytes {
                let mut byte_val: u8 = 0;
                for bit in 0..8 {
                    let bit_pos = byte_idx * 8 + bit;
                    if bit_pos < relevant_modules.len() && relevant_modules[bit_pos] == 1 {
                        byte_val |= 1 << bit;
                    }
                }
                data.push(byte_val);
            }
            data
        }
    };

    // v0.6: 反向应用数据掩码（XOR 自反），恢复原始 RS 编码字节
    let encoded_data = crate::common::mask::apply_mask(&encoded_data);

    // RS 解码：任一块纠错失败即报错（坏块混入会毒化 FEC，见 RsDecoder 文档）
    let rs = RsDecoder::new();
    let decoded = rs.decode(&encoded_data)?;

    Ok(decoded)
}

/// 载荷解码失败计数（M12）：整帧丢弃由 FEC 兜底是正常路径，
/// 但持续高频失败意味着几何/显示质量问题，需要周期性告警而不是静默。
static DECODE_FAIL_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// 记录一次载荷解码失败；首次与每累计 100 次输出一条 warn
fn note_decode_failure(err: &OptransError) {
    use std::sync::atomic::Ordering;
    let n = DECODE_FAIL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    if n == 1 || n.is_multiple_of(100) {
        log::warn!(
            "载荷解码失败（累计 {} 次，本帧交由 FEC 恢复）: {}",
            n,
            err
        );
    }
}

/// 完整解码一帧（格式信息 + 载荷）
///
/// `EncodingMode::Auto` 时按格式信息的模式位判定：二值帧直接解；
/// 灰度帧的**级数不在格式信息里**（只有 1 bit 模式位），故依次尝试
/// 16/8/4/2 级，由载荷的 RS 校验裁决——发送端可用 `--gray-levels`
/// 按显示链路质量调整级数，接收端无需同步配置。
pub fn decode_frame(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
    encoding_mode: &EncodingMode,
) -> OptransResult<DecodedFrame> {
    let format_info = decode_format_info(pixels, img_width, img_height, loc)?;

    let (actual_mode, payload) = match encoding_mode {
        EncodingMode::Auto if format_info.encoding_mode_bit == 0 => {
            let mode = EncodingMode::Binary;
            let payload = decode_payload(pixels, img_width, img_height, loc, &format_info, &mode)?;
            (mode, payload)
        }
        EncodingMode::Auto => {
            let mut first_err: Option<OptransError> = None;
            let mut found: Option<(EncodingMode, Vec<u8>)> = None;
            // 由密到疏尝试：通道质量好时首选 16 级（帧数最少）
            for &levels in crate::common::types::VALID_GRAY_LEVELS.iter().rev() {
                let mode = EncodingMode::Gray(levels);
                match decode_payload(pixels, img_width, img_height, loc, &format_info, &mode) {
                    Ok(p) => {
                        found = Some((mode, p));
                        break;
                    }
                    Err(e) => {
                        if first_err.is_none() {
                            first_err = Some(e);
                        }
                    }
                }
            }
            match found {
                Some(v) => v,
                None => {
                    return Err(first_err
                        .unwrap_or_else(|| OptransError::FrameDecode("载荷解码失败".to_string())));
                }
            }
        }
        other => {
            let payload = decode_payload(pixels, img_width, img_height, loc, &format_info, other)?;
            (other.clone(), payload)
        }
    };

    Ok(DecodedFrame {
        frame_type: format_info.frame_type,
        frame_sequence: format_info.frame_sequence,
        block_index: format_info.block_index,
        total_blocks: format_info.total_blocks,
        payload,
        encoding_mode: actual_mode,
    })
}

/// 平移三个 finder 中心（整体平移采样原点）
fn shift_finders(loc: &FrameLocation, dx: f64, dy: f64) -> FrameLocation {
    let mut shifted = loc.clone();
    shifted.tl.center_x += dx;
    shifted.tl.center_y += dy;
    shifted.tr.center_x += dx;
    shifted.tr.center_y += dy;
    shifted.bl.center_x += dx;
    shifted.bl.center_y += dy;
    shifted
}

/// Meta 帧采样原点微扫（兜底）
///
/// 格式信息锚定在帧底部两行（采样位置由 BL finder 决定），其 CRC 对 TL/TR
/// 的中心误差不敏感；而数据载荷从帧顶部开始，采样网格原点就是 TL 中心。
/// 远程缩放/模糊下 TL 中心可能偏半个模块，表现为「格式 CRC 通过、载荷 RS
/// 全部超出纠错能力」——此时整帧被丢弃，Meta 帧因此永远解不出。
///
/// 本函数以初始位置为中心做小步平移搜索（±0.6 模块，步长 0.2），由载荷的
/// RS 校验裁决（255 字节块可纠 19 字节，误判概率可忽略）。仅用于 Meta 帧：
/// 载荷小（< 8KB），单次尝试只解一个 RS 块，代价可接受。
///
/// 返回 `Some((修正后的位置, 解码帧))`。
pub fn decode_meta_origin_sweep(
    pixels: &[u8],
    img_width: i32,
    img_height: i32,
    loc: &FrameLocation,
    info: &FormatInfo,
) -> Option<(FrameLocation, DecodedFrame)> {
    if info.payload_bytes == 0 {
        return None;
    }
    let mode = detect_encoding_mode(info);
    let ms = loc.tl.module_size.max(1.0);
    const FRACS: [f64; 7] = [-0.6, -0.4, -0.2, 0.0, 0.2, 0.4, 0.6];

    for &fx in &FRACS {
        for &fy in &FRACS {
            // 零偏移由调用方的常规解码路径覆盖
            if fx == 0.0 && fy == 0.0 {
                continue;
            }
            let cand = shift_finders(loc, fx * ms, fy * ms);
            let Ok(payload) =
                decode_payload_at(pixels, img_width, img_height, &cand, info, &mode)
            else {
                continue;
            };
            if payload.is_empty() {
                continue;
            }
            return Some((
                cand,
                DecodedFrame {
                    frame_type: info.frame_type,
                    frame_sequence: info.frame_sequence,
                    block_index: info.block_index,
                    total_blocks: info.total_blocks,
                    payload,
                    encoding_mode: mode,
                },
            ));
        }
    }
    None
}

/// 从解码后的元数据帧载荷中解析 MetaPayload
pub fn parse_meta_payload(payload: &[u8]) -> OptransResult<MetaPayload> {
    if payload.is_empty() {
        return Err(OptransError::FrameDecode("元数据载荷为空".to_string()));
    }

    let mut offset = 0usize;

    if offset >= payload.len() {
        return Err(OptransError::FrameDecode(
            "元数据格式错误：缺少文件名长度".to_string(),
        ));
    }
    let name_len = payload[offset] as usize;
    offset += 1;

    if offset + name_len > payload.len() {
        return Err(OptransError::FrameDecode(
            "元数据格式错误：文件名超出边界".to_string(),
        ));
    }
    let raw_name = String::from_utf8(payload[offset..offset + name_len].to_vec())
        .map_err(|e| OptransError::FrameDecode(format!("文件名 UTF-8 解码失败: {}", e)))?;
    // 清洗文件名：剥离目录成分并拒绝非法名，防止路径穿越写文件
    let file_name = crate::common::types::sanitize_file_name(&raw_name)
        .map_err(|e| OptransError::FrameDecode(format!("文件名不合法: {}", e)))?;
    offset += name_len;

    if offset + 8 > payload.len() {
        return Err(OptransError::FrameDecode(
            "元数据格式错误：缺少文件大小".to_string(),
        ));
    }
    let file_size = u64::from_le_bytes(
        payload[offset..offset + 8]
            .try_into()
            .map_err(|_| OptransError::FrameDecode("文件大小解析失败".to_string()))?,
    );
    offset += 8;

    if offset + 32 > payload.len() {
        return Err(OptransError::FrameDecode(
            "元数据格式错误：缺少 SHA-256".to_string(),
        ));
    }
    let mut sha256 = [0u8; 32];
    sha256.copy_from_slice(&payload[offset..offset + 32]);
    offset += 32;

    if offset + 4 > payload.len() {
        return Err(OptransError::FrameDecode(
            "元数据格式错误：缺少总块数".to_string(),
        ));
    }
    let total_blocks = u32::from_le_bytes(
        payload[offset..offset + 4]
            .try_into()
            .map_err(|_| OptransError::FrameDecode("总块数解析失败".to_string()))?,
    );
    offset += 4;

    // FEC 字段（v0.26+）：fec_k(2) + fec_m(2) + fec_groups(4) + fec_shard_size(4) = 12 字节
    // 向后兼容：旧版无此字段，长度不足时 FEC 全为 0（未启用）
    let (fec_k, fec_m, fec_groups, fec_shard_size) = if offset + 12 <= payload.len() {
        let k = u16::from_le_bytes(
            payload[offset..offset + 2]
                .try_into()
                .map_err(|_| OptransError::FrameDecode("fec_k 解析失败".to_string()))?,
        );
        let m = u16::from_le_bytes(
            payload[offset + 2..offset + 4]
                .try_into()
                .map_err(|_| OptransError::FrameDecode("fec_m 解析失败".to_string()))?,
        );
        let groups = u32::from_le_bytes(
            payload[offset + 4..offset + 8]
                .try_into()
                .map_err(|_| OptransError::FrameDecode("fec_groups 解析失败".to_string()))?,
        );
        let shard_size = u32::from_le_bytes(
            payload[offset + 8..offset + 12]
                .try_into()
                .map_err(|_| OptransError::FrameDecode("fec_shard_size 解析失败".to_string()))?,
        );
        (k, m, groups, shard_size)
    } else {
        (0, 0, 0, 0)
    };

    Ok(MetaPayload {
        file_name,
        file_size,
        sha256,
        total_blocks,
        fec_k,
        fec_m,
        fec_groups,
        fec_shard_size,
    })
}

/// 从 FormatInfo 打包的 13 字节解包
fn unpack_format_info(data: &[u8]) -> OptransResult<FormatInfo> {
    if data.len() < 13 {
        return Err(OptransError::InvalidFormatInfo(format!(
            "格式信息数据不足: 需要 13 bytes，实际 {} bytes",
            data.len()
        )));
    }

    let mut val: u128 = 0;
    for i in 0..13 {
        val |= (data[i] as u128) << (i * 8);
    }

    let frame_type_raw = (val & 0x3) as u8;
    let frame_sequence = ((val >> 2) & 0xFFFF) as u16;
    let block_index = ((val >> 18) & 0xFFFF_FFFF) as u32;
    let total_blocks = ((val >> 50) & 0xFFFF_FFFF) as u32;
    let payload_bytes = ((val >> 82) & 0x1F_FFFF) as u32; // 21 bits
    let encoding_mode_bit = ((val >> 103) & 0x1) as u8;

    let frame_type = FrameType::from_u8(frame_type_raw).ok_or_else(|| {
        OptransError::InvalidFormatInfo(format!("未知帧类型: {}", frame_type_raw))
    })?;

    Ok(FormatInfo {
        frame_type,
        frame_sequence,
        block_index,
        total_blocks,
        payload_bytes,
        encoding_mode_bit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_zero_format_bits_rejected() {
        // 全零格式比特（屏幕大片暗区/纯色区的典型采样）。
        // 回归：CRC-16/XMODEM 对全零数据的校验值是 0，与「收到 0」相等，
        // 旧实现先比较 CRC 再查全0，导致解包成 Meta(0)+seq=0+pb=0 的假帧并放行。
        let raw_bits = vec![0u8; 600];
        let err = vote_and_check(&raw_bits, 5).unwrap_err();
        assert!(
            is_blank_region_error(&err),
            "全零格式比特应被判为假帧: {}",
            err
        );
    }

    #[test]
    fn test_zero_payload_meta_rejected() {
        // 构造 type=Meta、seq=1、payload_bytes=0、其余为 0 且 CRC 正确的格式信息：
        // 真实发送端不会产生该组合（Meta 必含文件名等字段），应判为假帧。
        let mut val: u128 = 0;
        val |= 1u128 << 2; // seq=1，type 保持 Meta(0)
        let mut data = [0u8; 13];
        for i in 0..13 {
            data[i] = ((val >> (i * 8)) & 0xFF) as u8;
        }
        let crc = crc16_xmodem(&data);
        assert_ne!(crc, 0, "该构造的 CRC 应非零，才能触发语义校验分支");

        let mut bytes = [0u8; 15];
        bytes[..13].copy_from_slice(&data);
        bytes[13..].copy_from_slice(&crc.to_le_bytes());

        // 15 字节 → 120 bits，重复 5 份拆分
        let mut raw_bits = Vec::with_capacity(600);
        for _ in 0..5 {
            for b in bytes {
                for bit in 0..8 {
                    raw_bits.push((b >> bit) & 1);
                }
            }
        }
        let err = vote_and_check(&raw_bits, 5).unwrap_err();
        assert!(
            is_blank_region_error(&err),
            "载荷长度为 0 的 Meta 应判为假帧: {}",
            err
        );
    }

    #[test]
    fn test_unpack_format_info_roundtrip() {
        let info = FormatInfo {
            frame_type: FrameType::Data,
            frame_sequence: 42,
            block_index: 100,
            total_blocks: 500,
            payload_bytes: 2000,
            encoding_mode_bit: 0,
        };

        let mut packed = vec![0u8; 13];
        {
            let mut val: u128 = 0;
            val |= info.frame_type as u128;
            val |= (info.frame_sequence as u128) << 2;
            val |= (info.block_index as u128) << 18;
            val |= (info.total_blocks as u128) << 50;
            val |= (info.payload_bytes as u128) << 82;
            val |= (info.encoding_mode_bit as u128) << 103;
            for i in 0..13 {
                packed[i] = ((val >> (i * 8)) & 0xFF) as u8;
            }
        }

        let unpacked = unpack_format_info(&packed).unwrap();
        assert_eq!(unpacked.frame_type, FrameType::Data);
        assert_eq!(unpacked.frame_sequence, 42);
        assert_eq!(unpacked.block_index, 100);
        assert_eq!(unpacked.total_blocks, 500);
        assert_eq!(unpacked.payload_bytes, 2000);
        assert_eq!(unpacked.encoding_mode_bit, 0);
    }

    #[test]
    fn test_unpack_format_info_payload_21bit() {
        // payload_bytes 为 21 bits，测试边界值 2,097,151 (2^21-1)
        let info = FormatInfo {
            frame_type: FrameType::Data,
            frame_sequence: 1,
            block_index: 0,
            total_blocks: 1,
            payload_bytes: 2_097_151,
            encoding_mode_bit: 0,
        };

        let mut packed = vec![0u8; 13];
        let mut val: u128 = 0;
        val |= info.frame_type as u128;
        val |= (info.frame_sequence as u128) << 2;
        val |= (info.block_index as u128) << 18;
        val |= (info.total_blocks as u128) << 50;
        val |= (info.payload_bytes as u128) << 82;
        val |= (info.encoding_mode_bit as u128) << 103;
        for i in 0..13 {
            packed[i] = ((val >> (i * 8)) & 0xFF) as u8;
        }

        let unpacked = unpack_format_info(&packed).unwrap();
        assert_eq!(unpacked.payload_bytes, 2_097_151);
    }

    #[test]
    fn test_parse_meta_payload() {
        let mut payload = Vec::new();
        let file_name = "test.dat";
        payload.push(file_name.len() as u8);
        payload.extend_from_slice(file_name.as_bytes());
        payload.extend_from_slice(&12345u64.to_le_bytes());
        payload.extend_from_slice(&[0xAB; 32]);
        payload.extend_from_slice(&10u32.to_le_bytes());
        // FEC 字段
        payload.extend_from_slice(&128u16.to_le_bytes()); // fec_k
        payload.extend_from_slice(&64u16.to_le_bytes());  // fec_m
        payload.extend_from_slice(&1u32.to_le_bytes());   // fec_groups
        payload.extend_from_slice(&32768u32.to_le_bytes()); // fec_shard_size

        let meta = parse_meta_payload(&payload).unwrap();
        assert_eq!(meta.file_name, "test.dat");
        assert_eq!(meta.file_size, 12345);
        assert_eq!(meta.sha256, [0xAB; 32]);
        assert_eq!(meta.total_blocks, 10);
        assert_eq!(meta.fec_k, 128);
        assert_eq!(meta.fec_m, 64);
        assert_eq!(meta.fec_groups, 1);
        assert_eq!(meta.fec_shard_size, 32768);
        assert!(meta.fec_enabled());
    }

    #[test]
    fn test_parse_meta_payload_legacy_no_fec() {
        // 向后兼容：旧版元数据无 FEC 字段
        let mut payload = Vec::new();
        payload.push(4u8); // name_len
        payload.extend_from_slice(b"test");
        payload.extend_from_slice(&100u64.to_le_bytes());
        payload.extend_from_slice(&[0xAB; 32]);
        payload.extend_from_slice(&5u32.to_le_bytes());

        let meta = parse_meta_payload(&payload).unwrap();
        assert_eq!(meta.total_blocks, 5);
        assert_eq!(meta.fec_k, 0);
        assert!(!meta.fec_enabled());
    }

    #[test]
    fn test_detect_encoding_mode() {
        let binary_info = FormatInfo {
            encoding_mode_bit: 0,
            ..FormatInfo::new_eot(0)
        };
        assert_eq!(detect_encoding_mode(&binary_info), EncodingMode::Binary);

        let gray_info = FormatInfo {
            encoding_mode_bit: 1,
            ..FormatInfo::new_eot(0)
        };
        assert!(matches!(detect_encoding_mode(&gray_info), EncodingMode::Gray(_)));
    }

    #[test]
    fn test_calc_gray_module_count() {
        assert_eq!(calc_gray_module_count(100, 2), 800);
        assert_eq!(calc_gray_module_count(100, 4), 400);
        assert_eq!(calc_gray_module_count(100, 8), 267);
        assert_eq!(calc_gray_module_count(100, 16), 200);
    }
}
