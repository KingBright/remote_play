# 媒体载荷暂停与核心服务归属：具体改动范围

状态：2026-10-14 用户澄清后的方案，尚未实施。用户目标是保留传输连接和会话，在无实际消费需求时暂停载荷，恢复时复用已有状态。此前“一律后台继续传输”的未提交草案已经撤回；该草案的测试通过不能作为本方案的验收。

## 已核实的现状

| 边界 | 当前行为 | 证据 |
| --- | --- | --- |
| 控制协议 | `SetActivity` 修改已有订阅的 video/audio 状态，不删除连接或订阅；`Unsubscribe`、`Close` 才结束资源生命周期 | `host/src/service.rs::Connection::set_activity`、`remote_core/src/workspace_session.rs::Actor::prepare_control` |
| 保活 | 接收侧保留 5 秒控制心跳；暂停侧发送低频遥测，无需新增空媒体包 | `remote_core/src/workspace_session.rs::Actor::run`、`host/src/lib.rs` 暂停遥测分支 |
| 多订阅 | 视频状态按连接内 subscription ID 区分；现有音频组以成员需求的 OR 决定暂停，不应破坏这个聚合 | `host/src/service.rs::refresh_audio` |
| macOS 视频 | pause 停止并释放 SCStream，resume 重新创建；VideoToolbox 编码对象仍由原任务持有 | `host/src/capture.rs::MacVideoCapturer`、`host/src/lib.rs` 视频循环 |
| macOS 系统音频 | pause 同样停止 SCStream；采集队列被清理 | `host/src/audio_capture.rs::MacSystemAudioCapturer` |
| Linux Portal 视频 | 暂停 mailbox 保留 Portal 会话；原始帧编码器会丢弃 FFmpeg 实例，恢复再创建 | `host/src/linux_capture.rs`、`host/src/linux_video_encode.rs::set_paused` |
| Linux/Windows 兼容视频 | pause 将 FFmpeg source 置空，Drop 杀死并回收子进程，resume 重启 | `host/src/linux_video_encode.rs`、`host/src/windows_video_encode.rs`、`host/src/ffmpeg_hevc.rs` |
| 关键帧 | macOS 能对下一次编码请求关键帧；现有 FFmpeg 配置 keyint=fps，约每秒一个关键帧，恢复标志本身不能证明下一帧即 IDR | `host/src/video_encode.rs::request_keyframe`、`host/src/ffmpeg_hevc.rs`、`host/src/linux_raw_encode.rs` |
| 音频旧数据 | 发送侧清理部分包并保留 stream/sequence ID；接收端尚无明确的 payload 暂停事件来统一清理 jitter、解码及播放积压 | `host/src/lib.rs` 音频循环、`client/src/audio_player.rs`、`remote_core/src/client_session.rs::AudioIngressEvent` |
| GUI 所有权 | GUI 创建 WorkspaceConnection、SessionState、ClientMediaRuntime；关闭 GUI 清空会话。后台只承接发现、路由和主机角色；推送接收仍启动 GUI | `app/src/desktop/original_owner.rs`、`app/src/desktop/model.rs`、`app/src/background_service.rs`、`app/src/directed_delivery.rs` |
| 现有 IPC | 已有同用户、受 profile/generation/可执行文件身份约束的控制通道；目前没有解码后的原生视频帧跨进程通道 | `app/src/background_service.rs`、`app/src/private_ipc.rs` |

不能再把 `video=false/audio=false` 本身称为断开连接。现有问题在于需求来源与 GUI 焦点混用，以及平台 pause 释放原生采集/编码资源。

## 拟实施的完整范围

1. **在现有会话所有者中合并消费者需求。** 区分视频、音频需求、输入焦点、载荷暂停、显式结束、实际故障。窗口焦点仅管理输入；可见呈现、音频播放和已有定向接收意图分别登记其真实需求。不得凭空把文件传输算成音视频需求，也不新增用户授权开关。同一会话的需求按 OR 合并；带 generation 的消费者登记与移除，防止旧窗口回调停掉新消费者。现有每连接/订阅边界和音频组聚合保留。

   修改范围：`app/src/desktop/model.rs`、`app/src/desktop/original_owner.rs`、`app/src/restored_ui.rs`，及其原产品测试。沿用 `SetActivity`/`Activity` 的 revision 与重试，不更换网络会话协议。平台可见性不足时明确保守策略，不将“非键盘焦点”自动等同“无消费者”。

2. **将载荷暂停与资源销毁分开。** 暂停时保留连接、订阅、Portal lease、必要采集对象和编码状态；有界持有最新原始/已解码帧，清理过时负载。恢复请求新可解码帧，维持来源与序列身份。真正停止、故障或授权撤销仍执行原来的完整释放。不得简单缓存或任意丢弃压缩参考帧后继续解码。

   修改范围：`host/src/lib.rs`、`host/src/capture.rs`、`host/src/audio_capture.rs`、`host/src/linux_capture.rs`、`host/src/linux_video_encode.rs`、`host/src/linux_raw_encode.rs`、`host/src/linux_system_audio.rs`、`host/src/windows_video_encode.rs`、`host/src/ffmpeg_hevc.rs`。优先在两台 Mac 上验证保留 SCStream 与 VideoToolbox 会话；Linux/Windows 原 FFmpeg 路径需要明确验证进程保留、输出排空和关键帧恢复策略，不能靠改布尔值宣称完成。未实测的平台不得承诺一帧恢复。

3. **补齐接收侧恢复边界。** 保留 WorkspaceConnection、解码对象和输出设备；暂停/恢复时按实际音频组清理旧 jitter、播放队列和时间基线，防止补播或突发输出。视频丢弃旧呈现结果并等待有效恢复帧；输入仍需当前来源、租约和新呈现证据。保持文件、剪贴板与控制收发独立于 A/V 载荷。

   修改范围：`remote_core/src/workspace_session.rs`、`remote_core/src/client_session.rs`、`client/src/lib.rs`、`client/src/audio_player.rs`，必要时复用/扩展现有 AudioMixer 的按流清理接口。不用卸载再重建整个播放运行时来模拟“快速恢复”。

4. **把接收生命周期归入现有核心服务。** 由 `UnifiedServiceOwner` 持有接收连接、会话、解码及数据任务；`OriginalOwner` 成为界面适配层，窗口关闭移除其消费者，不直接清空核心会话。定向接收 worker 调用核心会话，GUI 仅在确需显示时附着。扩展现有同用户 IPC 传递会话命令、需求和状态，不新增服务框架。

   修改范围：`app/src/lib.rs`、`app/src/background_service.rs`、`app/src/directed_delivery.rs`、`app/src/desktop/model.rs`、`app/src/desktop/original_owner.rs`、`app/src/restored_ui.rs`、`app/src/private_ipc.rs`。解码后的原生帧必须通过平台原生资源句柄及同步信息跨进程呈现；当前 JSON 快照通道不具备该能力。需在现有 native presenter/buffer 适配处加入有界句柄传递、生命周期和完成回执，保持零拷贝目标，不以 CPU RGBA 中转或“已提交 UI”等同实际显示。此部分是实质架构改动，不能包装成几处字段搬移。

以上是同一个具体方案的阶段实施顺序，不能用拆小提交替代整体范围确认。沿用主 main、既有 target、签名和发布脚本，不新增源码副本、平台或权限变更。

## 验收与测量

- 原产品协议测试：暂停期间连接 ID、subscription ID、source revision 和保活保持；无重复握手、订阅、系统选源；显式结束只结束指定会话。
- 多消费者：A 隐藏而 B 仍消费时载荷继续；最后一个消费者离开才暂停；旧 generation 的 detach 不影响新需求；视频与音频独立聚合。
- 原生资源：记录 pause/resume 前后的采集、编码、解码实例/进程及 Portal 会话，验证预期复用；长期暂停和显式结束均无资源泄漏。
- 恢复延迟分段记录：需求恢复→控制确认→首个新捕获帧→首个可解码载荷→首个新解码帧→实际呈现；至少报告多次样本的 p50/p95/max、帧率、编码器、平台与路由。不用单一 UI 提交时间宣称端到端一帧恢复。
- 音频：暂停前后可识别的受控音源，核对序列/时间戳、积压边界与非零输出，验证无旧音频突发补播；非零样本本身不等于无爆音。
- 两台 Mac 先行；HO5 须先获得正常本地 Portal 选择；cube 当前离线，Windows 实机数据保持未知。Pixel 不动。

## 本轮 HO5 的精确边界

2026-10-14，一次 15 秒窗口诊断：KDE Wayland、wayland-0、DISPLAY=:0；自建窗口 `RemotePlay Capture Probe - pure test colors`，960×540。

- CreateSession：10 ms 收到成功响应。
- SelectSources：12 ms 收到成功响应。
- Start：13 ms 方法返回并开始等待 Response；没有收到成功/取消 Response。
- 15001 ms：测试自己的 15 秒 deadline 发起取消；raw_frames=0。不是 Portal 超时响应，也不是黑帧判定。
- fixture/probe 均已退出，原已安装 RemotePlay 未替换。
- D-Bus 确认 Desktop portal PID 3348、KDE backend PID 3446。AT-SPI 只见 GTK portal 应用，没有暴露 KDE picker。因此**实际系统窗口标题、确认按钮和是否被遮挡尚未证实**，不能编造精确点击步骤。需要 HO5 本机用户在下一次受控调用时观察正常系统选源窗口并选择上述测试窗口；本轮不重复启动等待，不代替用户绕过系统选择。

操作证据：build `d72f68d4-1f9d-484d-bf51-0773522c061d`；窗口诊断 `547a0769-6135-41a4-ac90-e3e73d66536a`。诊断二进制 SHA-256：`24f9e5c5b656dede5d451ad4282c0a5fecec22195a546218097dca6678e457c1`。
