# 四平台共用 GUI 与主动拉取验收

日期：2026-09-29。版本：2.0.0-alpha.4。

## 目标与架构

Linux、macOS、Windows 默认使用 app/src/desktop 中同一套 eframe/egui 界面，复用同一 Session、WorkspaceConnection、共享文件目录和数据通道；Android 保留 Compose/MediaCodec 的系统呈现与触控层，复用 Rust/JNI 网络、鉴权、会话和文件传输。平台区别只保留在图形窗口、解码、屏幕捕获、权限和输入适配处。macOS 旧 GPUI 仅为兼容入口，不作为新增跨平台功能唯一入口。

## 本轮完成的源码修复

- 桌面共享文件清单与主动取回采用独立请求 ID、请求类型匹配、在途去重、十秒超时及有界在途数量；迟到回包不覆盖新清单，队列未接收不进入虚假的忙状态。
- 翻页支持上一页/下一页；文件选择绑定具体连接实例与原网络，而不是仅绑定设备名称，重连后不会把旧选择自动发给新会话。
- 按键在失焦、切换会话、文件视图、弹窗与后台时释放；确认订阅/恢复后仍等待新的实际解码画面再允许输入。
- 系统字体按需读取以显示中文设备名/文件名；不将系统字体加入分发包。
- 默认 Windows GUI 是观看端，开启应用不启动 Windows 屏幕捕获或麦克风，不恢复自启动。

## 实际验证与未通过的项目

- HO5 新 GUI 使用正常 WorkspaceConnection 和文件传输通道，向鉴权回环文件节点主动取回合成 512 KiB 文件；接收端逐字节一致，SHA-256 为 61d1d9c5745bdaa4fab39240651bc242a5186b15393fd475082fcf6e84f400ab。真实 GUI 启动并生成测试回执，测试进程退出，原生产服务在测试期间未被替换。
- Windows 新 GUI 完成同样的文件测试，内容 SHA-256 一致；测试前后 GameViewerService 为 Running，服务 PID 不变，测试后 RemotePlay 为零进程。没有启动 Windows 捕获。
- HO5 到 MacBook 的实际画面连接已鉴权，实际采用 Relay，但 MacBook 返回系统 TCC 拒绝屏幕捕捉。decoded_frames=0、displayed_frame=false，该项未通过，不用旧 alpha.3 的 832 帧记录冒充本次通过。
- Android 正式签名包构建及签名/JNI/16 KB ELF 对齐验证通过。JVM 单测结果见构建 XML；实体手机的像素与触点一致性、多机长稳、Windows 捕获与 UU 共存仍未验收。

## 最终验证与部署结果

所有本轮构建与测试命令已退出；初次严格 Clippy 的 15 条问题已修复，最终 Clippy 通过，不使用 allow 来隐藏告警。

| 平台 | 目标模块单测 | 真实 GUI 文件验收 | 安装/分发 |
|---|---:|---|---|
| HO5/Linux | App 59、Client 21、Host 15、Bridge 32，共 127 | 512 KiB 拉取、逐字节一致，完成状态与落盘一致 | 已安装 2.0.0-alpha.4；用户服务 active/running，重启次数 0；原网络身份不变；启动入口与单实例唤回已验证 |
| macOS | App 75、Client 22、Bridge 32，共 129 | Mac Studio 同套 GUI 拉取 512 KiB，逐字节一致 | 已构建并发布；主机原安装未替换；本地签名，未 Apple 公证 |
| Windows | App 59、Client 21、Bridge 32，共 112 | 同套 GUI 拉取 512 KiB，SHA-256 一致，完成提示正确 | 已构建并发布；测试结束 RemotePlay 0 进程，自启动关闭；UU 始终 Running 且 PID 不变 |
| Android | JVM 30 项有效结果，任务命中缓存 | 实体手机交互未进行本轮验收 | Release/Lint/签名/JNI/16 KB 对齐通过；正式签名 ARM64 APK 已发布，versionCode=5 |

三个桌面平台的库测试与后续修改后的 App 定向复测均通过；测试数量含跨平台重复，不能相加当独立功能覆盖。没有宣称运行完整工作区的所有测试。合成文件通过真实 GUI 的同一工作流自动发起，并非人工鼠标点击全部控件；交互手感与所有边界仍需实测。

本轮最终代码同时修正完成提示：文件状态必须等接收验证与落盘后才显示完成，不停留在“请求受理”；录屏被主机拒绝后撤销输入/画面确认状态，不显示虚假的可控制状态。

HO5 安装位置：`/home/liang/.local/opt/remoteplay/2.0.0-alpha.4/remote_play`。
菜单/命令入口：`~/.local/bin/remoteplay`；桌面菜单名称 RemotePlay。
生产用户服务：`remote-play-current.service`；部署后 PID 593061，最终需以 systemd 实时查询为准。
回滚备份：`/var/home/liang/workspace/.rp-four-gui-20260929/deployment-backup`；alpha.3 二进制保留。
未更改原 mesh 身份、网关地址、UU、sing-box 或网络/显示驱动。

最终冻结源码 ID：`464b5b7c0279a4c8d2ae6bad12f6655bb598895feb4b9eadfb4e2e6388e43c5e`。
四端编译输入与对应冻结源逐文件校验；发布辅助脚本、说明文档在编译后补充，不影响编译产物。

## NAS 分发

下载入口：`https://relay.hackerlife.fun:8443/download/`。
版本化文件不可变，旧版本保留；本次全部文件均已通过公开下载字节 SHA-256 校验。
首次 Windows 包下载复核遇到网络超时，未重建或覆盖已上传内容；随后单独下载重试并核验成功。发布元数据及 Android latest 指向也已核验。

- `RemotePlay-Linux-x64-2.0.0-alpha.4.tar.gz`：8585783 bytes；SHA-256 `1cfe69db88ea2c750d5ae8e7938860a787028a770da1439186d1b49cd0befd88`。
- `RemotePlay-macos-arm64-2.0.0-alpha.4.zip`：11427242 bytes；SHA-256 `a96a4f442286909c6031fb0fc4f77aa628e5960b4085a4953594c5b4fefd35f9`。
- `RemotePlay-Windows-x64-2.0.0-alpha.4.zip`：44348145 bytes；SHA-256 `6c56429aa21dbd8fb958c6a9c3e17dc7a0725cccee674d02ce274c0ff7804d3c`。
- `RemotePlay-Android-2.0.0-alpha.4-f4c7212-release.apk`：15516304 bytes；SHA-256 `51962462fa4e92002483ea7f0c88f977218e284b8f39b4ff7be1d3c0a8c58d89`。

## 仍需明确的能力边界

- “有 GUI 并可主动发起拉取”不等于所有系统的被控/入站能力已完全对等。Windows 默认不开本机捕获和自启动；Android 被控输入依赖系统授权及现有适配。
- 当前 HO5 到 MacBook 的实际屏幕验证失败原因是 MacBook TCC 拒绝录屏，0 解码帧；必须由用户通过系统正常权限界面授权后重新验证，不能自动绕过权限。
- 只支持用户显式共享的文件目录清单，不是任意远端文件系统浏览器。
- Linux/Windows 当前使用私有或系统 FFmpeg 解码；本轮未完成硬件解码、长时间满负荷、所有网络切换的压力验收。
- Android 原生库与 APK 已匹配，但实体手机 SurfaceView/MediaCodec、横竖屏与触点一致性仍需验收。此前模拟器 ANR 不计作通过。
- 未自动提交、推送源码。本轮变更与此前用户/其他代理未提交内容一起保留在主工作区。
