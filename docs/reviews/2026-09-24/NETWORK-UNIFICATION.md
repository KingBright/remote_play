# 四机网络统一与遗留配置清理

日期：2026-09-24。目标：所有新版 RemotePlay 实例使用同一个设备网络，LAN/P2P/Relay 只是传输路径，不是不同的网络身份。

## 已完成

统一设备网络为 `remote-play-81a82899821abd22`，沿用既有成员的网络身份，没有创建第五套产品网络，也没有在设备之间复制 node_id。

|设备|当前设备 ID|结果|
|---|---|---|
|MacBook-M2-Max|62a3e1f90f70c4e10e6a6599298a993c|已从独立组迁入共用组，修正误写的 RemotePlay Android 名称，重启 launchd 服务并确认 running|
|Mac Studio|7878a72c723a4e2bd5a6898eeb2f0238|已从独立组迁入共用组，重启 launchd 服务并确认 running|
|HO5|6a32a19406b9e02dcbad058cd1cced30|保留既有共用组，完成上轮未收口的新版切换；systemd active，ExecStart 为 remote_play-3b59a408，NRestarts=0|
|cube|98c03a2f1b004431a324089ce8fce4e1|维护命令复核属于共用组，新版进程保持运行|

两台 Mac 的 launchd 配置显式指定 `REMOTE_PLAY_DEVICE_GROUP_DIR` 到 NativeMesh，LAN 发现、P2P、Relay 均启用，删除失效的 REMOTE_PLAY_MESH 环境变量，避免继承环境或旧目录再次导致分组分裂。

MacBook 和 Mac Studio 的旧 `Library/Application Support/RemotePlay/Mesh` 已移到各自工作区 `.remoteplay-network-clean-20260924/retired-Mesh`，该目录不在应用配置搜索路径中。历史配置备份一起移走。保留受限权限备份是为了回滚，不作为运行时备用网络。

cube 的 Startup/AetherLink.lnk 已移到 `.remoteplay-network-clean-20260924/AetherLink.lnk.retired`，复核原自启动入口不存在。检查时无运行中的 AetherLink/EasyTier 进程，也未发现相应 Windows 服务。

## 实际观察到的发现结果

MacBook 新版日志已出现 Mac Studio、HO5、cube 当前设备 ID 的 P2P 候选通告。
Mac Studio 新版日志也出现 MacBook、HO5、cube 当前设备 ID 的候选通告，并有两次 WebSocket relay tunnel connected 记录。
这证明了统一成员配置和控制面候选发现，不等于所有数据连接已验证成功，更不等于画面、控制、文件的完整验收。

Mac Studio 最近 800 行日志中 No route to host 计数为 0。不能据此认定此前的 LAN 广播问题已经永久修复。本轮没有修改广播实现，也没有声称 LAN、P2P、Relay 三条路径逐一业务验收通过。

## 仍未清理完成

1. Mac Studio 的旧 root Mesh 守护进程仍在运行，并会重新拉起旧 app 包内的 EasyTier；只移动配置目录不能终止这个 root 守护进程。应先卸载旧 com.remoteplay.mesh 系统任务，再终止旧 test app 守护进程和子进程，最后归档对应旧程序和配置。
2. HO5 的旧 system 级 remote-play.service / easytier-remoteplay.service 仍需 disable --now 并移出 systemd 配置路径。最新版 user 级 remote-play-current.service 不应被停用。
3. 含管理员认证的清理请求被平台安全检查拦截，未执行。之后 HO5 再次变为 device_offline，系统服务清理没有完成。
4. Mac Studio 另有独立 `/Users/jinliang/workspace/vpn` 项目的 EasyTier，网络名 easytier_vpn_254956d9，虚拟地址 10.251.89.2；这不是 RemotePlay 的旧同名设备组，本轮没有擅自停掉。sing-box/sing-tun、UU Remote、Remote Hosts 均未清理或替换。

## 续接注意

macOS 旧守护进程使用 `/Users/jinliang/remote_play_test/RemotePlay Unified.app/Contents/MacOS/remote_play`；其子进程来自同 app 包的 `Contents/Resources/bin/easytier-core`。必须按完整可执行路径核对，不能仅凭历史 PID，也不能宽泛 pkill easytier，以免误停独立 VPN。

HO5 的最新可执行文件为 `/home/liang/.local/bin/remote_play-3b59a408`。清理旧 system 服务时须保留 user 服务及其 `/home/liang/.config/remote-play-current/mesh`；旧配置只归档已确认属于旧 RemotePlay 的目录，不删除接收文件或其他代理配置。

本轮共享组密钥没有回显到报告。当前原生组校验摘要前三台一致；cube 同组配置由维护命令与历史配对信息复核，完整鉴权探测未完成。

## 验收工具边界

独立原生鉴权探测使用临时节点、不写入已安装身份；直接运行请求被平台拦截。后续仅编译检查没有完成，任务已停止，源码移至 Mac Studio 工作区 .remoteplay-network-clean-20260924/network_acceptance.unverified.rs，不留在产品构建目标中。本轮不将该探测记为通过，也不把候选通告当作数据通道握手成功。
