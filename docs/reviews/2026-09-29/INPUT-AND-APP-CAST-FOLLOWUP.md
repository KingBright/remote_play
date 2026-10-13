# 输入与应用窗口投屏：现场续接验收

## 本次确认的部署差异与处理

MacBook 和 Mac Studio 实际安装版本均已是 2.0.0-alpha.5；HO5 普通用户服务和 ~/.local/bin/remoteplay 仍指向 alpha.4。因此之前已经实现的静止画面输入修复与新 Relay 设备隔离，并没有进入用户在 HO5 正在使用的进程。

本次已成功切换 HO5 用户服务与启动入口到 ~/.local/opt/remoteplay/2.0.0-alpha.5/remote_play。候选 SHA-256 b5b72bbb311ef180ac26f5a945fb8df91da6cccda6fb15864202a7a0a99b91a2，与此前已验证构建一致。ldd 未发现缺失依赖。激活后 MainPID=996056、ActiveState=active、SubState=running、NRestarts=0，第二次观察 PID 稳定。只核对持久 mesh.conf / mesh.secret 保持不变，不把运行时 desktop-instance.addr 当作身份文件。没有修改权限、路由或其他产品服务。

回滚备份与部署回执：HO5 /home/liang/.local/state/remoteplay/deployments/alpha5-20260929-193132。

## 本轮重新执行的真实验证

1. 在 Mac Studio 上创建明确命名的合成输入测试窗口，通过已鉴权的实际运行 alpha.5 主机完成主屏订阅；只在该窗口确认前台和焦点后测试一组左键按下/抬起，并只在确认点击被它收到后发送单个字母 a。实测主屏解码 40 帧，窗口确实收到点击和按键。随后用 GUI 相同的 SwitchSource 协议切到该窗口，回执成功并解码 16 帧，解码错误为 0。此项使用主机本机回环接收程序，证明主机采集与注入有效，不当作 HO5 普通 GUI 人工操作已验收。
2. 在两台 Mac 创建各自独立的红/蓝合成应用窗口，从 HO5 使用当前 alpha.5 共享核心通过真实 Relay 同时订阅。两个目标具有独立端口、连接编号，业务订阅 ID 同为 1。连续五轮像素校验均对应正确设备，末轮 Studio 解码计数 30、MacBook 22。此测试只捕捉指定合成窗口，不注入输入。HO5 普通 alpha.5 服务在测试期间保持运行。
3. 两台 Mac 的隔离窗口按限时关闭，输入测试窗口也已关闭。没有留下捕获测试进程。未启动 Windows 捕获、未改变 UU 或系统授权。

本次没有新改动输入/Relay 业务源码，也没有生成新版本：修复已包含在此前发布的 alpha.5，本次补齐遗漏的 HO5 激活和实测。

## 使用与能力边界

- 主屏查看默认仍锁定输入。用户选择 Main display，点击 Enable control，再点击远端画面以获取键盘焦点。Ctrl+Alt+Esc 可锁定控制。
- 不再使用“两秒没有新帧”作为禁止输入依据。已确认来源的静止画面保持可控，是否活跃由鉴权连接心跳判断。暂停、失焦、失联、切源未确认与旧来源帧仍保持保护。
- 标注 view only 的独立应用窗口仅支持观看。窗口级输入目标和坐标映射没有实现，不能为了让按钮看似可用而将窗口输入发到主屏。需要操作某个应用时，通过 Main display 操作该应用。
- 用户所说 app 投屏的具体方向尚未明确。本轮核验的是桌面来源列表中的应用窗口。Android 手机整屏反向投送是另一条路径，未以本轮桌面窗口成功替代其验收。

## 本轮证据路径

Mac Studio：/Users/jinliang/workspace/.rp-input-followup-20260929/installed-alpha5-input-window.json。
HO5：/var/home/liang/workspace/.rp-input-followup-20260929/dual-real-window-alpha5.json。
HO5 部署前后记录见上面的部署回执目录。未提交或推送源码。
