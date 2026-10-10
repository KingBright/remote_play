# Linux native capture 边界阶段记录，2026-10-10

本阶段落实已批准 [最小方案](../../plans/LINUX-WAYLAND-CAPTURE-ADAPTER.md) 的离线可验证边界。它不是完整 portal/PipeWire 适配器或发布候选，现有 Linux FFmpeg capture 入口、GPUI、协议、输入、音频和其他三端运行路径尚未替换。

## 实际源码

- `host/src/linux_frame.rs`：受限 owned-plane 复制，支持 packed BGRA/BGRx/RGBA/RGBx、NV12、I420；先验证全部 plane，再分配复制。验证尺寸/像素与内存预算、正 stride、chunk offset/size、行 padding、plane 数量及 crop；420 crop 须偶数对齐，未实现 transform 明确失败。`mapping_offset` 保留为元数据，借用视图从 SPA data.data 开始，不重复加该偏移。保留原始 SPA 颜色枚举及 unknown、PipeWire 纳秒 PTS/seq 与独立 host arrival 时钟。
- `FrameMailbox`：仅接受未提交的 owned raw frame，单张 latest 槽替换旧帧，拒绝旧 generation；暂停清槽，关闭撤销交付并唤醒等待者。该类型不接受压缩 AU，也不是解码参考帧丢弃策略。
- `host/src/linux_portal.rs`：一次性选择的状态、关闭计划、受限 OwnedFd 单次转交和通用 RAII lease registry。persist_mode 固定 0、multiple=false，排除 VIRTUAL/未实现 cursor metadata；取消后晚 Response 无法恢复已关闭状态。FD 转交重验 session phase/generation。换源的 prepare 不删除当前 grant；commit 再验证认证 nonce、connection ID、share scope、source revision 和未失效 ticket，失败返回未提交资源，旧源保留。该模块没有 D-Bus 调用，也没有实际弹出系统选择器。
- `host/src/lib.rs`：声明上述模块；Unix 测试可编译 portal 边界，Windows 构建不引入 Unix FD 模块。
- HO5 用户脚本及独立测试见 [精确 DNS 本机步骤](HO5-EXACT-DNS-USER-STEP.md)。没有添加第二 GUI 或更改 Cargo.toml/Cargo.lock。

## 测试边界

| 新检查 | 结果 | 实际范围 |
| --- | --- | --- |
| `cargo test -p host --lib linux_frame::tests --offline --locked` | 7 通过 | 本 Mac、固定 owned-plane 内存与 mailbox；无真实 PipeWire |
| `cargo test -p host --lib linux_portal::tests --offline --locked` | 8 通过，补充 `only_selected_source_kind_can_reach_ready` 独立 1 通过，共 9 | 本 Mac、控制状态及 FD/registry；无 D-Bus/选择器 |
| `python3 -m unittest scripts.tests.test_repair_ho5_dns -v` | 8 通过 | mock 配置/服务及失败回滚；无系统修改 |
| `python3 -m unittest scripts.tests.test_ho5_dns_bootstrap -v` | 6 通过 | 同字节校验执行、导入隔离及 public fixture 非普通文件拒绝；无 sudo |

本轮复用 `/Users/jinliang/rust-target`，开工 84 GiB，后续观察约 82 GiB，超过 8 GiB 构建预检与 4 GiB 保留线。Portal 测试等待另一个项目释放共享 target 锁后实际执行（cargo 总耗时 13m02s），未删除锁、未打断其他构建、未建立新 target。既有已通过阶段测试不重复。原 105 项 mixed dirty 内容与 `/tmp/remoteplay-ely-preservation.json` 对比未变化；阶段提交仅纳入本轮明确文件。

## 仍需完成

1. Linux-only 依赖锁、窄 zbus request shim（先监听后调用、取消主动 Close、Session.Closed）与长期 D-Bus connection ownership。
2. 真实 PipeWire 专用线程、受限 FD connect、格式/metadata 协商、MemFd/MemPtr 安全 mapping、回调内 copy 后归还。这里的纯 plane 支持不能当作实际 SPA 格式已协商成功。
3. raw HEVC 输入、对应 AU timing、色彩/SPS 验证、压缩 AU 不丢参考帧的背压及取消无死锁，随后接现有 VideoCapturer/VideoEncoder。
4. 原 GPUI 本机分享动作与真实 parent 标识、Connection 范围 source catalog、准备后提交换源及撤销处理。纯 registry 测试不是实际 Session 路径隔离证明。
5. Linux native 编译与经正常系统选择后的物理源/实际 GPUI 截图、暂停/撤销/输入/音频/画质/性能/长时验收。全部尚未通过，也未升级已安装应用。

HO5 的管理员 DNS 变更仍需用户在本机终端认证；暂停传输确认字节仍为 0，原 source 包没有完成下载/同步、changed-file mtime refresh 或新 native build。批准不等于实际缺失的 root executor。MacBook `mesh.secret` 只查元数据：uid=0、gid=20、mode=0600，普通非链接文件，当前执行 uid=501；没有读取密钥、chown、启动或升级应用。该单文件修正仍需本机管理员能力。

阶段实现检查点为 `a8f853738154b610debaca177f73909e88f08524`（10 个明确文件，30 个新用例；16 Rust、14 Python）。提交后元数据检查 `8ce94560-a2bd-4f79-95ab-96e3d44b3d7f` 显示当前 HO5 宿主环境的 `pkg-config --modversion libpipewire-0.3/libspa-0.2` 均不能解析开发条目，PATH 找不到 clang。这不是所有已有 SDK/容器都不存在的证明；须先确认原有合法构建环境的依赖，不能直接推断旧 native build 使用当前宿主 shell 或自行安装系统包。检测未启动构建或修改系统包。最终本地空闲约 80 GiB；只删除本轮生成的 3 个 bytecode 文件后，status 路径、状态和文件哈希均回到原 105 项基线。
