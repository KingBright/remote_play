# Linux Wayland 采集适配最小方案，2026-10-09

2026-10-10 实施批准记录：主会话核实本人消息 `Sentinel_5ec2772cca9c81918b15c592d595c18c`，精确回复整组申请 `Sentinel_dbe19fceb4e081918803eb200df29752`：“批准以上全部的申请”。其中包含本方案的最小 portal/PipeWire 适配，保留 GPUI、公共协议及其他三端，初版 persist_mode=0。该批准不赋予通道实际缺失的管理员执行能力。下文保留 10-09 原提案与当时检查状态；实际实施及未验边界见 [10-10 阶段记录](../reviews/2026-10-10/LINUX-NATIVE-BOUNDARIES.md)。

本轮已完成 X11 根窗口尺寸与整幅缩放的小修，见 [回归报告](../reviews/2026-10-09/X11-CAPTURE-GEOMETRY.md)。以下方案仅供审阅，未添加 portal/PipeWire 依赖、未运行选择器、未修改默认后端或权限。

目标是保留现有单一 GPUI/Ely、HEVC 编码、Session、封包与传输及原生接收呈现，只补 Linux 平台的真实帧输入。最小实现先以 MemFd/MemPtr 复制到有界自有 buffer；DMA-BUF 与显式同步另行评审。CPU 路径须独立测量，不作为原生零拷贝或性能等价证明，也不能据此降低已工作的接收端原生呈现能力。

## 必须先确定的代码接缝

现有 `VideoCapturer::{start,stop,pause,resume,capture_frame}`、`VideoEncoder::{submit_frame,pull_encoded}` 在 `remote_core/src/traits.rs` 已具备所需平台接口，但实际 Linux 接线尚未使用真实采集帧：

- `host/src/linux_capture.rs` 的 `LinuxVideoCapturer` 产生空 placeholder，`LinuxVideoFrame` 只有宽高、Vec、计时等字段。
- `host/src/linux_video_encode.rs` 的 `LinuxVideoEncoder::new` 自行启动 `FfmpegHevcSource` 抓屏；`submit_frame` 丢弃传入帧。`pull_encoded_chunk` 读取该独立 FFmpeg 的 HEVC。
- `host/src/lib.rs::run_streaming` Linux 分支不传递实际 source，而是 `let _ = source`；已存在取消、暂停、设置、关键帧及 RTP sequence 处理。
- `host/src/service.rs` 对 MainDisplay 跳过 catalog 校验，来源替换先终止旧 task；Connection 超时 15 秒清理。`capture_sources::list()` 目前也没有 per-peer lease 参数。
- `ffmpeg_hevc::drain_hevc_reader` 当前在满队列时丢弃压缩 AU。这个策略不能直接复制到新适配链，因为它可能丢失参考帧。

因此“只换 capturer 文件，其余零改动”无法成立。建议限定在 Linux 平台接线：激活已有 submit_frame 接口，给现有 libx265/Annex-B 管线添加 raw-frame 输入模式，保留公共 traits、网络 wire 格式和 GUI 引擎。编码输入、压缩帧背压及受限 source lease 接线作为同一项实施范围确认，不能拆成小步绕开确认。

## 文件及依赖清单（拟实施）

| 文件 | 具体职责和现有接口 |
| --- | --- |
| 新 `host/src/linux_portal.rs` | ashpd 能力读取、一次性选择、Session/Request 的状态与取消；保留受限 FD 和 D-Bus 连接所有权 |
| 新 `host/src/linux_pipewire.rs` | 专用 PipeWire loop 线程、格式/缓冲协商、copy/归还、控制通道；不把 PipeWire 对象跨线程搬运 |
| 新 `host/src/linux_frame.rs` | owned planes、stride/offset 校验、实际像素格式、crop/transform、颜色、PTS、capture generation；纯函数可离线测试 |
| `host/src/linux_capture.rs` | 保留 VideoCapturer trait，用平台 backend 分派替代空帧；capture_frame 交付当前 generation 的自有帧 |
| `host/src/linux_video_encode.rs` | submit_frame 消费真实帧；同一 libx265 编码器模式接入，pull_encoded_chunk 保留输出类型、timing 与关键帧语义 |
| `host/src/ffmpeg_hevc.rs` | Linux raw input 模式及有界 writer，保留 HEVC 输出解析；处理取消下 reader/writer 退出，不丢压缩参考帧 |
| `host/src/lib.rs` | StreamingRunConfig 的 Linux 内部 lease 接线，settings 只调输出编码/FPS，不触发新的用户选择；其他平台管线保留 |
| `host/src/capture_sources.rs`、`host/src/service.rs` | per-peer/source-revision lease 注册、准备后提交换源、错误/能力准确；维持现有 Session/auth 隔离 |
| `host/src/lib.rs` 模块声明、`host/Cargo.toml`、Cargo.lock | 仅 Linux 平台依赖；锁定并审查精确新增包集合 |
| 原 GPUI 的 `app/src/restored_ui.rs` 等现有 host 分享入口 | 仅在确认交互方式后接一次性系统选择动作及现有窗口 parent handle；不增加另一 GUI |

建议 Linux-only 控制面复用锁内 `ashpd = =0.11.1`，default-features=false、async-std，与现有 `third_party/gpui-ce/Cargo.toml` 一致。不要为同版启用 tokio：该 crate 明确 compile_error 禁止 async-std+tokio 同开。控制任务可与现有 Tokio 任务经通道协作，不改 GPUI 的运行时配置。

取消需要窄 zbus request shim：检查的 ashpd 0.11.1 `Proxy::request` 会等待 Response 才返回 Request，直接 drop start future 无法保证关闭系统对话框。shim 复用现有 `zbus = =5.15.0`（async-io）和 `futures-util = =0.3.32`，持有预登记的 request handle，负责 Close 和竞态处理；不修改整个 GUI 依赖栈。[ashpd 官方仓库](https://github.com/bilelmoussaoui/ashpd)

数据面候选锁定 `pipewire = =0.10.1`（当前官方绑定文档版本，实施前验证目标机库/API）。使用其 spa/sys re-export，避免无必要的重复 libspa facade。构建前核对 pkg-config 的 PipeWire/SPA 头文件及绑定所需 clang/libclang，最低 C API 特性以目标环境实测决定，不自行安装系统包。[官方 Rust 绑定](https://pipewire.pages.freedesktop.org/pipewire-rs/pipewire/)

## 选择、会话与取消

建议初版 multiple=false、persist_mode=0、不存 restore_token；只展示运行时可用的 MONITOR/WINDOW，不开 VIRTUAL。标准不支持用 app_id/PID/标题静默指定窗口。SelectSources、Start 每 session 各一次，换源建立新 session；成功结果核对 source_type。返回的 size/position 是 compositor 逻辑几何，像素尺寸取自实际格式。本任务已观察 HO5 portal version=5；v6 pipewire-serial 只能作为可选增强，旧版 node ID 必须与 session/generation 绑定，失效后不能重用。[ScreenCast 规范](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html)

建议状态为 Idle → Creating → Selecting → Starting → Ready → Streaming/Paused → Closing → Closed，任意阶段可取消/失败。预登记 Response，再发请求；超时或取消主动 Request.Close 后直接结束等待，因为 Close 不会再发 Response。对本端和用户关闭都监听 Session.Closed。撤销后清 generation、停止交付/编码、清队列、关闭资源；不得退到全屏、X11 或重复弹窗。[Request 规范](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Request.html)，[Session 规范](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Session.html)

OpenPipeWireRemote 的受限 FD 只用于该 session。通过 Rust OwnedFd 将所有权转交 PipeWire connect_fd；其断开/失败负责关闭，不能 double-close，也不能用默认全局 PipeWire remote 绕过限制。保持 D-Bus 连接和监听直到 lease 关闭。[PipeWire Core](https://docs.pipewire.org/group__pw__core.html)

GPUI 可用现有 parent-window 标识接 portal；无窗口的 headless host 可按标准使用空 parent，仍须本机用户正常选择。不能为 parent 另建 GTK app，也不能静默同意系统对话框。[窗口标识规范](https://flatpak.github.io/xdg-desktop-portal/docs/window-identifiers.html)

## 帧输入与现有编码链

专用 PipeWire 线程持有 loop/context/core/stream/listener。param_changed 决定实际格式、宽高、各 plane 布局和颜色；process 回调只验证、复制到自有内存并交给有界 latest-frame 槽。原 Buffer 在回调结束前归还，只有 owned bytes 和 metadata 可跨线程，不留借用指针。[PipeWire Streams](https://docs.pipewire.org/page_streams.html)，[Rust Buffer 的归还行为](https://pipewire.pages.freedesktop.org/pipewire-rs/src/pipewire/buffer.rs.html)

实现策略是先支持明确协商的 packed BGRx/BGRA/RGBx/RGBA 和 NV12/I420，逐种测试后才声明支持；未实现的格式、P010/HDR、负 stride 等给明确错误，不误读成 RGBA。MemFd/MemPtr 校验 mapoffset、chunk offset/size、stride、每行有效字节、plane 数量及溢出；crop/transform 或 cursor 元数据必须处理或明确拒绝，不能无声省略。可用时优先 embedded cursor。此内存路径不包含 DMA-BUF fallback；DMA-BUF/同步单独后续审批。[缓冲布局](https://docs.pipewire.org/group__spa__buffer.html)

保存 SPA header pts/seq 与本地收到帧的单调时刻；映射到现有 FrameTimingCheckpoints 时明确时钟域，不能把 PipeWire 纳秒时间当 Unix 微秒。无 PTS 标记缺失并使用有标签的到达时间，不假称真实采集时刻。格式、像素尺寸、crop/transform、颜色变化启动新 encoder generation 和首个关键帧/SPS；保持实际颜色或 unknown，不按分辨率猜矩阵。现有 macOS `SourceColor` 含 CFString/VideoToolbox，不能直接跨平台复用。[SPA header](https://docs.pipewire.org/structspa__meta__header.html)

保留 libx265 与输出 Annex-B，raw writer 对接 submit_frame。raw 帧未提交编码前可合并旧输出，仅保留一张待提交最新帧；已提交帧保持顺序。压缩 AU 必须背压/完整发送，不能用当前满队列丢包策略；取消需先关闭接收/写入通道，再 kill/wait 子进程并 join 线程，避免 blocking_send 与 Drop 互相等待。帧 timing 必须关联到对应 AU，不沿用当前 pull 时生成“capture timestamp”的做法。这些是平台 encoder 输入接缝及正确性要求，尚未实施。

## 能力、绑定与恢复的决策点

1. **实际源如何进入现有 typed catalog。** 推荐本机用户针对指定 peer 发起分享，再将实际授权的显示器/窗口注册为该连接范围内的 Display/Window opaque ID，不能把 PipeWire node ID 当跨 session 稳定 ID。registry 归属已验证 peer/share scope；未选择只显示“需本机选择”，ListSources 不暗自弹窗。若需新增远端“系统选择器”能力/wire 字段，应明确单独协议评审，不能伪装为 MainDisplay。portal 不保证 PID/标题，window 的 process_id 可为 None，不套用 macOS PID 输入方法。
2. **输入边界。** ScreenCast 授权不含输入授权。窗口先 supports_input=false；显示器也须确认实际 source→桌面坐标映射及现有 uinput 可用性才能声明输入。不能对任意被选监视器直接复用 root 的绝对坐标，更不能把窗口输入降级为全桌面。若要 RemoteDesktop/libei，另列权限和实施计划。保留当前 X11 输入能力及其他平台已有窗口输入。
3. **取消换源保留旧内容。** 当前服务先停旧流。建议先完成新 lease 选择和格式准备，校验原 peer、连接、share scope、source revision 后再提交并关闭旧流；取消/准备失败保持原流。未选定的新来源不能接受输入或替换当前会话。
4. **传输与采集生命周期。** 传输暂失只重建传输，不再次选择或复用失效节点；native capture 失效则清 generation 并停止。当前 lease 若仍存活，可在有界宽限内暂停交付，只有验证为同一身份/scope 的新连接才能重新绑定。现有 Connection 超时回收语义需要内部 lease 归属决策；无法证明同一身份时关闭，不能按 IP/昵称迁移。取消/撤销不自动重试，显式用户操作才重新选择。
5. **初版 CPU 路径的质量界限。** MemFd/MemPtr copy 仅是受限可验证适配，不宣称平台最优性能。测 capture/copy/encode/传输/实际显示分别的成本、帧龄、内存上界和画质；DMA-BUF/显式同步为独立后续计划。高分辨率、HDR/不支持格式不得悄悄降低质量来制造通过结果。

以上编码接缝、source 交互及 lease 归属是实际触及平台核心接线的决策，实施前确认本方案范围。依据 [仓库 AGENTS.md](../../AGENTS.md)：“Before substantial GUI/default-entry, native media, protocol/configuration, platform support, signing/identity or deployment changes, obtain explicit user confirmation of the concrete plan.” 本轮授权的裁剪小修已完成；此提案不是实现批准。

## 可验证入口（拟新增测试，不是假称已通过）

| 入口 | 内容/验收 |
| --- | --- |
| `cargo test -p host --lib linux_portal::tests --offline --locked` | fake portal：一次调用、取消不等待 Response、late response、Closed、version=5 缺 serial、受限 FD 单次转交、换源取消保留旧 lease |
| `cargo test -p host --lib linux_frame::tests --offline --locked` | 固定 MemFd/MemPtr planes：stride/padding、offset/mapoffset、越界/溢出、NV12/I420 色块、crop/rotation、logical≠pixel、缺 PTS/颜色 |
| `cargo test -p host --lib linux_pipewire::tests --offline --locked` | mock buffer 每次归还一次、无借指针跨线程、latest 槽保留最新帧、generation 切换、关闭/重连 |
| `cargo test -p host --lib linux_video_encode::tests --offline --locked` | raw 帧进入 encoder、全部象限与 SPS 尺寸、颜色变化新 generation、关键帧、timing/AU 对应、满队列不丢参考 AU、取消无死锁 |
| 现有 Session/source isolation tests | stale request、换 peer/连接/scope、窗口 ID 复用、取消及输入释放；源目录跨 peer 不泄漏 |
| 新 bounded actual-product 入口 | 经本机选择 task-owned 动态窗口或明确显示器；保存同一时刻源与真实 GPUI 窗口画面，核实内容/黑屏/冻结、尺寸、哈希、亮度及撤销/暂停恢复。此入口在上述实现完成后建立，当前未重复实验 |

只测试元数据/解码/GPUI submission 不算显示验收。现有已通过检查保留，发布前仍运行仓库 release tests、签名/GUI gate 和原有功能检查；不替换正式安装、不改身份/launch agent、不新建 target、不批量部署，保留每设备回滚。
