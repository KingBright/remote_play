# 四台电脑部署记录

日期：2026-09-24。部署对象是 RemotePlay，不是 Remote Hosts。当前结果：3 台完成，HO5 尚未完成最终切换。

## 构建来源

产品版本 2.0.0-alpha.1；部署构建标识 20260924.1。
基线提交 f4c7212，加上此前三轮入网、独立接收和共享文件主动拉取的工作区改动。
冻结源码标识：3b59a408edfb10d50f771aab5b5cb8e575871ca921d55ccc12eb775c985d3f74。
源包 SHA-256：8031183aa01e5d739f3996b6341bde11504a30a85d4d149f1d58ad3bc97e776c。
快照包含 220 个文件。macOS 构建副本的 115 个 Rust/Cargo 构建输入与主仓库逐文件匹配；Windows/Linux 对源包全部文件进行了 SHA-256 校验。
三个平台均执行 cargo build --release -p remote_play_app --bin remote_play --locked，退出码 0。Windows/Linux 补齐了锁定版本依赖；HO5 复用 /home/liang/.local/lib/pkgconfig/alsa.pc，未改动系统包。

## 设备结果

|设备|结果|安装位置|已核对证据|
|---|---|---|---|
|MacBook-M2-Max|已切换并运行|/Users/jinliang/Applications/RemotePlay.app|launchd com.remoteplay.host，PID 79981；原 RemotePlay Local 签名保留；签名校验通过；入网文件哈希未变化|
|Mac-Studio|已切换并运行|/Users/jinliang/Applications/RemotePlay.app|launchd com.remoteplay.host，PID 59522；签名校验通过；入网文件哈希未变化|
|cube|已切换并运行|C:\Users\Liang\AppData\Local\RemotePlay\remote_play.exe|PID 5388，后续独立查询仍存活；安装文件与 Release 产物 SHA-256 相同；原 HKCU Run RemotePlayHost 保留；日志包含中继连接和已知设备 P2P 候选|
|HO5|新程序已准备，最终切换未完成|/home/liang/.local/bin/remote_play-3b59a408|Release 构建成功；两次启动返回 systemd active，但进程文件检查受到 procfs 权限限制，验证脚本触发回退；改用 systemd ExecStart 验证的最终请求在执行前返回 device_offline|

MacBook 的系统包版本字段为 2.0.0，CFBundleVersion 为 20260924.1，RemotePlayReleaseVersion 为 2.0.0-alpha.1，不代表稳定版。
macOS 只替换用户 Applications 下 com.remoteplay.unified 产品，没有修改 /Applications 中的 PlayStation RemotePlay 或 UU Remote。

## 已安装二进制 SHA-256

- MacBook：d058770d1ca1e94477e878dd391266baddcbaaf678bc883c817bbf8622dba599。
- Mac Studio：00cda2a0400e504c2c6764be53af97254f4e6d9d4198475325c0538a42560bea。
- cube：0a4e1a8081ae74634ac34e802722c8cc456869c1ae93ee4a6ef9131f7c4cf1c9。
- 两台 Mac 使用同一构建产物，签名不同导致最终文件哈希不同。

## 入网与验收边界

- 保留既有身份，没有在部署中把设备强制切换到另一个网络。
- MacBook 当前网络名 remote-play-8b22e7c606e4e947；其余三台此前确认的网络名 remote-play-81a82899821abd22。MacBook 与三台电脑的网络成员关系尚未统一。
- 两台 Mac 的启动日志有 255.255.255.255:38117 广播 No route to host (os error 65) 告警。进程启动成功不等于 LAN 发现已通过。
- cube 日志证明已连接中继并收到 Mac Studio、HO5 的候选通告，不等于已经完成媒体、控制和文件的全部业务验收。
- 本次没有完成四机跨设备文件传输、画面和输入控制验收。独立业务探测工具调用被平台安全检查拦截，未执行。
- 检查还看到了 Mac Studio 与 HO5 遗留的旧 Mesh/EasyTier 进程或服务；新版不包含它们，本次没有完成对旧系统级服务的清理。
- Android APK 未在本轮构建或发布。Windows/Linux 的现有平台能力边界没有因安装新版而自动补齐。

## 回滚与续接

MacBook 备份：/Users/jinliang/Workspace/.remoteplay-deploy-20260924/RemotePlay.before.app。
Mac Studio 备份：/Users/jinliang/workspace/.remoteplay-deploy-20260924/RemotePlay.before.app。
cube 备份：C:\Users\Liang\Workspace\.remoteplay-deploy-20260924\backup。
HO5 原服务文件备份：/var/home/liang/workspace/.remoteplay-deploy-20260924/remote-play-current.service.before。

HO5 恢复在线后，先核对当前服务文件与备份以及候选程序哈希，再将 remote-play-current.service 的 ExecStart 切到 /home/liang/.local/bin/remote_play-3b59a408，daemon-reload 后 restart。使用 systemd ActiveState、MainPID、ExecStart、NRestarts 和启动日志验收，不应把 procfs 路径检查受限误判为应用启动失败。
最后观察 HO5 为 offline；bazzite.local 的既有 SSH 检查无法解析目标，没有通过其他路径完成切换。

源码未提交或推送；没有改动 Remote Hosts 服务。各已完成设备的本次部署回执保存在其工作区 .remoteplay-deploy-20260924/deployment-receipt.json。

## 后续网络统一更新

本轮之后，HO5 已成功切换到 remote_play-3b59a408；两台 Mac 的新版实例已迁入 remote-play-81a82899821abd22 并保留本机 ID。旧配置目录和 Windows AetherLink 自启动已退役，root 级遗留进程及 HO5 旧系统服务仍待清理。最新事实与边界见同目录 NETWORK-UNIFICATION.md。
