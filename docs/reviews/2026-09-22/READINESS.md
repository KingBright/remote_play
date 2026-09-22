# RemotePlay 目标差距与本轮优化（2026-09-22）

> 历史审查记录：当前大版本实现及未完成项请以 [V2-ACCEPTANCE.md](V2-ACCEPTANCE.md) 为准。

> 这是本次工作的第一阶段记录。后续已刷新远端、核对全部工作树，并继续修复和实测原生解码播放；最新结果见 [AV-PERFORMANCE.md](AV-PERFORMANCE.md)。下文中的“未测真实解码”和“远端分支未刷新”仅描述第一阶段。

> 后续补充：[多窗口、双向投屏与数据协作路线](MULTI-WINDOW-ROADMAP.md)。发送端本地完成误显示送达、目录首个子文件误终结整组的问题已修复；文件 ACK/补传/续传仍未实现。Android 本轮新增接收画面的实际比例适配并通过构建和尺寸单元测试。

目前不能承诺“任意两台设备都能完成桌面音视频、控制和文件互传”。macOS 的实现最完整；Windows/Linux 的主路径是被控端；Android 是观看和控制端。核心网络、媒体和文件模块已经具备，但仍有产品功能缺口和端到端验收缺口，不适合用一个完成百分比代替。

本轮以本地 `main` 的 `969ca10` 为基线检查。初始工作区干净；修复保留在工作区，尚未提交、打包或部署。范围暂按 Windows/macOS/Linux 电脑互控、Android 作为控制端；手机作为被控端、iOS 单列。远端分支未刷新，本文的“最近”指本地可见提交。

## 最近改动

9 月 12—14 日可见 15 个提交。9 月 12 日加固之后的 14 个提交涉及 64 个文件，增加 4,347 行、删除 4,846 行。

| 方向 | 已有改动 | 仍需区分的事实 |
| --- | --- | --- |
| 连接 | 原生 UDP rendezvous/P2P、LAN→P2P→WSS relay 路由、穿透加固、移除 EasyTier | 单元测试和本地转发通过不等于跨运营商 NAT 矩阵验收 |
| Android | 共用原生发现与路由、签名发布、NAS 版本发布、固定 JDK 17 | APK 可构建不等于文件、剪贴板或手机被控已经实现 |
| 性能稳定性 | 媒体队列、抖动缓冲、遥测预算与报告开销修复 | 本轮未重测真实端到端延迟、功耗和音画同步 |
| 平台适配 | Windows 剪贴板、音频 cfg、文件选择依赖隔离、macOS 设备组目录隔离 | 编译成功不等于拥有该平台观看界面和媒体解码输出 |

## 功能矩阵

“有实现”指源码路径存在，不代表本轮完成两台物理设备的功能验收。

| 平台 | 作为被控端：画面与控制 | 作为观看/控制端 | 桌面系统音频 | 文件传输 |
| --- | --- | --- | --- | --- |
| macOS | ScreenCaptureKit/VideoToolbox/CoreGraphics 已接入，依赖系统授权 | 原生界面、解码、渲染、输入已实现 | 已有实现，仍需 `REMOTE_PLAY_SYSTEM_AUDIO=1`；默认麦克风流不能替代桌面声音 | 收发、目录组、取消、校验已有；本轮验证双向本机路径，弱网可靠性未闭环 |
| Windows | FFmpeg gdigrab/libx265 软件路径与 SendInput；本轮修复键鼠映射 | GUI 与真正的视频解码/渲染仍缺失 | 当前复用 cpal 默认输入设备，缺少系统输出 loopback | 共享内核/被控接收路径已有；缺少完整原生发送界面和双机验收 |
| Linux | FFmpeg X11/KMS 软件路径与 uinput；现代 Wayland portal/PipeWire 路径未完整接入 | GUI 与真正的视频解码/渲染仍缺失 | 当前为默认输入设备，缺少系统输出 monitor 路径 | 共享内核/被控接收路径已有；缺少完整界面和双机验收 |
| Android | 没有手机屏幕采集及被控服务 | MediaCodec/Opus、触控映射、发现和路由已接入 | 能接收主机发送的音频；本轮未测真机扬声器 | bridge 的 `file_transfer_control` 为 `None`，未接入 |
| 浏览器 | 没有桌面被控实现 | WebCodecs UI 存在，缺少原生 relay 协议适配 | Web 完整音视频协议链路未打通 | 未接入 |
| iOS/iPadOS | 未发现实现 | 未发现原生客户端 | 未实现 | 未实现 |

源码位置：`app/src/lib.rs` 的非 macOS GUI 分支；`client/src/lib.rs` 的非 macOS 解码分支；`client/src/audio_player.rs` 的非 macOS音频丢弃分支；`host/src/ffmpeg_hevc.rs`；`host/src/lib.rs` 音频任务；`remote_client_bridge/src/lib.rs` receiver 配置；`web/src/client.js` gateway 要求。

Linux 的独立 `LinuxVideoCapturer` 仍有空帧占位；实际像素由 FFmpeg 编码源自身采集。不能把文件名和接口数量当作 PipeWire/VA-API 零拷贝已经实现的证据。

## 影响目标的主要缺口

1. **文件可靠交付尚未完成。** `FileTransferControl` 目前只有取消消息，没有接收完成确认或补块请求。发送器把 envelope 交给 UDP 调度器；`OutgoingCompleted` 在读完本地文件后产生，UI 随即显示完成。`Reliable` 通道名称表示调度类别，不能证明丢包重传、落盘确认或断线续传。需要补齐确认、重传、超时与失败语义，并通过丢包测试。
2. **电脑双向角色不完整。** Windows/Linux 原生观看端没有完整 GUI、视频解码/渲染和音频输出。仅补被控端性能无法实现所有电脑互控。
3. **桌面声音覆盖不足。** macOS 有可选系统音频；Windows/Linux 目前是麦克风输入。产品应明确系统音频、麦克风、对讲三个开关，并各自报告可用性。
4. **移动和 Web 的功能未对齐。** Android 需接文件选择、持久化接收目录、传输状态与权限；Web 需真正的网关或支持浏览器的传输协议以及音频接入。iOS 和手机被控属于新增平台工作；不能假定移动操作系统允许与桌面相同的任意控制能力。
5. **文件服务依赖媒体会话。** 当前先启动捕获与编码，再启动文件 runtime；没有独立文件会话。录屏权限或编码失败会连带阻止文件传输。
6. **配对与会话鉴权未统一。** `load_session_psk()` 只读取 `REMOTE_PLAY_SESSION_PSK`；未设 PSK/`REMOTE_PLAY_REQUIRE_AUTH` 时会话鉴权可选。设备组的 P2P/relay capability 不等于默认 LAN 端口已经强制鉴权。正式交付前需明确可信设备、授权、撤销和必需的会话保护。
7. **跨网络、跨设备验收不足。** 需要真实首帧与显示、可听系统音频、鼠标/组合键/释放、双向文件最终哈希、断连与恢复。已有历史双 Mac 中继记录不能代替当前版本全平台验收。

对应关键位置：`protocol/src/lib.rs::FileTransferControl`、`remote_core/src/file_transfer_runtime.rs::send_reader`、`remote_core/src/scheduled_sender.rs::send_scheduled_envelope`、`client/src/transfer_center.rs`、`host/src/lib.rs::run_streaming`、`remote_core/src/session_crypto.rs::require_session_auth`。

## 本轮修复

- 统一应用的剪贴板、文件和对讲开关传入 `HostServiceConfig`，修复界面/发现宣告开启、被控端却只读取环境变量的问题。保留独立 host 入口的环境配置。
- 发现信息根据平台实现及是否启用观看媒体宣告 `can_view`，Windows/Linux 不再宣告未实现的观看与对讲能力。
- Windows 修复普通字母、数字、标点、功能键和导航键映射，拒绝把未知 macOS 键码直接当作 Windows 虚拟键。补齐常用修饰键同步、导航键扩展标志、会话结束时释放已注入按键；纠正右键和中键编号。
- Windows/Linux 的 FFmpeg 启动错误向上传递，移除返回空视频数据的成功路径。FFmpeg 启动后的运行时错误仍需进一步改进诊断；这次没有替换媒体后端。
- Windows/Linux 的 cpal 回调改为非阻塞入队，队列满时不阻塞实时音频线程。
- headless smoke client 增加可选上传输入，收到首个媒体包后提交文件，用独立落盘与哈希检查覆盖默认被控文件服务。

本轮没有增加协议版本或改变现有 wire 格式。

## 本轮验证

| 检查 | 结果 | 证据 |
| --- | --- | --- |
| macOS Rust workspace/all-targets | 362 项通过，0 失败 | `rust-tests-final.log` |
| macOS Clippy | `--workspace --all-targets -- -D warnings` 通过 | `clippy-final.log` |
| 格式与差异 | `cargo fmt --all -- --check`、`git diff --check` 通过 | 本轮执行结果 |
| Windows GNU 交叉检查 | workspace 通过，平台未完成分支仍有 warnings | `windows-check-final.log` |
| Web | 11 项通过 | `web-tests.log` |
| 主机→观看端文件/媒体 | 216 个视频包、338 个音频包、64 KiB 文件接收完成 | `media-file-smoke.log` |
| 默认配置：观看端→主机上传 | 未设置文件开关环境变量，1 MiB 落盘，SHA-256 完全一致 | `default-services-result.json` |
| 系统音频 | 269 个视频包、924 个音频包；麦克风及系统音频各 1 份配置 | `default-services-probe.log` |

1 MiB 文件 SHA-256：`c1568ef846c82e2a1df7c900581a5a6edb26b947a051ec405fbf8af090385057`。

以上媒体冒烟证明实际捕获/编码/协议收包和文件落盘，没有测量实际显示首帧、扬声器输出、音画同步或物理输入效果。视频/音频计数是包数，不是显示帧数；`file_completed=none` 出现在上传探针中，是该探针未接收下行文件，上传结果由主机落盘哈希独立证明。

Linux HO5 在线且源码为 `969ca10`，发现两个已有 RemotePlay 进程；Windows cube 当时离线。本轮未替换这些进程。传往独立验证目录的候选源码受 Remote Hosts `source_dns_or_connection` 阻挡，传输已取消并确认未改动目标文件，因此不声称 Linux 候选版本已经构建或运行验收。Android 本轮仅检查源码及已有构建文档，没有重新构建或真机验收。

`default-services-probe.py` 保存本次临时探针，使用本机 `/tmp/remote-play-review-target` 已构建二进制和独立 49372 端口；它是本次证据附件，不是跨机器通用发布脚本。

## 建议交付顺序与完成条件

1. **先完成可验收的桌面基本链路。** 优先修复文件 ACK/补块/超时与接收成功语义、明确配对鉴权；用两台 Mac 双向验证系统音频、输入、目录传输和断线恢复。完成条件是接收端落盘确认、真实显示和声音都通过。
2. **补全 Windows/Linux 双向能力。** 实现观看界面、视频硬件解码与渲染、音频播放、系统声音采集，再完成权限引导和真实桌面控制。Wayland 与 Windows 的现有 FFmpeg 软件路径需分别做兼容性和性能验收。
3. **接入 Android 文件功能。** 复用修复后的文件协议，加入系统文件选择/目录访问和可恢复传输；完成 Android→三种桌面主机的实机测试。
4. **再扩展 Web、iOS、手机被控。** 按明确的平台支持范围分别立项和验收，不纳入已有桌面实现的完成度。

只考虑三个桌面系统与 Android 控制端，最小平台矩阵也有 3×3 个桌面角色组合加 3 个 Android→桌面组合。每个组合至少覆盖 LAN、P2P、relay，及两端角色切换、权限拒绝、休眠重连、丢包、限速、大文件与目录传输。当前距离目标仍有上述功能开发，不能只安排性能微调或一次打包发布。
