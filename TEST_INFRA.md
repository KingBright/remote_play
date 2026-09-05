# E2E Test Infra: remote_play Streaming Pipeline Optimization

## Test Philosophy
- Opaque-box, requirement-driven. No dependency on implementation design internals.
- Methodology: Category-Partition + BVA + Pairwise Combinatorial + Real-World Workload Testing.

## Feature Inventory & Test Coverage Mapping
| # | Feature | Source (Requirement) | Tier 1 (Feature) | Tier 2 (Boundary) | Tier 3 (Pairwise) | Tier 4 (Scenario) |
|---|---------|----------------------|:----------------:|:-----------------:|:-----------------:|:-----------------:|
| 1 | S1: Host Screen Capture 探针 | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ | ✓ |
| 2 | S2: Host Hardware Encode 探针 | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ | ✓ |
| 3 | S3: Host Packetization 探针 | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ | ✓ |
| 4 | S4: Network Transit 时戳同步 | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ | ✓ |
| 5 | S5: Client Ingress & Reassembly 探针 | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ | ✓ |
| 6 | S6: Client Jitter Buffer 探针 | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ | ✓ |
| 7 | S7: Client Hardware Decode 探针 | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ | ✓ |
| 8 | S8: Client Presentation 探针 | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ | ✓ |
| 9 | FrameTimingCheckpoints 协议 | ORIGINAL_REQUEST §R1 | 5 | 5 | ✓ | ✓ |
| 10 | 运行时多分位数聚合引擎 | ORIGINAL_REQUEST §R2 | 5 | 5 | ✓ | ✓ |
| 11 | 流健康指标监控 (FPS/丢包/抖动) | ORIGINAL_REQUEST §R2 | 5 | 5 | ✓ | ✓ |
| 12 | 遥测协议与 UI 呈现对接 | ORIGINAL_REQUEST §R2 | 5 | 5 | ✓ | ✓ |
| 13 | Chrome Trace JSON 导出 | ORIGINAL_REQUEST §R3 | 5 | 5 | ✓ | ✓ |
| 14 | 时延毛刺 Spike 触发导出 | ORIGINAL_REQUEST §R3 | 5 | 5 | ✓ | ✓ |
| 15 | 瓶颈归因与诊断分析工具 | ORIGINAL_REQUEST §R3 | 5 | 5 | ✓ | ✓ |
| 16 | 捕获与编码零拷贝流转 | ORIGINAL_REQUEST §R4 | 5 | 5 | ✓ | ✓ |
| 17 | 网络收发池化与重组优化 | ORIGINAL_REQUEST §R4 | 5 | 5 | ✓ | ✓ |
| 18 | 无锁队列与低争用架构 | ORIGINAL_REQUEST §R4 | 5 | 5 | ✓ | ✓ |
| 19 | 单元与集成测试验证 | ORIGINAL_REQUEST §R5 | 5 | 5 | ✓ | ✓ |
| 20 | 探针开销与性能基准验证 | ORIGINAL_REQUEST §R5 | 5 | 5 | ✓ | ✓ |
| 21 | 全工作区编译、测试、Clippy与格式 | 验收标准 | 5 | 5 | ✓ | ✓ |

## Test Architecture
- **Test Runner**: `cargo test --workspace` & standalone integration test suites in `remote_core/tests/` & `tests/`
- **Pass/Fail Semantics**: All test suites return exit code 0, 0 failures, 0 panics.
- **Overhead Benchmark Harness**: Criterion benchmarks / timing assertions ensuring per-probe cost <100ns, per-frame cost <50µs.

## Real-World Application Scenarios (Tier 4)
| # | Scenario | Features Exercised | Complexity |
|---|----------|--------------------|------------|
| 1 | 8 阶段高帧率 4K60 串流端到端微秒打点与时间轴追踪 | F1~F9, F16, F17 | High |
| 2 | 高突发网络丢包与乱序下的分片重组与 Jitter Pacing 丢弃统计 | F5, F6, F11, F17 | High |
| 3 | 周期性遥测聚合、多档分位数计算与 ControlMessage 广播 | F10, F11, F12 | Medium |
| 4 | 模拟卡顿毛刺并自动化触发生成 Chrome Trace Event JSON 与归因诊断 | F13, F14, F15 | High |
| 5 | 高并发多线程下无锁队列、内存零拷贝与低争用无饥饿压测 | F16, F17, F18, F20 | High |

## Coverage Thresholds
- Tier 1 (Feature Coverage): ≥ 105 test cases (5 × 21 features)
- Tier 2 (Boundary & Corner): ≥ 105 test cases
- Tier 3 (Cross-Feature Pairwise): ≥ 21 test cases
- Tier 4 (Real-World Scenarios): ≥ 11 test cases
- Total Minimum: ≥ 242 test cases (加上现有 253 个基准测试，保持全量绿灯)
