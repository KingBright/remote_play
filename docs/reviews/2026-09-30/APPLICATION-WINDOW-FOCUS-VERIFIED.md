# 单窗口键盘焦点：真实验收已通过

## 本轮根因修复

同进程的系统标题栏附属表面会使用 Window 标题、66×20 尺寸和父窗口左上角 (6,6) 偏移。旧判断只排除无标题附属表面，误拦截了真正已经获得焦点的窗口。本轮仅识别这组严格组合条件，保留同 PID、layer、父窗口范围和实际鼠标命中检查；不是按标题 Window 单独忽略窗口，也未删除权限或遮挡保护。

## 真机结果

实际签名运行的 Mac Studio alpha.6 / 20260930.6.10，通过鉴权回环订阅两个同进程、同标题合成窗口，不订阅桌面。A/B 分别解码 57/77 帧，A 正确收到两次有效点击及字符 a，B 仅收到针对它的一次点击及字符 b。移动缩放后的映射正确；几何正向测试明确把目标放回前台，不假称后台控制。失焦、旧来源版本、无版本旧输入、暂停和隐藏后的操作均未误投到另一窗口。隐藏测试使用 orderOut，并非所有应用的窗口销毁测试。

验收 JSON 的 passed=true 来源于实际窗口计数、坐标、字符和帧数；所有合成测试进程均已关闭。不能将此回环测试说成 HO5 普通 GUI 跨网络长稳测试。

## 部署状态

Mac Studio 与 MacBook 均已通过固定原签名身份的升级器运行 alpha.6 / 20260930.6.10，原网络和启动配置未变。Mac 包 SHA-256：52a2d01ecf24e2c8c75745606ad3fd7fb1681830e91544b4338ad143d420aa57。

HO5 已安装配套 alpha.6 GUI，用户启动入口与 service ExecStart 均已更新。因用户此前退出了程序，本轮保留 inactive 状态。打开应用菜单的 RemotePlay 或运行 ~/.local/bin/remoteplay 即使用新版。候选二进制 SHA-256：f540624bf7820b36d92e66afc02b7c189d122dc223360e57e0a504ad404e5dec。旧配置备份在 ~/.local/state/remoteplay/deployments/focus-alpha6-20260930-161636。

来源列表中选择 Application windows 下的实际窗口，Enable control 后点击远端画面再输入。现在是前台窗口控制，会使用主机实际键鼠焦点，不是任意隐藏窗口的独立后台桌面。

## 自动化与交付边界

功能构建 Host 42、Protocol 33、App 82 项库测试通过，共157项。随后源码移除一次不必要的字符串分配，13项焦点回归及 Host/App 库目标严格 Clippy 通过；这一等价比较优化未覆盖安装后的不可变6.10包，随下一构建发布。

本轮没有更新 NAS 正式下载索引，也没有宣称 Windows/Android 获得新增真机验收。源码未提交或推送；用户不需要重置权限或重新入网。

## 持久化记录

MacBook /Users/jinliang/Workspace/.rp-focus-resume-20260930/progress.json 保存当前完成状态。real-acceptance.json 是实际测试回执，经网关传输并校验 SHA-256 18cfdef7de08f777a6f5ce74dd34934751ff42903f4b7ca4cd57fc0471d09e16。原文件备份在 before/。Mac Studio 同名目录保存构建、测试、Clippy 和安装日志。
