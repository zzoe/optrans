# optrans — Optical Transfer

基于 QR-like 视觉编码的高速文件传输工具。
发送端将文件编码为全屏黑白帧，接收端截屏解码还原文件。

## 快速开始

```powershell
# win7
cargo +nightly build --release --target x86_64-win7-windows-msvc -Z build-std
# win8+
cargo build -r

# 发送
optrans send ./file.bin

# 接收
optrans receive

# 测试（发送端展示固定数据，接收端解码）
optrans test send
optrans test receive

# 远程桌面场景（Horizon/Citrix 需要更大边距）
optrans test send --frame-margin 120
```

## 命令行参数

| 参数              | 默认值  | 说明                                  |
| ----------------- | ------- | ------------------------------------- |
| `--module-pixels` | 4       | 每模块像素数 (2-8 偶数)               |
| `--interval-ms`   | 150/300 | 帧间隔毫秒                            |
| `--gray-levels`   | 16      | 灰度级数 (2/4/8/16)                   |
| `--frame-margin`  | 20      | 帧边距像素（远程桌面建议 120）        |
| `--encoding-mode` | auto    | 接收端编码模式 (auto/binary/gray-N)   |
| `--debug`         | false   | 接收端调试：失败截图存 optrans_debug/ |

## 架构

```
┌──────────────────────────────────────────────┐
│                   Sender                      │
│  file → chunk → RS encode → render → window  │
└──────────────────────────────────────────────┘
                        │
                   屏幕光学通路
                        │
┌──────────────────────────────────────────────┐
│                  Receiver (v0.19)             │
│                                              │
│  capture线程 ──tx──→ aggregator线程           │
│      │ spawn               │                 │
│      └→ decode线程         │                 │
│          做完 drop tx      │                 │
│                            │                 │
│  FrameCollector             │                 │
│   seen_seqs去重             │                 │
│   block→buffer             │                 │
│   EOT→flush→写文件          │                 │
└──────────────────────────────────────────────┘
```

## 帧格式

```
┌────────────────────────────┐
│  Finder Patterns (3×7×7)   │
│  TL ──────────────── TR    │
│  │                         │
│  Format Info (顶2行)       │  ← CRC16 + 3~5路多数投票
│  │                         │
│  Data Modules              │  ← RS(255,217) + XOR掩码
│  │                         │
│  BL                       │
└────────────────────────────┘
```

- **Finder**: 7×7 QR-like 定位图案 (3个)，三角搜索定位帧角
- **Format Info**: 15B (13B 元数据 + 2B CRC16)，3~5路重复 + 多数投票
- **Data**: RS(255,217) 纠错码 + 位级 XOR 掩码

## 技术栈

- Rust (纯 std, 无异步运行时)
- RS(255,217) 纠错码
- Win32 GDI 全屏窗口 / 截屏
- 多线程流水线 (capture + decode池 + aggregator)

## 许可证

MIT
