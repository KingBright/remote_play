# 恢复版 GUI：视频恢复修复与跨平台回归

## 本轮交付边界

本轮继续执行已经确认的原 GPUI 恢复方案，没有改框架、降低画质、删功能、改变默认界面或替换运行中的 alpha.7。完成的是共享媒体恢复、失效画面输入保护及 Windows P2P 缺陷修复。完整恢复版尚未达到发布条件。当前 Linux/Windows 的原生视频呈现仍未完成，原功能清单和性能/长稳还没有全面验收。

## 已修正的具体缺陷

1. 原接收端只根据自己的包缺口和解码错误请求关键帧。但解码队列满丢弃参考帧后，解码器会等待关键帧并跳过依赖帧，这不一定产生 codec error；接收端还可能因看见一个 IDR 就错误认为已恢复。现在解码器有明确恢复信号，直到实际有效输出才解除，继续使用原有 250 ms 限频和正确会话/来源目标，不扩大队列、不无限重试。
2. Mac 的 reset_video 增加 epoch 后，解码循环此前没有在每次输入时落实会话重建。现实现检测 epoch、重建旧解码状态并等待关键帧；输出发布与 epoch 修改共享同一锁，避免一次检查后旧回调又发布。旧帧对象在锁外释放。其他平台已有的 pipe reset 路径也接入相同恢复状态。没有新增逐帧像素转换或跨语言图像序列化。
3. 恢复期间不能对旧画面继续操作。公共 Session 在进入恢复状态时只释放一次按键/鼠标，清除呈现确认；解码恢复后仍要求当前来源的新帧实际呈现才允许输入。保留用户输入锁，静态桌面不受旧的两秒新帧限制。已扩展生产 GUI 至本地协议记录器测试，检查恢复前/中/后、旧时间戳和重复 poll。
4. Windows 完整回归发现四项 P2P 失败，旧基线同样失败。UDP 之前发送给关闭端口的报错被当成整条接收通道终止；此外，原 NAT 相邻端口猜测用于 loopback 会撞到同机应用/发现 socket，使其收到原本应由 P2P 层消费的 Punch。现区分 UDP ConnectionReset/ConnectionRefused 与真正配置/权限错误，跳过不存在 NAT 的 loopback 等地址的相邻端口猜测，仍保留其他真实候选/NAT 探测能力；未知或不同组的 P2P 控制不再漏入业务数据。没有改系统网络、驱动或 Windows socket 全局设置。

Windows UDP 语义依据：Microsoft recvfrom 文档说明，UDP 的 WSAECONNRESET 可表示先前发送引发 ICMP Port Unreachable，不能等同 TCP 流重置。参考 https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-recvfrom 。

## 实际 Relay 故障仍未闭环

先前“connected=true 所以心跳正常”的表述不充分。本轮直接检查 peer_is_responsive 后，一次失败现场已确认 heartbeat_responsive=false。必须区分已鉴权的会话对象存在、业务接收持续工作、心跳仍有效及已显示画面。

一次基线复现：223 个包到达抖动缓冲并弹出，解码 8 帧，但恢复版可见帧只更新一次，207 计入 decode_queue_dropped。这个旧指标也统计等待参考帧时主动跳过的包，不能说成 207 次真实队列满或网络丢包。新增参考缺口/跳过/关键帧请求计数用以区分。

补充恢复机制后另一次仍只收到第一帧。开发专用计数记录中，中继收到 253 个目标匹配的响应并尝试本地转发，而 Workspace 接收 socket 只读取 17 个 datagram，最终仅一帧完成解码；直接心跳检查已超时。同一时间主机日志记录约 15 fps，发送 216 帧、发送错误 0。这把调查重点收敛到目标中继响应的本地交接/会话接收进度，但转发计数是调用尝试，不证明每个 send_to 成功，更不能据此宣布已经查明唯一根因。

后续包含本测试进程原生采样的组合调用被平台安全检查拒绝，没有执行，也没有用别的入口重试采样或提权。继续采用源码分析和不使用真实设备网络的本地协议测试。临时计数插桩已从正常构建源码移除、在独立 staging 留档。没有把它带进正式应用。

本地测试新建实际加密 Actor、分片数据与容量 2 的解码队列，故意使队列满，验证 Pong、来源请求/响应及取消仍然畅通；12 次重复通过。但是它没有复现外部 Relay 的停顿，不构成真实 Relay 顺序切换通过。六轮切换、每轮三帧连续正确的门槛没有降低；当前真实结果仍是未通过。最终 P2P 修复属于另一条缺陷，本轮没有把它当成 Relay 修复证据。

## 最终自动化结果

| 平台 | remote_core | client | App | 单独 GUI 输入恢复回归 | 编译/静态检查 |
| --- | ---: | ---: | ---: | ---: | --- |
| macOS | 236 | 22 | 107 | 1 | App all-target check；三库严格 Clippy 均通过 |
| Linux | 236 | 21 | 92 | 1 | App all-target check 通过 |
| Windows | 234 | 21 | 92 | 1 | App all-target check 通过；原四项 P2P 失败全部消除 |

各数字有跨平台重复，不合并成独立功能覆盖量。App 默认忽略的真实 Relay 和其他专项测试不自动视为通过；本轮显式执行的是本地 GUI 输入恢复记录器。Mac all-target 示例仍有原有未使用导入警告，不能把严格 lib Clippy 的范围扩大到所有例子。Android 没有新增实体机验证。

Windows 先后编译基线和修复源码共享 target 曾使一次聚焦测试显示旧用例数量，不能取那一轮作为最终证据。最终构建明确指定绝对源码 manifest/cwd，核对全部修改文件 SHA 并要求新增测试名称出现在 --list，重新完整运行。最终全 core 为 234 通过/0失败，而不是选择性过滤旧四个失败。

## 源码与设备状态

本轮共修改 8 个现有源码/测试文件，名单和 SHA 在 /Users/jinliang/Workspace/.rp-gpui-continue-20261001/final-canonical-source-manifest.json。Mac Studio、HO5、cube 的对应八文件 SHA 已分别核对一致，保存 final-device-and-source.json。源码仍未提交或推送；协议结构、签名策略、GUI 入口、已发布包和主机捕获实现未改动。

两台 Mac 仍运行正确签名的 alpha.7 / 20260930.7；HO5 alpha.7 服务仍 active，PID1899036，NRestarts=0；Windows 无 RemotePlay 进程，UU Running/PID27892。合成测试窗口已退出。所有本轮引用的最终构建和测试命令均已退出，没有留后台作业等待。

主机侧证据在 /Users/jinliang/workspace/.rp-gpui-continue-20261001，特别是 sequence-repro2.json、sequence-recovery1.json、sequence-tap1.json、host-tap1-media.log、local-actor-repeat.json、mac-core-corrected-final.log。Linux 使用 /var/home/liang/workspace/.rp-gpui-continue-20261001；Windows 使用 C:/Users/Liang/Workspace/.rp-gpui-continue-20261001，其中 verified-core-test-list.log 和 windows-core-corrected-final.log 为最后有效用例/结果。

## 发布结论

暂不发布、不部署。不能把本轮媒体恢复与 P2P 修复的自动化成功写成“原 GUI 全部恢复”“切换两台 Mac 已稳定”或“所有平台原生 GPU 视频已完成”。下一项发布阻断仍是恢复版真实 Relay 持续视频/顺序切换，以及 Linux/Windows 原生视频呈现；原 31 功能组保留验收和同条件性能指标继续保留，不能跳过。
