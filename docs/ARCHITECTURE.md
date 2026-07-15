# optrans 架构文档

## 概述

optrans 是一个基于屏幕视觉编码的文件传输工具。发送端将文件编码为全屏灰度帧序列，接收端通过截屏解码还原文件。适用于隔离网络间的单向文件传输场景（如远程桌面环境）。

## 技术栈

- **语言**: Rust 2024 Edition
- **纠错码**: RS(255,217) 帧内纠错 + RS(192,128) 块级 FEC
- **平台**: Windows（Win32 GDI）
- **并发**: 多线程流水线，无异步运行时

## 目录结构

```
src/
├── main.rs              # CLI 入口
├── cli.rs               # 命令行参数解析（clap derive）
├── lib.rs               # 库入口，导出公共 API
├── common/              # 公共模块（类型、常量、工具）
│   ├── constants.rs     # 全局常量（模块像素数、灰度级数、FEC 参数等）
│   ├── types.rs         # 共享数据类型（帧类型、格式信息、元数据、数据块等）
│   ├── error.rs         # 统一错误类型 OptransError
│   ├── fec.rs           # 块级 FEC 编解码（RS(192,128) 纠删码）
│   ├── crc.rs           # CRC-16/XMODEM 校验
│   ├── mask.rs          # 位级 XOR 数据掩码
│   ├── reed_solomon_utils.rs  # RS(255,217) 编码辅助工具（块数计算、容量计算）
├── encoder/             # 编码器（帧构建与渲染）
│   ├── pattern.rs       # Finder Pattern 生成与格式信息区标记
│   ├── reed_solomon.rs  # RS(255,217) 帧内编码
│   ├── frame.rs         # 帧组装（元数据帧、数据帧、EOT 帧、FEC 校验帧）
│   └── render.rs        # 帧到 RGBA 像素缓冲区渲染（灰度编码）
├── decoder/             # 解码器（截屏处理与帧解码）
│   ├── locate.rs        # 帧定位（Finder Pattern 检测与仿射变换）
│   ├── extract.rs       # 模块提取（二值/灰度采样）
│   ├── reed_solomon.rs  # RS(255,217) 帧内解码
│   └── decode.rs        # 帧解码（格式信息 + 载荷）
├── sender/              # 发送端（文件读取与显示）
│   ├── chunker.rs       # 文件读取、SHA-256 计算、分块
│   ├── transmit.rs      # 发送编排（FEC 编码、帧构建、异步渲染）
│   └── window.rs        # Win32 全屏窗口与 GDI 渲染
├── receiver/            # 接收端（截屏与文件重组）
│   ├── capture.rs       # Win32 全屏截屏
│   ├── engine.rs        # 接收流水线引擎（capture → decode → aggregator）
│   ├── collect.rs       # 帧收集器（去重、分类存储、FEC 恢复）
│   └── reassemble.rs    # 文件组装器（排序拼接、SHA-256 校验、写文件）
└── win32/               # Win32 平台相关
    ├── ffi.rs           # Win32 API 绑定（窗口、GDI、截屏）
    └── helpers.rs       # Win32 辅助函数（字符串转换、错误处理）
```

## 模块职责说明

### common 模块

| 文件           | 职责                                                                     |
| -------------- | ------------------------------------------------------------------------ |
| `constants.rs` | 集中管理所有可调参数（模块像素数、灰度级数、帧间隔、FEC 参数等）         |
| `types.rs`     | 定义跨模块共享的数据结构（帧类型、格式信息、元数据、数据块、编码模式等） |
| `error.rs`     | 统一错误类型 `OptransError`，覆盖所有可能的错误场景                      |
| `fec.rs`       | 块级前向纠错，使用 RS(192,128) 纠删码，每组 128 数据块 + 64 校验块       |
| `crc.rs`       | CRC-16/XMODEM 校验函数，用于格式信息的完整性验证                         |
| `mask.rs`      | 位级 XOR 掩码，用于数据混淆，提高模块值分布的均匀性                      |
| `rs_utils.rs`  | RS 编码辅助工具，包括块数计算、容量计算、多块合并格式处理                |

### encoder 模块

| 文件              | 职责                                                            |
| ----------------- | --------------------------------------------------------------- |
| `pattern.rs`      | 生成三个 Finder Pattern（7×7 QR-like 定位图案），标记格式信息区 |
| `reed_solomon.rs` | RS(255,217) 帧内编码，217 数据码字 + 38 校验码字                |
| `frame.rs`        | 帧组装，支持元数据帧、数据帧（多块合并）、EOT 帧、FEC 校验帧    |
| `render.rs`       | 将帧渲染为 RGBA 像素缓冲区，支持 2/4/8/16 级灰度编码            |

### decoder 模块

| 文件              | 职责                                                       |
| ----------------- | ---------------------------------------------------------- |
| `locate.rs`       | 帧定位，通过三个 Finder Pattern 计算帧角坐标和仿射变换参数 |
| `extract.rs`      | 模块提取，从像素坐标映射到模块坐标，支持二值和灰度采样     |
| `reed_solomon.rs` | RS(255,217) 帧内解码，可纠正单帧内最多 19 个字节错误       |
| `decode.rs`       | 帧解码，包括格式信息解码（多数投票 + CRC 校验）和载荷解码  |

### sender 模块

| 文件          | 职责                                                    |
| ------------- | ------------------------------------------------------- |
| `chunker.rs`  | 文件读取、SHA-256 计算、按帧容量分块                    |
| `transmit.rs` | 发送编排，包括块级 FEC 编码、帧构建、异步渲染、串行显示 |
| `window.rs`   | Win32 无边框全屏窗口，GDI 位图渲染                      |

### receiver 模块

| 文件            | 职责                                                  |
| --------------- | ----------------------------------------------------- |
| `capture.rs`    | Win32 GDI BitBlt 全屏截屏，支持三级回退策略           |
| `engine.rs`     | 接收流水线引擎，协调 capture、decode、aggregator 线程 |
| `collect.rs`    | 帧收集器，去重、分类存储、FEC 恢复                    |
| `reassemble.rs` | 文件组装器，按块索引排序拼接、SHA-256 校验、写文件    |

### win32 模块

| 文件         | 职责                                             |
| ------------ | ------------------------------------------------ |
| `ffi.rs`     | Win32 API 绑定，不依赖外部 Windows crate         |
| `helpers.rs` | Win32 辅助函数（字符串转换、错误处理、HDC 检查） |

## 数据流

### 发送端

```
文件 → chunker.read_file_with_block_size()
       ├── 读取文件内容
       ├── 计算 SHA-256
       └── 按帧容量分块 → Vec<BlockData>

→ transmit.run_send()
       ├── fec.encode_groups() — 块级 FEC 编码
       │   └── 每组 128 数据块 + 64 校验块
       ├── frame.new_metadata_frame() — 构建元数据帧
       ├── frame.new_data_frame() — 构建数据帧（多块合并）
       ├── frame.new_parity_frame() — 构建 FEC 校验帧
       └── frame.new_eot_frame() — 构建 EOT 帧

→ render.render_frame()
       ├── pattern 生成 Finder Pattern
       ├── 格式信息编码（含 CRC）
       ├── 灰度数据矩阵渲染
       └── 心跳注入

→ window 显示循环
       └── 异步渲染线程 → 有界 channel → 串行显示
```

### 接收端

```
capture.capture_full_screen() → (pixels, width, height)

→ engine.run_receive()
       ├── Phase 1: 等待 Meta 帧
       │   ├── locate.locate_frame_all_with_diag() — 全屏扫描
       │   ├── decode.decode_format_info() — 提取格式信息
       │   └── decode.decode_frame() — 完整解码
       │       ├── extract.extract_data_modules() — 提取数据模块
       │       └── reed_solomon.rs_decode_data() — RS 解码
       │
       ├── Phase 2: 锁定位置后捕获数据帧
       │   ├── decode_format_info() — 快速预判（去重）
       │   └── threadpool 并行 decode_frame()
       │
       └── aggregator (run_aggregator)
           ├── collect.ingest() — 去重、分类存储
           ├── collect.try_fec_recover() — FEC 恢复
           └── reassemble.finalize_to_file() — 文件重组与校验
```

## 帧格式

### 帧布局

```
┌────────────────────────────┐
│  Finder Patterns (3×7×7)   │
│  TL ──────────────── TR    │
│  │                         │
│  Format Info (顶2行)       │  ← CRC16 + 多数投票
│  │                         │
│  Data Modules              │  ← RS(255,217) + 灰度编码
│  │                         │
│  BL                        │
└────────────────────────────┘
```

### 帧类型

| 类型        | 值  | 说明                                              |
| ----------- | --- | ------------------------------------------------- |
| `Meta`      | 0   | 元数据帧：文件名、大小、SHA-256、总块数、FEC 参数 |
| `Data`      | 1   | 数据帧：文件数据块（支持多块合并）                |
| `EOT`       | 2   | 传输结束帧：空载荷                                |
| `FecParity` | 3   | FEC 校验帧：块级 RS 纠删码校验分片                |

### 格式信息（104 bits）

| 字段                | 位数 | 说明                       |
| ------------------- | ---- | -------------------------- |
| `frame_type`        | 2    | 帧类型                     |
| `frame_sequence`    | 16   | 帧序号（去重）             |
| `block_index`       | 32   | 数据块索引                 |
| `total_blocks`      | 32   | 总数据块数                 |
| `payload_bytes`     | 21   | 载荷字节数                 |
| `encoding_mode_bit` | 1    | 编码模式（0=二值，1=灰度） |

### 元数据帧载荷

| 字段             | 长度 | 说明                     |
| ---------------- | ---- | ------------------------ |
| `name_len`       | 1B   | 文件名长度               |
| `file_name`      | N B  | 文件名（UTF-8）          |
| `file_size`      | 8B   | 文件大小（LE）           |
| `sha256`         | 32B  | 文件哈希值               |
| `total_blocks`   | 4B   | 总数据块数（LE）         |
| `fec_k`          | 2B   | FEC 每组数据分片数（LE） |
| `fec_m`          | 2B   | FEC 每组校验分片数（LE） |
| `fec_groups`     | 4B   | FEC 组数（LE）           |
| `fec_shard_size` | 4B   | FEC 分片长度（LE）       |

### 数据帧载荷（多块合并）

| 字段             | 长度   | 说明           |
| ---------------- | ------ | -------------- |
| `count`          | 2B     | 块数量（LE）   |
| `block_index[N]` | 4B × N | 块索引（LE）   |
| `data_len[N]`    | 4B × N | 数据长度（LE） |
| `data[N]`        | 变长   | 块数据         |

## 关键特性

### RS(255,217) 帧内纠错

- 每帧数据经过 RS(255,217) 编码
- 217 数据码字 + 38 校验码字
- 可纠正单帧内最多 19 个字节错误

### 块级 FEC

- RS(192,128) 纠删码
- 每组 128 数据分片 + 64 校验分片
- 容错率 33%，收到任意 128 个分片即可恢复

### 灰度编码

| 灰度级数 | Bits/Module | 说明              |
| -------- | ----------- | ----------------- |
| 2        | 1           | 二值模式          |
| 4        | 2           | 4 级灰度          |
| 8        | 3           | 8 级灰度          |
| 16       | 4           | 16 级灰度（默认） |

### 格式信息 N 路多数投票

- 根据帧宽动态计算重复次数（最多 5×）
- 多数投票 + CRC 校验，抵抗远程桌面视频压缩噪声
- CRC 失败时尝试平移 ±1 像素和不同采样半径

### 异步渲染 + 串行显示

- 渲染线程通过有界 channel（容量 2）发送像素
- 显示循环严格串行输出，防止画面撕裂
- 背压机制：渲染线程阻塞直到 channel 有空闲

## 性能特性

- **发送端**: 异步渲染 + 串行显示，避免渲染积压
- **接收端**: 流水线架构，capture 全速运行，decode 使用线程池并行处理
- **内存**: 每帧像素通过 Arc 共享，避免大内存拷贝
- **帧率**: 发送端固定帧间隔（默认 100ms），接收端全速截屏

## 错误处理

- **格式信息错误**: CRC 校验失败 → 尝试平移和不同采样半径 → 最终返回错误
- **帧解码错误**: RS 解码失败 → 返回错误，不影响其他帧
- **漏帧检测**: 帧序号不连续 → 警告日志，依赖 FEC 恢复
- **文件完整性**: SHA-256 校验，不匹配则拒绝写入

## 可配置参数

所有参数位于 `src/common/constants.rs`：

| 参数                       | 默认值 | 说明                      |
| -------------------------- | ------ | ------------------------- |
| `MODULE_PIXELS_DEFAULT`    | 4      | 每模块像素数（2-8，偶数） |
| `MODULES_PER_SIDE_DEFAULT` | 200    | 默认每边模块数            |
| `FRAME_INTERVAL_MS`        | 100    | 帧间隔毫秒                |
| `GRAY_LEVELS_DEFAULT`      | 16     | 灰度级数                  |
| `FRAME_MARGIN_PX_DEFAULT`  | 20     | 帧边距像素                |
| `FEC_DATA_SHARDS`          | 128    | FEC 每组数据分片数        |
| `FEC_PARITY_SHARDS`        | 64     | FEC 每组校验分片数        |
