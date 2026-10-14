# 设备互联与主动推拉：第一轮修复记录

日期：2026-09-24。基线：main / f4c7212。此文档记录代码修改与验证边界，不是全产品验收报告。

## 产品目标

加入同一设备网络后自动互相发现，按实际能力从任意一端发起观看、控制、投送和文件交换。网络成员、媒体来源、接收方和会话发起方不是固定角色。网络、身份、会话与媒体订阅分别管理。继续沿用自研 LAN → P2P → Relay，不引入第三方 VPN。

## 本轮已修改

1. core、桌面与 Android 共用 prepare_join：入网保留本机节点 ID 与名称；重复加入同一组不写配置、不刷新运行时；无效邀请保留原配置；同名网络密钥轮换仍算实际变更。
2. Android 的 Rust bridge 不再过滤非视频成员。真实传播 can_view、can_stream、file_transfer、clipboard_sync 和是否存在入站服务，不把“可发现”当作“可接受连接”。仍排除已移除方案的旧 Mesh 路由。
3. Android 扫码与邀请码共用异步入网流程，防止重复操作和 UI 线程阻塞。活动会话/共享期间禁止直接换组，提示先结束当前操作。显示实际网络名，入网提示不再被共享状态覆盖。
4. App 入口围绕网络成员组织：扫码/邀请码置顶，手填地址放高级选项。按服务与能力开放查看和发送文件入口。不要求开启视频才能进行文件会话。
5. 发文件入口在本次会话确认支持文件后才打开系统选择器；文件发送检查目的会话 token，切换设备或断开后拒绝旧选择结果。
6. 工作区保存设备 ID，在建立或重新建立连接时通过发现结果解析最新地址；命名设备缺失时显式失败，不盲目使用旧 P2P/Relay 临时地址。高级手动地址仍支持。这不是无缝链路切换的完整实现。

## 验证

- rustfmt 已作用于本轮四个 Rust 文件，git diff --check 通过。
- 8 项 Kotlin 独立行为检查通过，使用当前工作容器的 kotlinc/JVM，非 Android 设备执行。测试源码与 MacBook 仓库 SHA-256 核对一致。
- NetworkDevice.kt SHA-256：8507d0205438db1eb844e9730bfcf7cf37d05f8fc5926f372962b74171d8c185。
- HostDeviceActionsTest.kt SHA-256：c1d759d9e9d0d209359f318580769725ef4a6f2c67e838eccb99d189fa154f70。
- 八项覆盖：仅观看成员、纯文件节点、仅屏幕节点、缺少入站服务、离线节点、按设备 ID 使用新地址、拒绝缺失设备的旧地址、显式手动连接。
- 新增 7 项 Rust 回归测试：入网身份保持、重复入网、无效邀请、密钥轮换、首次入网、非视频成员、旧 Mesh 排除等。尚未取得执行通过证据。
- workspace Rust 单测与 Android compileDebugKotlin/testDebugUnitTest/lintDebug 已尝试启动，但进程运行数分钟后日志仍为空，已取消本轮两项任务，避免继续占用资源。观察到 MacBook 平均负载约 44，不能据此断言阻塞根因。
- Rust/JNI 编译检查最终通过：cargo check -p remote_client_bridge --features android --locked --offline -j 2，退出码 0。首次检查发现的 MeshConfig 导入缺失已经恢复。此检查在 macOS 上进行，不等于 Android ABI 构建。
- 收窄后的 mesh::tests 在编译 remote_core 阶段中止，退出码 130，尚未执行用例。Android 离线、2-worker 检查也未取得完整结果，已取消。没有 Android 真机或模拟器通过 adb 连接。
- 尚未验证完整 Android 编译、JNI 打包、真机界面与跨设备媒体/文件闭环；不得把独立 Kotlin 检查写成整包验收。

## 仍缺少的关键能力

- 定向推送屏幕/窗口到指定接收端，并在接收端完成邀请确认与展示。现有“使手机画面可供查看”不是定向推送。
- 主动浏览获授权的远端文件位置并拉取文件。现有发送文件/已收文件不是远端文件浏览器。
- Android 没有启动屏幕共享时，独立接受文件或投送请求的常驻服务，以及合理的后台/电量策略。
- Android 被控输入服务；Windows/Linux 完整观看界面、应用采集与音频能力。
- 真实多端、跨网络、切换网络、断线恢复、休眠恢复、并发媒体与文件的验收；能力广播不等于已完成互通验证。

## 发布边界

本轮修改留在 MacBook 的 main 工作区，未提交、未推送、未发布 APK，未替换 NAS 落地页。原有 deploy/nas/download/index.html 修改保留且未处理。
新增 JNI 方法必须配套重新构建 native 库，再构建与验证 APK；不得只换 Kotlin 代码搭配旧 so 发布。

## 接续验证命令

```sh
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/tmp/remote-play-review-target \
  cargo test -p remote_core --lib --locked mesh::tests
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/tmp/remote-play-review-target \
  cargo test -p remote_client_bridge --lib --locked
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/tmp/remote-play-review-target \
  cargo test --manifest-path client/Cargo.toml --lib --locked mesh_pairing::tests
CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/tmp/remote-play-android-target \
  ./scripts/build_android_native.sh
cd android
JAVA_HOME=/opt/homebrew/opt/openjdk@17/libexec/openjdk.jdk/Contents/Home \
ANDROID_HOME=/Users/jinliang/Library/Android/sdk \
  ./gradlew :app:testDebugUnitTest :app:lintDebug :app:assembleDebug --console=plain --max-workers=2
```

构建结果只能证明相应代码检查，不替代真机交互验收。
