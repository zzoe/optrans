# optrans 架构文档 v0.19

## 模块结构

```
src/
├── main.rs              # 入口，CLI 分派
├── cli.rs               # 命令行定义 + 参数结构体
├── common/
│   ├── constants.rs     # 配置常量
│   ├── types.rs         # 核心类型定义
│   ├── error.rs         # 错误类型
│   ├── crc.rs           # CRC-16 XMODEM
│   └── mask.rs          # 位级 XOR 掩码
├── encoder/
│   ├── frame.rs         # 帧构建 (Meta/Data/EOT)
│   ├── render.rs        # 帧渲染 (像素→位图)
│   └── reed_solomon.rs  # RS(255,217) 编码
├── decoder/
│   ├── decode.rs        # 格式信息解码 + 完整帧解码
│   ├── locate.rs        # finder pattern 定位 + 三角搜索
│   ├── extract.rs       # 仿射变换 + 模块值提取
│   └── reed_solomon.rs  # RS(255,217) 解码
├── sender/
│   ├── transmit.rs      # 发送编排 (分块→编码→循环显示)
│   ├── window.rs        # Win32 全屏窗口
│   └── chunker.rs       # 文件读取 + 分块
├── receiver/
│   ├── mod_impl.rs      # 接收主循环 (v0.19 流水线)
│   ├── capture.rs       # GDI 全屏截屏
│   ├── collect.rs       # 帧收集器 (去重/P帧合并)
│   └── reassemble.rs    # 文件重组 + SHA-256 校验
├── test_mode.rs         # 测试命令实现
└── win32/
    ├── ffi.rs           # Win32 FFI 绑定
    └── helpers.rs       # Win32 辅助函数
```

## 发送流程

```
1. read_file(path) → FileManifest { blocks[], sha256, file_name, file_size }
2. calculate_frame_capacity() → modules_w, modules_h, max_payload
3. chunk data (block_size = max_payload - 8)
4. create META frame → RS encode → render → display
5. loop blocks:
   a. Frame::new_data_merged([&block], seq, total) → Frame
   b. RS encode → XOR mask → gray encode → render → update_bitmap
   c. sleep(interval_ms)
6. Frame::new_eot() → display
```

### 帧构建 (Frame)

```
Data frame payload:
  [2B count] [4B block_index] [2B data_len] [... data ...]
  ↓
  RS(255,217) encode: [4B total_len] [ceil(payload/217) × 255B]
  ↓
  XOR mask: 每字节异或位置相关的伪随机值
  ↓
  Gray encode: 每模块 n 比特 → 模块灰度值
```

### 格式信息 (15 bytes)

```
bits:       [0..15] [16..47] [48..55] [56..63] [64..79] [80..95]
fields:     unused  SHA-32   frame_kind total_blocks frame_seq encoding_mode
bits:       [96]    [97]
fields:     merge_ext frame_type
```

- 13B 数据 + 2B CRC-16
- N 路重复 (N = min(5, floor(format_area / 120)))
- 接收端 N 路多数投票 → CRC 校验 → 解包

### 数据掩码

```rust
fn mask_byte_at(index: usize) -> u8 {
    // LCG 伪随机序列，零开销，确定性
    (index.wrapping_mul(0x9E37_79B9).wrapping_add(0x9E37_79B9) >> 24) as u8
}
```

## 接收流程 (v0.19 流水线)

```
run_receive(args):
  1. 创建 mpsc::channel<DecodedFrame>()
  2. spawn aggregator 线程 (持有 rx)
  3. run_capture_loop (主线程, 持有 tx)

capture_loop:
  loop:
    capture_full_screen() → (pixels, w, h)
    
    if locked_loc = OnceLock.get():
      candidates = [locked_loc]
    else:
      candidates = locate_frame_all_with_diag(pixels, w, h)
      if empty: heartbeat, continue
    
    for each candidate:
      info = decode_format_info(pixels, loc)  # 快速 (~1ms)
      if EOT:
        tx.send(EOT_frame)
        drop(tx)
        return
      if valid:
        spawn decode_thread(loc, pixels, tx.clone(), Arc::clone(&OnceLock))
    
    # 不等待 decode 结果，立即下一轮 capture

decode_thread:
  frame = decode_frame(pixels, loc)  # 完整解码 (~7ms)
  if success && !payload_empty:
    if OnceLock 未设置:  # 首次成功 = 锁定帧位置
      OnceLock.set(loc)
    tx.send(frame)

aggregator_thread:
  collector = FrameCollector::new()
  loop rx.recv():
    match collector.ingest(frame):
      NewMeta → 打印文件信息
      NewBlock → 打印进度
      EOT → is_complete? flush_file : Err
      Duplicate/Invalid → skip
  # rx close → flush_file
```

### 三角搜索 (locate)

```
1. find_finder_candidates(pixels) → 横向扫描 1:1:3:1:1
2. verify_finder_vertical(pixels, fp) → 垂直验证 + 边缘检测
   - 若 finder 下方存在黑边 (FRAME_MARGIN > 0 特征) → 跳过验证
3. merge_nearby_candidates() → 去重合并
4. cluster_by_module_size() → 按 ms 分组
5. search_triangles(cluster) → O(n³)
   - 每簇最多 200 三角组早退
   - TR/BL 约束: |dy|/|dx_bl| < 5~10 模块
6. 排序: 帧型优先 → 像素面积降序
```

### 帧锁定 (OnceLock)

首次 decode 成功后，`FrameLocation` 写入 `Arc<OnceLock<FrameLocation>>`。
后续 capture 循环直接用缓存坐标，跳过全屏扫描 (~0ms locate)。

## 数据流

```
Sender                           Screen              Receiver
┌──────────┐                   ┌──────┐             ┌──────────────┐
│ file.bin │                   │ GDI  │             │ GDI capture  │
│    ↓     │                   │window│             │    ↓         │
│ chunker  │ → RS → mask →    │ 帧  │  ───→       │ locate       │
│    ↓     │   render → GDI    │图案 │  光路       │    ↓         │
│  frames  │                   └──────┘             │ format_info  │
│  [0..n]  │                                       │    ↓         │
│    ↓     │                                       │ spawn decode │
│  EOT     │                                       │    ↓         │
└──────────┘                                       │ aggregator   │
                                                   │    ↓         │
                                                   │ flush file   │
                                                   └──────────────┘
```

## 关键设计决策

| 决策 | 理由 |
|------|------|
| RS(255,217) | 每 255B 块纠错 19B，抗屏幕噪声 |
| XOR 掩码 | 打破重复数据模式，避免 DC 偏置影响视频压缩 |
| 三角搜索早退 | O(n³) → 200上限，避免 479³ = 109M 迭代 |
| 帧锁定 (OnceLock) | 首帧后复用坐标，从 ~50ms→0ms |
| 线程池 vs 协程 | CPU 密集 (RS/仿射)，std::thread 更高效 |
| capture 快速预判 | decode_format_info ~1ms，capture 不阻塞 |
| decode 线程设锁 | 避免 CRC 碰撞假阳性 (1/65536) |
| unbounded channel | capture 不等待 decode，吞吐优先 |
| EOT 立即退出 | sender 不会再发数据，不等不重试 |
