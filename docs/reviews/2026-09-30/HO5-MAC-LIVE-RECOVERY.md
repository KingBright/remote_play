# HO5 与 Mac 拉流/输入故障：实际运行恢复与待部署修复

## 不再混淆三类证据

本轮开始时 HO5 没有运行中的 RemotePlay，用户服务 inactive；不据此归责用户。两台 Mac 运行原签名 alpha.6 / 20260930.6.10 后台程序。随后使用 HO5 已安装的真实桌面 GUI，正常发现指定设备并拉流：Studio 45 秒 763 帧且实际上传到 GUI 纹理，后续仅增加状态输出的诊断版分别从 MacBook / Studio 显示 651 / 514 帧。两端均授权成功、来源支持输入、连接心跳正常；由于测试没有点击启用控制，用户锁保持开启。不能用这类只读拉流证明真实键鼠全部工作。

## 已复现并恢复的部署问题

Mac launchagent 原来以 REMOTE_PLAY_HEADLESS=1 运行，而 GUI 的单实例锁没有覆盖无界面路径。打开 Mac Studio 应用会另起网络运行时，与现有进程冲突，实际日志包含 Address already in use。Relay 同组同设备身份的注册又会替换前一条连接，因而两套实例存在互相踢下线的风险。现有泛化 WebSocket reset 日志不能逐条认定全由这一个原因导致。

本轮已将两台 Mac 原 launchagent 的 HEADLESS 改为0，使用现有正确签名应用，让单一 GUI 进程同时承载后台网络。原配置0600备份在本轮 staging 目录。主机端口和 GUI IPC 均验证；再次正常启动应用复用原PID，不再另起一套。网络 mesh.conf / mesh.secret 哈希未变，没有权限修改、TCC重置、UU或其他服务调整。

## 已复现并修复源码的输入问题

旧 GUI 仅在 response.clicked()（松开时）申请键盘焦点。鼠标按下的下一帧，失焦清理就提前发送 ReleaseInput；真实松开事件因本地按下记录已清除而不再发送。这破坏鼠标点击、拖拽和鼠标/键盘焦点衔接，先前直接对主机发送输入的验收无法覆盖。

新增 app/src/desktop/input_path_tests.rs 使用生产 Desktop::screen 和实际 WorkspaceConnection，仅连接本机录制协议事件的模拟服务，不调用任何系统输入注入：修复前失败，记录 Down → ReleaseInput、缺失Up；改成 is_pointer_button_down_on() 时即获取焦点后，通过 Down → Up → KeyDown → KeyUp 完整序列且无提前释放。该测试在主仓库默认ignore，需单独执行且不继承真实用户网络密钥，避免全套测试共享全局状态干扰。

## 待部署的 alpha.7

- 鼠标按下即申请焦点。
- View/Control 不复用失联或视频失败的旧连接；增加 Reconnect selected device 入口，重新按具体设备ID取路由，新会话默认保持输入锁定。
- headless 与 GUI 共用同一设备profile锁。新 headless 启动不会创建第二网络实例；GUI发现既有headless会明确提示迁移现有服务，不静默争用网络。
- 诊断回执区分：连接、图像是否上传、输入锁、可控制能力、暂停、心跳、当前来源帧与解码失败。

主仓库 app/VERSION 为2.0.0-alpha.7；协议与库保持alpha.6，没有更改线上协议。最终7文件delta SHA-256：77437a0f9abf028cede21ef60bcc87353839480e18bd2c304363213671518009。最终同步HO5并进行全量相关测试/Release构建的工具调用被平台安全检查拒绝，没有执行，未用其他形式重放。因此新实例互斥/重连测试及最终alpha7构建尚无通过回执，不可将已有的一项鼠标回归替代最终构建验收。

## 当前安装与恢复状态

两台Mac依然安装alpha.6的正确签名包，但已切为单一GUI+主机运行。HO5恢复启动现有alpha.6，PID1880955、active/running、NRestarts=0，未自动建立远控会话、未注入输入，网络身份保持。新鼠标修复尚未部署到HO5。NAS索引未更新，源码未提交或推送。

## 证据路径与后续边界

HO5：/var/home/liang/workspace/.rp-recovery-20260930/ 下的 installed-gui-studio-before.json、gui-input-gates-*.json、gui-input-test-final.log（红）、gui-input-fixed-test.log（绿）、existing-gui-restored.json。
MacBook：/Users/jinliang/Workspace/.rp-recovery-20260930/recovery-progress.json 与 recovery-source-final.json。
Mac Studio：/Users/jinliang/workspace/.rp-recovery-20260930/studio-gui-launch-before.log 和 studio-gui-service-recovery.json。

下一步是在本机正常开发环境对最终alpha7源完成回归/打包/部署及真实HO5跨网络键鼠验收。macOS必须沿用已固定签名和验证升级流程，不修改旧版已发布ZIP或授权。当前不能声称所有使用时序的鼠标键盘和重连均已恢复。

## 最后补验：Mac Studio 作为主动观看端

在不修改签名/授权且避免双进程的情况下，用当前安装的 Mac Studio GUI 正常连接 MacBook，35秒内实际解码546帧，已显示1670×1080图像，GUI视频error为空。测试未注入输入，未保存屏幕截图。结束后恢复原来的统一GUI用户服务，PID76573，配置保持HEADLESS=0。

该过程仍记录 WebSocket reset-without-close 警告，因此只确认此时普通GUI拉流可用，不宣称所有Relay异常已经消失或已通过长稳验证。
