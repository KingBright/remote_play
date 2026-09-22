# 自定义串流参数、媒体暂停与 Android 后台策略

> 历史审查记录：当前大版本实现及未完成项请以 [V2-ACCEPTANCE.md](V2-ACCEPTANCE.md) 为准。

2026-09-22，基于 `main@969ca10`，全部修改仍在主干工作区，未提交或发布。工作树与分支复查只有当前 `main`，本地与已刷新的 `origin/main` 无差异，无待合并分支。此前的优化保留；整体功能差距见 [READINESS.md](READINESS.md)，此前音视频记录见 [AV-PERFORMANCE.md](AV-PERFORMANCE.md)。

后续多窗口及反向投屏设计见 [MULTI-WINDOW-ROADMAP.md](MULTI-WINDOW-ROADMAP.md)。本文已实现的 Android 后台暂停面向当前观看端；将来添加手机投屏源、后台音频和文件传输时，必须分别判断活动任务，不能把观看端的全局超时断连原样套到发布端。

## 已实现的行为

- macOS 与 Android 可输入自定义正整数 FPS、视频 kbps，连接中可以修改并持久化。支持 37 FPS 等非预设值；不支持 29.97 等小数。实际帧率受画面源、显示刷新、编码器及网络限制，码率是编码目标，不是固定网络流量。
- 手动暂停及退后台暂停主机音视频采集、编码和发送，同时保留会话。暂停不发送 `StopStream`，恢复请求关键帧。暂停指令按会话和修订号确认，丢包自动重试，旧指令不会覆盖新状态。
- macOS 保留捕获对象和编码器；Windows/Linux 停止 FFmpeg 子进程，恢复时重建。后两者已做编译检查，没有对应设备的恢复时延验收。
- 设置面板打开时释放远端按键，避免输入参数被送到远端或按键卡住；暂停时降低桌面和画中画刷新频率。Android 清空待播帧及音频，停止 Surface 不可用时的媒体轮询。
- 文件、剪贴板等已建立的旁路在媒体暂停时不主动关闭。本次没有重新验收暂停期间的文件交付；Android 文件模块原有缺口仍在。

## Android 后台策略

| 状态 | 行为 |
| --- | --- |
| 前台观看 | 正常媒体与交互 |
| 短时后台 | 停止媒体；前台服务保留会话，移动端心跳每 5 秒一次，不发送延迟探测；主机暂停遥测每 5 秒一次 |
| 超过保活时限 | 主动停止会话、前台服务及原生发现/P2P/中继网络任务；不在后台循环重连 |
| 返回前台 | 短时暂停恢复原会话；超时断开则重建连接并恢复画面 |
| 用户明确断开 | 清除自动恢复意图，不自动重新连接 |

默认保活 **300 秒**。设置面板可以输入 `0–86400` 秒，`0` 表示一直保活。后台服务每 5 秒检查，正常调度下到期可有约 5 秒偏差；系统休眠、进程终止和厂商策略仍可影响执行时刻或连接。没有新增常驻 WakeLock 或精确闹钟。自动恢复意图保存在进程内，进程被系统终止后不会假装原会话仍然存在。

长时后台断开后的恢复是一条新会话，因此会重新获取关键帧；使用动态 P2P/中继端点时还可能需要重新发现路由。短时恢复的几百毫秒数据不能用于承诺这种冷启动的耗时。

## 已测结果

原始数值及构建产物哈希保存在 [stream-controls-results.json](stream-controls-results.json)。

| 场景 | 实测 |
| --- | --- |
| macOS release，1080p，目标 37 FPS / 3500 kbps，20 秒 | 去掉前 2 秒后观察到 35.61 FPS；采集到解码 p95 13.08 ms；0 解码错误 |
| macOS release，1080p，目标 60 FPS / 8000 kbps，20 秒 | 去掉前 2 秒后观察到 53.22 FPS；采集到解码 p95 12.70 ms；0 解码错误 |
| 暂停 20 秒，在暂停期间改为 37 FPS / 3500 kbps | 允许 1 秒在途数据缓冲后，主机媒体包为零；全程只有一次 StartStream |
| 丢弃第一次暂停请求和确认包，恢复后回放旧暂停指令 | 自动重试成功；暂停确认约 403 ms，恢复首帧约 389 ms；旧指令未误暂停；0 解码错误 |
| Android API 35 arm64 模拟器，短时后台约 25 秒 | 返回后仍是会话 1，UI 有实际解码输出；后台选取的 22 秒窗口双向共 8 个 UDP 包、104 字节载荷，0 媒体包 |
| Android 将保活时限临时设为 10 秒 | 暂停请求后约 10.47 秒发 StopStream；`dumpsys` 确认前台服务已停止；随后 46 秒媒体会话链路零数据报 |
| Android 超时后返回前台 | 自动 StartStream 建立会话 2，UI 恢复实际解码（该截图 19 FPS）；测试后时限已恢复为 300 秒 |

此前 debug 试跑曾观察到目标 37 FPS、主机遥测 37.257 FPS、恢复首帧 262.777 ms。这些是不同负载下的单次结果；本轮连续测试应采用上表的 35.61/53.22 FPS，不能据此宣称所有场景满帧。

macOS 数值是**同机采集到解码**，不含两台设备的网络、屏幕扫描输出和可听音频验收。性能测试期间没有同时编译或运行 Android 模拟器，但这不是完全隔离的空闲系统。Android 模拟器早期试跑曾发生心跳超时，原因尚未定位；后续干净会话和低频保活试验通过，不能替代真机长时稳定性验收。

## 省电结论与系统统一唤醒

“不传视频”依然耗电：CPU 定时器、发现与中继通道、Wi-Fi/蜂窝无线电唤醒都可能有成本。上述 104 字节只统计媒体会话的本地代理链路，**不包含发现与公网中继的全部通信，也不是功耗测量**。蜂窝无线电的活跃尾部可能比载荷大小更影响电量，所以减少包数和最终断开都必要。

可以复用 Android 平台共享的唤醒能力，按场景区分：

1. **用户回到 App 观看**：使用 Activity 生命周期恢复连接，本轮已经实现，无需推送或定时唤醒。
2. **长时后台接收远端连接请求**：有 Google 服务的设备用 FCM；无 Google 服务的市场需要相应厂商推送通道。共享推送连接承担通知可达性，应用不维持自己的媒体长连接。推送只携带短期、可验证的请求标识，唤醒后再鉴权取回状态；不能把推送当作控制授权。
3. **延后同步等任务**：使用 WorkManager 让系统安排执行；它不是即时远程控制的唤醒通道。媒体恢复不依赖轮询闹钟或长期 WakeLock。

FCM 普通优先级消息在 Doze 中可能延迟，高优先级用于需要及时展示给用户的内容，滥用静默消息可被降级。推送不保证即时或必达，也不绕过 Android 的后台启动、录屏及控制权限。远端唤醒应提示用户，再按系统允许的方式进入连接；不能承诺静默启动任意手机被控功能。

**当前没有接入 FCM/厂商 SDK、推送 token 注册或服务端推送。** 这部分需要确定目标手机市场和现有后台服务，并完成真实设备验证；没有添加未配置的 SDK 或占位实现。

官方依据（本轮已读取）：

- [Android Doze：FCM 共享持久连接及电量收益](https://developer.android.com/training/monitoring-device-state/doze-standby)
- [FCM Android 消息优先级](https://firebase.google.com/docs/cloud-messaging/android/message-priority)
- [后台启动前台服务的限制](https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start)

剩余电量验收应在真机分别测 Wi-Fi 与蜂窝：相同屏幕和系统条件下，对照前台串流、后台保活、后台断开至少各 15–30 分钟，用系统电量/唤醒记录与整机耗电共同判断。模拟器不能提供有效电池结论。

## 验证与重现

377 项 Rust 测试、workspace Clippy `-D warnings`、格式与差异检查通过。Windows 交叉编译通过（保留平台已有警告）；bridge 的 Android/wasm feature 检查通过。Android arm64 原生库、debug APK 与 lint 通过；x86_64 Android Rust target 未安装，本轮没有构建或测试该架构。没有对外发布。

```sh
CARGO_TARGET_DIR=/tmp/remote-play-review-target cargo build --release -p remote_play_app --bin remote_play -p client --example playback_probe
REMOTE_PLAY_PROBE_PAUSE_AT=4 REMOTE_PLAY_PROBE_PAUSE_SECONDS=20 \
REMOTE_PLAY_PROBE_UPDATE_AT=10 REMOTE_PLAY_PROBE_NEXT_FPS=37 \
REMOTE_PLAY_PROBE_NEXT_BITRATE=3500 \
python3 docs/reviews/2026-09-22/playback-check.py release-pause-faults \
  --seconds 32 --bin-dir /tmp/remote-play-review-target/release --pause-faults
```

Android 观测器是同目录的 `android-pause-check.py`，使用模拟器连接 `10.0.2.2:39475`，在 UI 中切换前后台和配置保活时间。它仅用于本地明文协议验收，结束时会停止自建主机并写出数据包统计。
