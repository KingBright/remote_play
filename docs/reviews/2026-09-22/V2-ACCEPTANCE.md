# RemotePlay 2.0.0-alpha.1 实现与验收

日期：2026-09-22。代码基线：`main@969ca10922eb17e705877b7ca4cf5ce3bec836c1`。

本次直接在 main 实现。检查时只有一个 worktree，没有等待合并的本地分支。本记录描述提交前的验收状态；本版尚未正式发布。Rust 七个包与 Android 已标为 `2.0.0-alpha.1`，Android versionCode 为 2；Web 客户端仍是原有协议入口。

**这是一版可运行、经过 macOS 真实采集解码管线和 Android 模拟器验收的大版本 Alpha，尚未达到“任意两台设备全部功能验收通过”。** 以下记录取代同目录早期审查中对当前实现状态的描述。早期文档保留作为历史。

## 已落地的变化

| 能力 | 本版行为 | 已验证的范围 |
|---|---|---|
| 连接与媒体分离 | V2 配对鉴权连接独立于视频订阅；不开画面也能传文件；订阅、活动状态和设置有确认与重试 | 本机原生探针、桌面与 Android 模拟器双向文件 |
| 多窗口 | macOS 枚举显示器与应用窗口，独立采集、编码和解码；桌面网格/标签页，Android 网格/标签页 | macOS 两个不同宽高比窗口同时真实解码；Android 真实窗口解码 |
| 隐藏与恢复 | 单独暂停每个订阅的画面/声音；连接继续；恢复请求关键帧；桌面可选择后台保留声音 | 隐藏窗口 2 秒内解码增长为 0，另一个增长 53 帧；恢复首帧 238 ms；关闭一个窗口不影响另一个 |
| 自定义参数 | 每窗自定义正整数 FPS、kbps；可靠 Configure；尺寸变化自动协商，保持源比例并适配编码器对齐 | 37 FPS / 3500 kbps / 640×480 包围框配置成功，实际解码尺寸缩小；旧 Subscribe 重传不会覆盖新配置 |
| 音频 | macOS 按应用 PID 采音，同应用窗口共享音轨；接收端音量调整；音频持续性按可见窗口合并判断 | 协议共享 owner、混音/重采样单元测试；尚未完成跨设备听感、串音和长时间同步验收 |
| 文件 | 有界并发与滑动窗口重传；清单确认后发数据；校验后原子落盘；发送成功等待接收端确认；组整体校验；目录路径与资源上限检查 | 1 MiB 无视频文件传输；丢清单、数据、最终 ACK 和重复包测试；手机↔桌面 53,248 字节文件均 SHA-256 一致 |
| 文件使用 | Android SAF 选文件；收到的文件可浏览、打开、分享、另存副本；FileProvider 使用临时读取授权 | SAF 发送、收到文件列出、系统分享页；另存副本验收见证据文件 |
| 剪贴板 | 文本/PNG/TIFF 与文件组分开传输；文件组完整后才发布文件剪贴板；普通下载不改剪贴板；桌面选择唯一同步目标 | 丢包及丢最终 ACK 后重传且只写入一次；Windows/Linux 真实系统提供器有交叉编译证据，未做相应系统桌面实测 |
| 手机后台策略 | 立即停止媒体；默认闲置 300 秒后断开，前台重连；可手动设时长，0 为持续保留；自适应缩短到省电 30 秒、低电量 60 秒、计费网络 120 秒；充电时使用设定值；文件任务未完成时推迟休眠 | 策略单测；模拟器短后台媒体发送计数停止增长、前台重新出帧；长后台恢复见证据文件 |
| Android 反向发布 | 系统 MediaProjection 选择全屏或单应用；每次授权只有一个 VirtualDisplay；暂停释放编码器、保留有效授权；可选采集允许被捕获的设备播放音频 | 全屏及单应用授权入口、配对认证、文件落盘、硬件不支持时通知接收端并释放投屏/前台服务 |
| 控制 | macOS 工作区主显示器键鼠、Android 接收端主显示器触摸；焦点变化与断开释放按键 | 映射/状态测试；应用窗口为只读；未完成所有设备组合的真实输入验收 |

“自定义”表示接受用户指定的整数目标，实际帧率和码率仍受源刷新、硬件编码能力与网络约束。当前不支持 29.97 等分数帧率。协议最多 8 个订阅，Android 接收界面暂限 4 个，Android 发布端每次授权只暴露一个源。尺寸不是强制 16:9，但编码器可能要求偶数或更大对齐单位。

## 联调中修复的实际问题

- 配对输入未完整提交导致两端组密钥不同：新增手动配对入口，并重新验证加密握手与真实媒体。
- Android 顶部按钮被状态栏遮住：工作区遵守系统安全区域。
- 旧单屏页面进入后台误停工作区网络：网络生命周期现在同时考虑观看工作区与发布端。
- 解码器输入拥塞滞留旧帧：等待超时后清理积压并请求关键帧。
- Android SELinux 拒绝 app 私有目录的硬链接：采用 `renameat2(RENAME_NOREPLACE)`，保持原子发布与不覆盖目标的语义。
- VideoToolbox 首次会话回调上下文存在多余泄漏分配：统一保留一个稳定地址，销毁会话后释放。
- 文件半成品退出清理、符号链接父目录拒绝、组整体校验，以及握手丢包/杂包下的重试边界。

## 验证结论与证据

精简输出、构建结果及 APK SHA-256 见 [V2-EVIDENCE.txt](V2-EVIDENCE.txt)。原始运行日志在本次会话 `/tmp/remote-play-v2-*.log` 中，不作为长期依赖。

- Rust workspace/all-targets：384 项通过，0 失败。一次默认高并发运行出现 P2P 本机测试超时；单独重跑及最终 4 线程全量运行通过。
- Clippy workspace/all-targets：`-D warnings` 通过；Android JNI feature 检查、ARM64 原生库构建通过。
- Android Debug APK、5 项单元测试、Lint 通过；Lint 为 0 error、14 warning，不能称为零警告。
- macOS 原生探针使用真正的 ScreenCaptureKit、VideoToolbox、UDP 与解码器。最后一次双窗分别解码 84 / 93 帧，解码错误均为 0。238 ms 是本机本次恢复样本，不能推广为所有设备或 P95 保证。
- Android API 35 ARM64 模拟器实际收到并解码了 macOS 应用窗口，并验证了两个订阅同时解码；媒体暂停时发送计数保持不变，恢复后再次出现首个解码帧。通过界面将闲置期限设为 1 秒，验证了超时后前台服务退出，返回前台连接恢复、两个订阅再次解码；这不代表数小时功耗验收。
- 模拟器没有可用于本次请求的硬件 HEVC 编码器。反向视频给出了明确错误，MediaProjection 与前台服务随后为空。**反向视频首帧尚未通过真机验收。**
- 模拟器同时启用 Wi-Fi 与 Ethernet 时，控制台 UDP 重定向走 Ethernet；切换到 Ethernet 后反向鉴权正常。这是测试环境路由问题，不能据此宣称公网 NAT 穿透已验收。

## 尚未完成的目标

1. Windows/Linux 原生观看 GUI、视频解码渲染、独立应用窗口采集以及各系统应用音频隔离。现有宿主/剪贴板实现与交叉编译不等于完整互控产品。
2. Android 被控端的用户授权控制服务/输入法，以及跨设备真实输入验收。MediaProjection 权限不包含输入注入能力。
3. Android 同时独立采集任意多个第三方应用，不能按桌面窗口能力承诺。当前支持接收多个远端窗口，以及发布一次系统授权选择的单个源。
4. Android 按指定应用 UID 的音频选择与播放捕获策略验证。目前文案明确为“设备播放音频”，不能承诺只采集系统单应用分享所选应用的声音。
5. 跨会话/跨进程文件断点续传、完整取消交互、发送缓存和接收剪贴板图片的保留/清理策略。当前可靠重传只覆盖存活会话。
6. 平台推送唤醒接入。可以接 FCM/厂商推送作为重连提示，但需要实际推送服务配置；本版未把自己的常驻心跳冒充平台统一唤醒。Doze、系统回收、用户强停与投屏授权失效仍须分别处理。
7. 真实设备双向音视频、声画同步、回声/串音、弱网、4 窗以上资源预算、长时间稳定性及电池功耗测试。尚无“性能极致”或“空闲连接几乎不耗电”的实测证据。
8. 桌面 GPUI 工作区完整交互回归、Web V2 工作区适配、所有目标系统的安装包与正式发布验收。

源应用窗口被系统移出可采集屏幕或最小化时，ScreenCaptureKit 可能不再提供图像；接收端隐藏标签页的暂停/恢复与这种源窗口系统行为是两个不同条件。测试夹具保持可见，没有用可见窗口结果代替所有后台源窗口的验证。

## 复现

在仓库根目录运行；两个 V2 端必须使用同一批次构建，开发期间没有发布中间协议版本。

```sh
CARGO_TARGET_DIR=/tmp/remote-play-review-target cargo test --workspace --all-targets -- --test-threads=4
CARGO_TARGET_DIR=/tmp/remote-play-review-target cargo clippy --workspace --all-targets -- -D warnings
CARGO_TARGET_DIR=/tmp/remote-play-review-target cargo check -p remote_client_bridge --features android
CARGO_TARGET_DIR=/tmp/remote-play-android-target ./scripts/build_android_native.sh
```

本机媒体验收：先运行 `swift app/examples/workspace_windows.swift`，再构建并运行 `app/examples/workspace_probe.rs`。结束后关闭测试夹具进程。

配对设备验收：`workspace_device_host` / `workspace_device_client` 示例读取独立测试组目录；后者通过 `REMOTE_PLAY_TEST_TARGET` 指定目标，`REMOTE_PLAY_TEST_FILE` 指定文件，`REMOTE_PLAY_TEST_FILE_ONLY=1` 仅验收文件。邀请包含配对秘密，不应进入日志或代码库。

Android 构建在 `android` 目录运行 `./gradlew :app:assembleDebug :app:testDebugUnitTest :app:lintDebug`，需设置 JDK 17、Android SDK 和 NDK。当前产物为 `android/app/build/outputs/apk/debug/app-debug.apk`，本次打入 ARM64 原生库，未构建 x86_64 原生库；这是调试 Alpha，不是正式发布包。

平台边界参考：[MediaProjection](https://developer.android.com/media/grow/media-projection)、[播放音频捕获](https://developer.android.com/media/platform/av-capture)、[Android 剪贴板访问限制](https://developer.android.com/about/versions/10/privacy/changes)。
