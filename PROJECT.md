# Project: remote_play Streaming Pipeline Optimization

## Architecture
`remote_play` 是一个低延迟跨平台桌面与游戏远程串流系统。核心流媒体管线由 8 个端到端阶段构成：
```
[Host 端]
  (S1) 屏幕捕获 (ScreenCaptureKit/Linux) 
       │ 
  (S2) 硬件编码 (VideoToolbox/NVENC/x264)
       │ 
  (S3) 分片封包与 Egress (RTP / CompactRealtime / UDP Socket)
       │
[网络传输]
  (S4) 传输层网络中转与时钟同步 (Timeline Sync & Monotonic Clock Delta)
       │
[Client 端]
  (S5) 网络 Ingress 与分片重组 (UDP Socket Recv / Reassembly)
       │ 
  (S6) 抖动缓冲区与起搏调度 (Jitter Buffer / Frame Pacing / Late Drop)
       │ 
  (S7) 硬件解码 (VideoToolbox / CVPixelBuffer)
       │ 
  (S8) 渲染提交与呈现回调 (GPUI Metal / Surface Render / Presentation)
```

## Feature Inventory
| # | Feature | Description | Milestone | Source |
|---|---------|-------------|-----------|--------|
| F1 | S1: Host Screen Capture 探针 | 捕获回调微秒打点、帧提取耗时与格式转换打点 (<100ns) | M1 | R1 |
| F2 | S2: Host Hardware Encode 探针 | 编码输入入队、硬件压缩耗时与 NALU 输出发射打点 | M1 | R1 |
| F3 | S3: Host Packetization & Egress 探针 | 分片 chunking、RTP 封包与 UDP socket 发送打点 | M1 | R1 |
| F4 | S4: Network & Transit 时钟同步与打点 | 数据包头部时间戳传播、单调时钟校准与传输耗时计算 | M1 | R1 |
| F5 | S5: Client Ingress & Reassembly 探针 | UDP 接收时间戳、分片重组与 RTP 还原耗时打点 | M1 | R1 |
| F6 | S6: Client Jitter Buffer 探针 | 队列到达、缓冲驻留、帧释放起搏与丢弃判定打点 | M1 | R1 |
| F7 | S7: Client Hardware Decode 探针 | 解码入队、硬件解码执行与解码帧释放打点 | M1 | R1 |
| F8 | S8: Client Presentation & Render 探针 | 渲染提交、Metal/GPUI 呈现回调与 VSync 耗时打点 | M1 | R1 |
| F9 | FrameTimingCheckpoints 数据结构 | 紧凑高效的跨网络 8 阶段微秒时序元数据载荷与解析 | M1 | R1 |
| F10 | 运行时多分位数聚合引擎 | p50/p95/p99/min/max/stddev 低开销常数内存滚动分位数算法 | M2 | R2 |
| F11 | 流健康指标全面监控 | 实时 FPS、端到端时延、抖动缓冲区深度、丢包丢帧原因分类监控 | M2 | R2 |
| F12 | 遥测协议与 UI 呈现集成 | 周期性日志输出与 ControlMessage::HostTelemetry / Client HUD 对接 | M2 | R2 |
| F13 | Chrome Trace Event JSON 导出 | 支持导出标准 Chrome Trace (chrome://tracing) 与 Timeline 格式 | M3 | R3 |
| F14 | 时延毛刺自动触发导出 | 超过设定延迟阈值 (Spike Threshold) 时自动抓取前后 Trace 事件 | M3 | R3 |
| F15 | 瓶颈归因与诊断分析工具 | CLI/自动化分析工具，精确定位编码、网络或渲染瓶颈根本原因 | M3 | R3 |
| F16 | 捕获与编码路径零拷贝 | 采用 Bytes 共享与预分配缓冲区，消除 3~4 次逐帧全量内存深拷贝 | M4 | R4 |
| F17 | 网络收发与重组池化优化 | 消除 UDP 64KB 高频堆分配与分片重组逐片克隆，预分配分片发送缓冲 | M4 | R4 |
| F18 | 无锁队列与低争用改造 | 替换单槽互斥锁与 HostStats 读写锁，消除线程饥饿与主循环卡顿 | M4 | R4 |
| F19 | 全面单元与集成测试套件 | 验证时戳传播、分位数算法、弱网乱序重组与遥测正确性 | M5 / E2E | R5 |
| F20 | 性能基准与探针开销验证 | Benchmark 验证单探针 <100ns、单帧打点 <50µs、CPU 开销 <0.5% | M5 / E2E | R5 |
| F21 | 全工作区静态检查与代码规范 | 消除所有 Clippy 警告与格式差异，确保 cargo 检查全部绿灯 | M5 / E2E | 验收标准 |

## Milestones
| # | Name | Scope | Dependencies | Status |
|---|------|-------|-------------|--------|
| M1 | Microsecond Stage Profiling & Timing Checkpoints | S1~S8 探针集成、`FrameTimingCheckpoints` 协议定义与时戳传播 (F1~F9) | none | DONE |
| M2 | Runtime Telemetry Engine & Percentile Aggregator | 分位数计算器 (p50/p95/p99/min/max/stddev)、流健康监控与 HostTelemetry/HUD (F10~F12) | M1 | PLANNED |
| M3 | Diagnostic Trace Export & Bottleneck Analysis Tooling | Chrome Trace Event JSON 导出器、Spike 触发器与 CLI 归因分析工具 (F13~F15) | M1, M2 | PLANNED |
| M4 | Zero-Copy Pipeline & Lock-Free Architectural Optimization | 消除深拷贝、池化 UDP 收发缓冲、无锁队列替代热路径锁 (F16~F18) | M1 | PLANNED |
| M5 | E2E Verification, Benchmarks & Hardening | 全工作区测试套件、Benchmark 验证、Clippy/Fmt 修复与对抗测试 (F19~F21) | M1, M2, M3, M4 | PLANNED |

## Interface Contracts
### `protocol` ↔ `remote_core` / `host` / `client`
- `FrameTimingCheckpoints`:
  - `capture_ts_us: u64`
  - `encode_queue_ts_us: u32`
  - `encode_done_ts_us: u32`
  - `packetize_ts_us: u32`
  - `send_ts_us: u32`
  - `recv_ts_us: u32`
  - `jitter_enter_ts_us: u32`
  - `jitter_exit_ts_us: u32`
  - `decode_enter_ts_us: u32`
  - `decode_done_ts_us: u32`
  - `render_submit_ts_us: u32`
  - `render_done_ts_us: u32`
- `StageLatencyStats`:
  - `min_us: u32`, `p50_us: u32`, `p95_us: u32`, `p99_us: u32`, `max_us: u32`, `stddev_us: u32`
- `PipelineTelemetryReport`:
  - 包含 8 阶段各分位数统计、总端到端延迟分布、FPS、丢包/晚帧/满队列丢帧计数与网络码率。

### `remote_core` ↔ `client` / `host`
- `RollingQuantileAggregator`:
  - 提供无锁/低锁常数空间分位数统计计算。
- `TraceExporter`:
  - `export_chrome_trace(spans: &[TraceSpan]) -> String` (标准 Chrome Trace JSON 格式)。
- `BottleneckAnalyzer`:
  - 分析 `PipelineTelemetryReport` / `TraceSpan`，输出根本原因诊断（如 "Bottleneck: Host Encode Queue Stalls (p99=32ms)"）。

## Code Layout
- `protocol/src/`:
  - `timing.rs`: `FrameTimingCheckpoints`, `StageId`, `TraceSpan` 协议定义与编码 [DONE]
  - `lib.rs`: 导出 timing 模块，扩展 `CompactRealtimeHeader` 与 `ControlMessage::HostTelemetry` [DONE]
- `remote_core/src/`:
  - `timing.rs`: 高精度探针工具类 (基于 `quanta::Clock`) [DONE]
  - `telemetry.rs`: `RollingQuantileAggregator`, `PipelineTelemetryEngine` [M2]
  - `trace.rs`: `ChromeTraceExporter`, `TraceSpikeTrigger`, `BottleneckAnalyzer` [M3]
  - `net.rs`: 零拷贝 UDP 分片发送与接收池化优化 [DONE & M4]
  - `jitter_buffer.rs`: 增强起搏与微秒驻留时序打点、丢帧分类 [DONE]
  - `stats.rs`: 增强统计模块 [M2]
- `host/src/`:
  - `capture.rs` & `linux_capture.rs`: S1 探针打点与无锁环形捕获槽 [DONE & M4]
  - `video_encode.rs` & `linux_video_encode.rs`: S2 探针打点与零拷贝 `Bytes` 输出 [DONE & M4]
  - `lib.rs`: S3 封包与 Egress 打点，集成 Telemetry [DONE]
- `client/src/`:
  - `session.rs`: S5/S6 探针打点，原子指标替代 `RwLock<HostStats>` [DONE & M4]
  - `video_decode.rs`: S7 探针打点与零拷贝解码流水线 [DONE & M4]
  - `render.rs`: S8 呈现与 VSync 回调打点 [DONE]
- `app/src/`:
  - `ui.rs`: 扩展 Live HUD 展示 8 阶段微秒耗时与分位数 [M2]
- `tests/` / `benches/`:
  - E2E 测试用例与微基准测试 [M5]
