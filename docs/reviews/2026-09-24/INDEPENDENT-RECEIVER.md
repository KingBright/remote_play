# 独立文件接收与投屏解耦

日期：2026-09-24。主仓库：MacBook `/Users/jinliang/remote_play`，基线 `main@f4c7212`。接续 INTERCONNECT-PROGRESS.md 中的入网、成员发现和文件发送入口修改。本记录不是全产品或真机验收报告。

## 本轮行为

用户可显式开启手机的独立文件接收服务，不要求 MediaProjection 或录音授权。文件接收与屏幕共享分别持有同一个鉴权入站端点；开启、关闭屏幕共享不替换已有文件会话，也不因媒体状态变化重建 P2P/Relay 路由。停止独立接收服务但仍在共享屏幕时，该屏幕会话原有的文件能力仍存在；界面明确要求停止两项才能关闭入站端点。

独立服务使用现有设备组鉴权、调度器、可靠文件传输、完整性校验与应用私有 received 目录。非组内端不能直接创建会话。没有本地屏幕授权时源列表为空，订阅明确拒绝，键鼠注入不开放。撤销授权清除媒体需求、退休旧订阅并拒绝此前排队的媒体；授权变更不清除文件连接。网络发现中 can_stream 表示该端点支持屏幕发布能力，不代表当前已授权任何画面；实际可用源由会话 Sources 返回。

Android 新增 FileReceiverClient 与 FileReceiverService。接收开关、状态、前台通知和停止操作接入现有主界面；服务生命周期使用所有者标记避免旧实例释放新实例资源。服务不自动开机启动，不冒充系统推送唤醒，也不保证系统回收或强停后常驻。开启独立接收或投屏时，原生接口也拒绝直接换组，避免运行中的接收端继续使用旧组密钥。

## 本轮验证

在 Mac Studio 的隔离源码副本 `/Users/jinliang/workspace/.remoteplay-verify-b2` 运行，原始快照 SHA-256 为 `1583dba310e193777166ac21db9a6366daa4bbc84fd928e82ab2512ff79a6301`。修改回写主仓库前逐文件检查旧版本，回写后检查与测试副本的当前 SHA-256 完全一致；不覆盖同期其他修改。

- `cargo test -p remote_core -p remote_client_bridge --lib --locked --offline -j2 -- --test-threads=2`：core 215 项、bridge 21 项，共 236 项通过，0 失败。回执 `eebefed7-4afc-440f-9b43-460e33de4873`，日志 `receiver-tests-final.log`。
- `cargo clippy -p remote_client_bridge --features android --all-targets --locked --offline -j2 -- -D warnings`：退出码 0。回执 `d913b25b-0187-43f0-bc2f-6196baa73b66`，日志 `receiver-clippy.log`。包含宿主环境 JNI feature 检查，不等于 Android ABI 构建。
- 接收端回归覆盖未鉴权拒绝、未授权不提供画面、授权前后同一连接接收两个 131072 字节文件并逐字节一致、旧排队媒体不能跨授权继续发送。
- 基线发现 timing 单测在 mark_encode_done 之前比较尚为零的 encode_done_ts_us。已把顺序断言移至标记之后，保留对真实时间顺序的检查；未改动生产时间戳逻辑。
- 第一版连续文件测试夹具每次新建发送运行时，导致传输 ID 重复。已改为同一发送运行时，与实际会话模型一致；没有放宽生产侧重复清单保护。
- Android 编译、单测与 Lint 的最终结果在下方追加。此次 Rust 回归不能替代 Android 真机或跨设备验收。

## 尚未包含

定向投屏请求/接收确认、远端授权目录浏览与主动拉取、多个并发入站对端、Android 被控输入、Windows/Linux 全能力，以及公网弱网、耗电与长稳验收。本轮仍沿用入站单对端限制。没有发布新 APK，也没有提交或推送代码。使用 App 验收前需要把本轮 Rust/JNI 与 Kotlin 构建在同一包中，不能搭配旧 so。

## 后续命令

```sh
# Mac Studio 验证副本已具备匹配依赖缓存
cargo test -p remote_core -p remote_client_bridge --lib --locked --offline -j2 -- --test-threads=2
cargo clippy -p remote_client_bridge --features android --all-targets --locked --offline -j2 -- -D warnings
# MacBook 主仓库，按既有 Android 构建流程构建匹配 native 库后再打包
cd android
JAVA_HOME=/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home \
ANDROID_HOME=/Users/jinliang/Library/Android/sdk \
./gradlew :app:compileDebugKotlin :app:testDebugUnitTest :app:lintDebug --offline --max-workers=2
```

## 回写文件与测试版本

- `remote_client_bridge/src/publisher_bridge.rs`：`e28ddb2968facdf64d9b7cd98003b89c010134e24fc707f8b334cc677ac357b3`
- `remote_client_bridge/src/jni_bridge.rs`：`36bafa522ec754e20c61f1293d6dd40c39ad4af5eed33a5b077ae1d1fd4b9b3f`
- `android/app/src/main/java/com/remoteplay/client/FileReceiverClient.kt`：`5b0a37dfecd02fe1180e202c962ebe75678f39654c844120053b34a951fdbf63`
- `android/app/src/main/java/com/remoteplay/client/FileReceiverService.kt`：`03f58f9da642b50014f8bdcad2b0c76787c9548f8ce2dbe8e060652d9fef00f7`
- `android/app/src/main/AndroidManifest.xml`：`418992614b5cb962aae985706e616604a489b86cb1d83ac1663918bd099a2bd6`
- `android/app/src/main/java/com/remoteplay/client/MainActivity.kt`：`aad993e52e07a270f33c401dfaf4dcf66a9a2a891a97e6b23d0cfcf679ea565e`
- `android/app/src/main/java/com/remoteplay/client/ui/MobileConsoleScreen.kt`：`8a5088dc50db32bab95bfdde0835a8227a63eb51091efae31df5c89e04c34991`
- `remote_core/src/timing.rs`：`e7faf688c346e8ae38975694b8ef4ea44b547d80139e4147b52c8014f2d019ef`
- `android/app/src/main/java/com/remoteplay/client/ui/WorkspaceScreen.kt`：`122b44fd47bcfc3b2d0d1f3acf842e2de5604b0ad472c72fcf3d3a9ed3a0f6fa`

## Android 验证收尾

Mac Studio 未缓存本项目指定 Gradle 8.13，官方分发下载连接超时，未进入 Android 构建。MacBook 使用既有 Gradle/SDK 启动 compileDebugKotlin、testDebugUnitTest、lintDebug，约五分钟未得到有效构建输出，本轮已请求取消并收到 cancelled 状态。不能认定 Android Kotlin 编译、Lint 或 APK 打包通过；无 Android 真机验收。Rust 236 项与 JNI Clippy 结果不受此边界替代。
