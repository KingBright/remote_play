# Windows 串流失败与网易 UU 共存事件

## 当前结论

Windows cube 上的 RemotePlay 继续保持停用：进程数为 0，HKCU Run 的 RemotePlayHost 自启动项仍未恢复。本轮只构建、测试和打包候选版本，不启动桌面捕获，不修改网易 UU、sing-box、防火墙、路由、虚拟显示驱动或设备网络身份。

事故不能归结为“RemotePlay 已证明导致 UU 崩溃”。RemotePlay 停止后，UU 的 GameViewerIddDriver.dll 在 2026-09-25 13:34:12（UTC+08:00）又独立发生了一次与 12:47:13 相同的用户态驱动崩溃。因此 RemotePlay 不是该故障发生的必要条件。RemotePlay 与 UU 的资源、显示共存仍需严格设计。

## Windows 事件证据

System / Microsoft-Windows-DriverFrameworks-UserMode：

- 2026-09-25 12:47:13：10121 报告 GameViewerIddDriver.dll 运行时故障；10120 报告用户态驱动宿主终止；10111 报告 GameViewer Virtual Display Adapter 因崩溃离线。
- 2026-09-25 13:34:12：再次出现同一组 10121 / 10120 / 10111。此时 RemotePlay 已从约 12:50 保持停止且自启动关闭。
- 2026-09-25 14:53 复查时，GameViewer Virtual Display Adapter 与 AMD Radeon(TM) 8060S Graphics 均为 OK / CM_PROB_NONE，GameViewerService 为 Running。

这些事实支持“UU 的 IDD 驱动自身也会失稳”，但没有 UU 内部调用栈，不能进一步判断其根因。

## RemotePlay 原 Windows 实现的确认缺陷

1. 发布包遗漏 FFmpeg，但 Windows 编码器依赖 gdigrab + libx265。
2. 原命令使用 gdigrab -i desktop，没有明确限定物理显示区域。
3. FFmpeg stdout 在异步串流任务里同步阻塞读取，取消订阅可能等待管道。
4. WindowsVideoCapturer 额外启动 blocking_send 定时线程，它不提供真实像素却增加生命周期复杂度。
5. 动态设置只更新高度和 FPS，没有同步更新宽度。
6. 旧式桌面查看默认可能带起主机麦克风路径，历史日志中至少出现 7 次 Remote microphone audio capture started。
7. 多视频订阅会分别创建 FFmpeg/x265 进程，资源使用近似线性放大。
8. 捕获运行时缺失时设备仍可能发布 can_stream=true。
9. 系统 PATH 中任意 ffmpeg.exe 都可能被使用，发布行为不确定。

## 当前共存加固

### 失败必须安静

- FFmpeg 能力预检先于音频组、麦克风和视频任务创建。
- 缺依赖时返回明确错误；文件传输等非视频能力继续可用。
- Windows 发现只有在视频运行时预检通过时发布 can_stream=true。
- 旧式桌面查看不再默认授权主机麦克风；仅显式 REMOTE_PLAY_MICROPHONE_CAPTURE=1 才启用兼容路径。

### 不接管第三方虚拟显示

- 使用 EnumDisplayDevicesW / EnumDisplaySettingsExW 枚举附着到桌面的显示适配器。
- 主动排除名称包含 Virtual Display、Indirect Display、IDD 或 ROOT\DISPLAY 类型的虚拟/间接输出。
- 优先选择剩余物理输出中的主显示；cube 的 AMD Radeon(TM) 8060S Graphics 不匹配虚拟显示过滤规则。
- gdigrab 使用选中物理显示的显式 video_size、offset_x、offset_y，不再抓整个虚拟桌面拓扑。
- RemotePlay 不调用显示模式修改接口，不启停、重置或配置 UU 的虚拟显示适配器。

### 资源预算

- Windows 安全版本暂时只允许一个活动视频捕获会话；第二个视频订阅明确拒绝。未来多 viewer 应共享一次捕获/编码，而不是复制编码器。
- Windows 请求预算：最多 2560×1440 像素、60 FPS、20 Mbps。StartStream、Subscribe、Configure 和 UpdateStreamSettings 使用同一预算。
- FFmpeg 进程使用 BELOW_NORMAL_PRIORITY_CLASS，不创建控制台窗口。
- 软件编码限制 filter threads=1、FFmpeg threads=2、x265 pools=2 / frame-threads=1。
- 删除 Windows 伪捕获后台线程，改成 Tokio 内轻量节拍；pause/stop 不遗留独立 capture worker。
- FFmpeg stdout 由独立 reader 线程持续排空，进入容量 4 的有界异步队列；过载时丢弃编码单元，不阻塞生产者、不无限增长内存。
- Tokio 串流任务异步等待该队列，取消任务不再阻塞在同步 Read。
- 重启编码器或恢复视频后等待新关键帧再输出。

### 可重复发布

Windows 默认只接受：
1. 安装目录的私有 bin/ffmpeg.exe；
2. 同目录私有 ffmpeg.exe；
3. 显式绝对路径 REMOTE_PLAY_FFMPEG_BIN。

系统 PATH 回退默认关闭，仅调试时显式 REMOTE_PLAY_ALLOW_SYSTEM_FFMPEG=1 才允许。

运行前探测：
- ffmpeg -devices 必须包含 gdigrab；
- ffmpeg -encoders 必须包含 libx265。

新增 scripts/package_windows_unified.ps1：
- 跟随实际 CARGO_TARGET_DIR；
- 构建或复用当前 release remote_play.exe；
- 强制校验私有 FFmpeg；
- 将 remote_play.exe、bin/ffmpeg.exe、FFmpeg license 输出和 build-info.json 一起打包；
- 不安装、不启动程序。

## 验证证据

Windows cube 隔离源码副本：

- cargo test -p host --lib --release --locked -j4：20 passed / 0 failed。
- 覆盖：连续 16 次失败订阅不创建音视频资源；私有 FFmpeg 解析；系统 PATH 默认禁用；虚拟显示过滤；物理显示选择；分辨率宽高更新；FFmpeg 显示区域与线程预算；有界 reader 过载；单视频 reservation；Windows 安全资源预算。
- cargo check -p remote_play_app --all-targets --locked -j4：通过。
- cargo build --release -p remote_play_app --bin remote_play --locked -j4：通过。
- 所有构建/测试期间 RemotePlay 始终为 0 进程、自启动 False，没有真实桌面采集。

FFmpeg / 包：
- Gyan.FFmpeg.Essentials 9.0.1，隔离探测 gdigrab=True、libx265=True。
- ffmpeg.exe SHA-256：72a489eccd008c2ec2c0a5856c5c75bc3d8bbfa90166c4566865c246445e6aa3。
- 自包含候选包大小：41,792,767 bytes。
- 候选包 SHA-256：42b2dca5d0e8a5fbe6ef4724e627cf902d2dce9a28178c24d15d893654bdfdff。
- 包内 RemotePlay SHA-256：51a143b70dd62087819d2a206763ad33931cf376d299bdc59e4f0ac53f68df54。
- 包内仅包含 RemotePlay/remote_play.exe、RemotePlay/bin/ffmpeg.exe、ffmpeg-license.txt、build-info.json。
- 候选包未安装、未执行、未恢复自启动。

本轮 10 个源码/发布文件经过前后 SHA-256 预检后回写 MacBook 主仓库；git diff --check 通过，没有覆盖同时发生的其他编辑。

## 尚未宣称完成

- 尚未启动修补版做真实 Windows 串流，因此不能声称黑屏已真机闭环。
- 尚未做“UU 正常串流时启动 RemotePlay”的受控 A/B 共存测试。开始前应记录 UU 稳定基线和 DriverFrameworks RecordId。
- Linux 兼容检查仍在独立环境进行；Windows 对 FFmpeg reader 的异步接口同步调整了 Linux encoder。
- Android 对主机明确错误的 UI 和重试策略仍可加强。
- FFmpeg/x265 随产品分发涉及开源许可证义务；候选保存了 ffmpeg -L 输出，但公开发布前仍需按发行方式核对。
- 代码尚未提交、推送或正式发布。

## 客户端错误传播补充

本轮补齐主机错误到客户端的传播链：ClientSessionEvent 增加 HostError；Android bridge 对当前 session 的错误切换到 Error 并清理连接；JNI 暴露 nativeGetSessionError；Android UI 优先显示主机具体拒绝原因；桌面 App 对当前 session 的 HostError 停止该 session。

验证：client_session 目标测试 11 passed；Android bridge 21 passed；Windows host 20 passed；remote_play_app all-targets check 通过；Android bridge all-targets check 通过；HO5 原生 Linux host check 通过且生产服务未重启。全量 remote_core 在 Windows 并行测试有 4 个既有 P2P UDP 并发测试因串包/ConnectionReset 失败，分离本轮相关测试后全部通过。主仓库 MobileConsoleScreen 已有较新并发改动，本轮仅局部合并错误提示，没有覆盖。
