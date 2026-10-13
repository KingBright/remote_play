# alpha.7 四电脑部署与 Android 发布回执

## 交付状态

- HO5：实际安装 ~/.local/opt/remoteplay/2.0.0-alpha.7/remote_play，原用户服务与 ~/.local/bin/remoteplay 已更新；最终 active/running，PID 1899036，NRestarts=0。真实 GUI 拉流验收结束后恢复正常用户服务。
- MacBook 与 Mac Studio：实际安装 ~/Applications/RemotePlay.app，版本 2.0.0-alpha.7 / build 20260930.7，两机均使用原固定授权证书；最终 GUI+主机各一进程，PID分别96760、78134。再次发起headless启动确认退出且不另建网络实例。
- Windows cube：替换 %LOCALAPPDATA%/RemotePlay/remote_play.exe，桌面和开始菜单 RemotePlay 入口更新。GUI 真启动报告 alpha.7，能发现另外三台电脑；测试结束后进程0，本机捕获与自启动仍关闭，UU Running/PID27892未变化。原用户 Mesh 身份保留。
- Android：正式签名 alpha.7，versionCode=8；签名延续、25项JNI导出、包内本次原生库摘要和16KB对齐校验通过。没有实体手机连接ADB，也没有Android Remote Hosts设备，因此未将APK发布说成手机已安装。模拟器不是用户手机，本轮没有替它安装。

## 此次真正部署的修复

鼠标按下即取得焦点，防止失焦保护过早发送ReleaseInput并吞掉Up；两个启动模式共用profile锁，避免GUI与后台重复入网/端口冲突；失败或失联的连接不再复用，提供Reconnect selected device。各桌面使用同一份编译源码；协议与既有库版本未为发行号调整而另改线上协议。

## 验证

Linux：相关库120项通过，另单独执行1项真实GUI事件到回环协议记录器的鼠标/键盘回归，总121。macOS：相关库160项，独立GUI输入回归1项，总161；App/Host lib+bin严格Clippy通过。Windows：相关库135项，独立GUI输入回归1项，总136。默认ignore的本地协议测试已在独立进程中显式执行，不继承真实用户会话密钥。该测试不向系统注入输入。

部署后从HO5实际安装的alpha.7 GUI分别连接Mac Studio和MacBook，每次30秒，实际显示画面并分别解码449/505帧；来源支持输入、心跳正常、视频错误为空。两次测试没有主动点击Enable control，没有向用户应用发送键鼠，没有保存截图，不将其描述为完整跨网络人工键鼠验收。Windows实际GUI启动完成版本与发现验证，不启动本机屏幕采集。

Android Release和Lint通过，30项JVM测试结果来自有效缓存复用。打包/签名/安装防护45项测试通过，包括本次新增的运行时单实例信息与持久身份分离检查。

## 部署过程中修复的校验问题

HO5第一次切换后尝试读取/proc/PID/exe被OS限制，升级逻辑已回滚，没有提权或修改安全设置。后续以已校验安装文件、systemd ExecStart、稳定PID和产品GUI版本/画面回执验证并成功部署。

Mac升级校验旧实现把desktop-instance.addr和desktop-instance.lock当持久配置，正常重启会使其变化。本轮仅排除这两个明确的进程运行时文件，仍比较其他所有配置与密钥，新增三项测试确认新端口变化允许、密钥及未知持久配置变化仍检出。沿用原签名约束，无TCC重置。该修改属于发布工具，不改变已经冻结编译的业务源码。

Android构建完成后外围zsh命令误用只读status变量导致外层非零退出；构建日志本身BUILD SUCCESSFUL，之后独立APK验证命令已成功退出，未重复构建或伪称原外层命令成功。

## 发布及追溯

NAS统一下载：https://relay.hackerlife.fun:8443/download/
Android覆盖升级：https://relay.hackerlife.fun:8443/download/RemotePlay-Android-latest.apk
使用说明：acceptance-2.0.0-alpha.7.html。
三个桌面版本化包已逐个公开下载并核对SHA-256；Android版本化包及latest链接也验证。发布索引均指向alpha.7，旧版本文件未覆盖。

编译快照source_id：112a3864306b3a90d28033447103cd0e509b6d8c6bd64043505af1511e2adcdd。最终回执、签名报告、各发布日志与工具修订摘要位于 /Users/jinliang/Workspace/.rp-alpha7-deploy-20260930。各设备旧安装/启动文件已保留回滚副本。源码没有自动提交或推送。

## 剩余验收边界

本轮是实际部署，不是对所有OS方向、所有应用、所有网络以及长稳的全面保证。Android实体机覆盖安装仍需用户在手机确认或连接授权部署通道。Windows未做并发UU捕获验收；应用窗口主机适配保持此前实现的macOS范围。不要据此让用户重新入网或重置权限。
