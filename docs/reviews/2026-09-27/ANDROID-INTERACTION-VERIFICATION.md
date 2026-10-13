# Android 视口交互续接记录

日期：2026-09-27。主仓库：`/Users/jinliang/remote_play`。

## 本轮范围

没有部署正式服务、发布 APK、提交或推送代码。Windows RemotePlay 保持停止，自启动关闭；没有改动 UU、现有私网或显示驱动。设备级验证使用本轮启动的只读临时 Android API 35 模拟器，不连接真实电脑，不访问麦克风，不调用 RemotePlay JNI 会话。

## 已落盘修复

1. 暂停、非 Streaming 状态或设置对话框打开时，视口不再把新的触摸发送给远端。暂停图像仍可本地捏合；取消已有手势时发送 Cancel，不让迟到的 Up 变成点击。未记录 Down 的 Move / Up 不再发送。
2. 视口改变尺寸时，按新旧宽高比例换算平移偏移，再限制边界，保持画面中心对应的归一化内容位置。此前只 clamp 原像素偏移，放大后旋转或改变宽高比会改变阅读位置。
3. 提取生产使用的 `RemoteViewportGestures` 与 `RemoteVideoSurface`，让设备测试复用实际事件分发、Compose 变换和 SurfaceView，而不是另写一份模拟算法。保留视频尺寸、视口尺寸、触控模式与可输入状态变化的取消语义。
4. 增加 14 项 Android instrumentation 用例，覆盖最终合成像素与触点一致性、裁剪、缩放尾部、暂停、拖拽取消、复位、双击、三指、黑边、触控板灵敏度，以及尺寸/内容变化。测试绘制本地网格与标记，不读取任何真实远程画面。

## 验证证据与限制

第一版修复的应用、测试 APK 和 JVM 单测构建已成功；JVM XML 记录 26 项通过：VideoDimensions 4、BackgroundConnectionPolicy 1、HostDeviceActions 8、ViewportTransform 13。随后补齐 contentKey 取消语义的最终构建结果将在本记录末尾补充，不能用前一轮结果替代。

设备级执行没有取得通过结果：软件渲染模式的首轮测试在 300 秒整体期限内未完成；硬件渲染模式返回 `INSTRUMENTATION_RESULT: shortMsg=Process crashed.`。系统日志明确记录测试应用在启动阶段触发 ANR：`failed to complete startup`，随后被 ActivityManager 终止。同期虚拟设备的蓝牙和 Google 服务也有崩溃记录，但这不足以证明所有故障与产品代码无关。

后续拟对测试包预编译后重试的调用被平台安全检查拦截，未执行。没有修改系统 ANR 判定或放宽交互断言。编译成功不等于 instrumentation 通过；没有获得可以用于声明画面与触点一致性的最终截图证据。

前期构建还遇到旧 Java 构建进程持有缓存锁；只结束了本轮启动的构建进程，改用机器已有 HotSpot JDK 17 的进程级 JAVA_HOME，没有修改系统 Java 配置。一次 Gradle connectedAndroidTest 因离线缺少 UTP 依赖失败；随后使用已编译 APK 和 AndroidJUnitRunner 直接执行，执行结果仍按上述失败记录保留。

## 留存

日志、修改前副本、SHA-256 和测试包位于 `Workspace/.rp-p1-interaction-20260927`。源码中只新增测试所需依赖和 test runner；正式版本未增加测试入口。Rust/主机切源协议本轮未继续修改。真实手机、MediaCodec/JNI/网络整链路、桌面切源首帧与 Windows/UU 共存均未由本轮验收证明。

## 最终构建回执

最终 contentKey 取消语义补齐后，构建操作 `fc3cd712-56a6-467b-a9e3-f0e51a5f89b8` 正常退出，退出码 0。`assembleDebug`、`assembleDebugAndroidTest` 和 `testDebugUnitTest` 均完成；最新 JVM XML 合计 **26 项、0 失败、0 错误**。14 项设备交互用例已编译进测试 APK，但并未取得设备执行通过结果。

已再次核对 7 个本轮源码/测试/构建文件的 SHA-256 与记录一致，`git diff --check` 通过。详细回执保存于 `Workspace/.rp-p1-interaction-20260927/final-verification-receipt.json`。先前“最终构建待补充”的状态由此节更新，不改变设备级验收未通过的结论。

本轮两次临时模拟器进程均已结束；没有继续在后台运行设备测试。最终调试 APK 仅是测试产物，未作为正式签名发布包分发，未替换真实手机或四台电脑上的安装版本。未提交、推送或部署。
