# 常态设备池与有向功能验收计划

2026-10-10 用户指定两台 Mac、一台 Windows cube、一台 Linux HO5 为常态验证设备，未来加入常态 Android。当前先落实 HO5 的 SDK、准确源码和 Linux 条件编译；现有硬件足以推进这些工作。离线、权限或依赖问题只影响对应单项，不泛化成“缺机器”。

## 当前桌面全互测轮次（2026-10-17，覆盖下方历史计划）

用户最新修正按操作系统类型验收：macOS↔Windows、macOS↔Linux、Windows↔Linux，共3组6方向。两台Mac视为同一种设备，不要求重复整套验收，Mac↔Mac不作为本轮必过项；本轮 Android 暂不验收，不取消长期 Android 支持。唯一当前明细在 [FOUR-PLATFORM-MATRIX.json](FOUR-PLATFORM-MATRIX.json) 的 `current_desktop_acceptance`，复用本文件及旧矩阵，不新建任务系统。下方旧日期的在线、安装、权限和验收观察保留为历史，不能覆盖本节。

统一目标为 alpha.9，冻结 producer 源码 `feba70a21830540e919e94f9441f5088461a5438`，保留源码 patch `e42426ca85da429676f1a2f6d1532034f4a8a6361f1543f021476fe1d48ad7b2`。版本字符串相同不算对齐：每平台须按原发布门禁取得真实 build identity、observed receipt、最终 manifest 与实际运行映像绑定。目前两 Mac/HO5 安装为 db9fc29，cube 仍 alpha.8；Linux 新候选只有真实编译/静态身份，尚无 native runtime observation。本文更新不触发重编译、部署或重装。 `feba70a` 是本轮二进制验收基线，不随文档 HEAD 前移。后续仅矩阵/计划文档提交不构成二进制版本差异；仍按原 compiler 源码快照、build identity、配置、产物哈希及 observed receipt 核验实际运行映像，不把文档提交重新标成二进制来源，也不放宽原 producer 源码校验。

箭头为采集发布端 → 接收呈现端。“推送”由发布端主动发起；“拉取”由接收端主动选择同一发布端；键鼠沿反向作用于发布端的专用测试窗口。两个方向、推送与拉取都须独立记录，不能互相借用结果。

| 采集端 → 接收端 | 主动推送 | 主动拉取 | 视频 | 音频 | 鼠标点击 | 键盘输入 | 断线重连 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| macOS（MacBook） → Windows（cube） | 未测 | 未测 | 未测 | 未测 | 未测 | 未测 | 未测 |
| Windows（cube） → macOS（MacBook） | 未测 | 未测 | 未测 | 未测 | 未测 | 未测 | 未测 |
| macOS（MacBook） → Linux（HO5） | 未测 | 未测 | 未测 | 未测 | 未测 | 未测 | 未测 |
| Linux（HO5） → macOS（MacBook） | 未测 | 未测 | 未测 | 未测 | 未测 | 未测 | 未测 |
| Windows（cube） → Linux（HO5） | 未测 | 未测 | 未测 | 未测 | 未测 | 未测 | 未测 |
| Linux（HO5） → Windows（cube） | 未测 | 未测 | 未测 | 未测 | 未测 | 未测 | 未测 |

共3组/6方向/42项，当前统一目标真机通过数为0；另有三个OS主测设备的后台存活项，均未验收。macOS主测选MacBook：已有签名/构建环境、安装alpha.9及冻结产物证据；Studio仅在必要复现/复核时使用，不重复整套。若需更换主测，记录实际映像与理由，不能用更换设备绕过拒绝。前一版6组12方向矩阵保留在同一JSON的 `superseded_acceptance_scopes`，仅为历史，不叠加为本轮要求。10月14日旧 alpha.8 Studio→MacBook 的视频显示及音频回调仅作历史 partial，反向认证超时为历史 failed，不计入当前通过率。

执行顺序：

1. 对每个可用组合核验实际运行映像/来源/版本、无冲突的会话和租约、正常发现路由及源授权；同一目标源码尚未对齐时不记通过。沿既有非敏感观察判断主测条件，不重复轮询无变化状态，不重复提示配对、重建分组或读取邀请码。
2. 先macOS(MacBook)↔Linux(HO5)，再macOS(MacBook)↔Windows(cube)，最后Windows(cube)↔Linux(HO5)。只跳过受阻组合，任何独立就绪组合都可前移，不能因一台设备阻塞停止全轮。
3. 每组先前向再反向；每方向按主动推送→主动拉取→视频/音频→点击/键盘→断线重连→原生资源释放执行。每次只使用已绑定 PID/窗口 ID/source revision 的自有测试窗口、短非敏感文本和已知音频信号；产品链路产生事件，目标原生事件及可见状态证明生效，不能用 OS 直连注入代替产品。
4. 三个OS主测设备另验证关闭自有 GUI 后同一后台 owner/映像/profile 存活且可再次连接；不停止现有服务或其他会话。HO5 当前 Standalone 不是已验证的分离后台，须按既有迁移门禁处理。
5. 配对仅沿正常产品 UI，不能读取或转发凭据、不改变持久安全授权。HO5 正式候选原上传 `81e4983d-5c9e-4f21-aeee-90fe1aee6936` 因源策略暂停/0字节，实际源 `sdmntprwestus.oaiusercontent.com`；不重试或换域名/路由，等待原通道修复确认。旧 observer 草稿上传另有 operation，不得混用。Windows 三路径 ACL 继续原待确认边界，不盲目续装。
6. 逐项回填真实 UTC、两端运行版本/源码/映像和 build identity、route/session/source revision、原生呈现/音频输出/输入副作用/重连和清理证据。无证据留 not_tested；已执行失败记 failed；部分链路仅 partial；不支持明确 unsupported，不能删项或当通过。原文件传输、多窗口、画质/性能与视觉回归要求继续保留，七项核心矩阵不缩减原功能范围。

### 2026-10-17 17:58–17:59 UTC 新只读核验与独立进展

MacBook主测仍0路由/0会话；Studio仅复核同样无路由/会话。HO5 GUI端点已响应、0会话/无租约，但当前背景状态命令exit1，不能据此新判后台角色或已配对；已装映像仍非冻结候选。cube原owner PID34140仍在，三处新profile DACL未保护且含额外capability ACE（2/1/1），未修改。正式候选原上传仍paused/0字节，源授权需刷新；只观察原回执，未重启Octendra、重放上传或修改DNS/ACL。没有新增真机通过。

真机受阻期间修复原 `gui_launch_lifecycle.py` 的独立缺口：`KeepAlive=false` 不足以证明关闭GUI后不重启，配置的StartInterval/Calendar、目录/文件/mount/event激活须拒绝给出该证明；只改只读校验，后台host策略不变。32项定向和原release 563项（554通过、9既有skip）通过，不计作真机后台存活验收。详细本地回执为 `/Users/jinliang/Workspace/Codex/2026-10-10/task-4/weavelo-db9fc29-build/readiness-and-lifecycle-activation-20261017-evening.json`。

后续仅沿正常配对、原HO5来源修复/授权及原cube三路径ACL确认推进；确认到位后先核验实际目标映像，再对任何已就绪OS组合按本节六方向顺序执行。不得由Octendra连接恢复推断DNS已修复，也不得由脚本回归推断产品交互已通过。

## 以下为历史设备池观察与较早范围（不增加本轮必过项）

## 设备登记与证据

| 设备 | 平台 / 设备 ID | 2026-10-10 当前连接观察 | 本轮产品版本证据 |
| --- | --- | --- | --- |
| MacBook-M2-Max | macOS / ba3bf113-2390-466e-88bc-40d5b4f02884 | Remote Hosts 在线 | 未重新核验；先前 profile ownership 待本机处理的记录不能当作当前启动成功 |
| Mac-Studio | macOS / 8af88e35-f316-4dd9-9810-fa5bd3a22196 | Remote Hosts 在线 | 未重新核验；历史安装回执与当前执行文件分开 |
| cube | Windows / f840a7fb-33c3-48bb-b41a-5e81cbde2ea9 | Remote Hosts 离线 | 当前进程、版本和权限未验 |
| HO5 | Linux / 02f29fa0-48c1-4e31-a345-90aa88467323 | Remote Hosts 在线 | user service active/running，PID 2232948；配置路径指向 alpha.8，实际执行文件版本/摘要未验 |
| Android-Phone（未来常态项） | Android / f4b13b05-1d5d-4460-8517-0fe3be577889 | 当前登记离线，能力未报告 | 未验；以后按真实设备、ADB/agent 状态和用户授权登记 |

连接观察为 devices_list receipt `req_2be02dc31a50445296df791be066d602`，UTC 2026-10-10T03:12:24Z。HO5 进程状态观察见 operation `42de4279-9b4c-415d-8b9c-af6bea451607`，UTC 03:19:56Z。[本轮环境证据](../reviews/2026-10-10/evidence/ho5-sdk-and-pipewire-review.json) 保存具体事实。工具 agent 的 0.10.25 版本不是 RemotePlay 版本。

每次验收刷新：设备在线时刻、CPU/GPU/显示协议与分辨率、SDK/编译器/缓存能力、当前 PID/执行文件路径/摘要/版本/renderer、profile 可访问性、源/输入授权和传输连接状态。只读元数据优先；不读配置秘密，不自动授予权限。过去在线或编译过不能代替这次可用性；服务配置的 executable path 不能代替运行进程来源。不可读取项记为 unknown，并保留拒绝原因。

## 有向连接

箭头表示采集发布端 → 接收呈现端。四台桌面设备有 12 个不同设备的有向组合，各自执行，不把反向结果互换：

| 发布端 \ 接收端 | MacBook | Mac Studio | cube | HO5 |
| --- | --- | --- | --- | --- |
| MacBook | 单机回环另记 | 待验 | 离线未验 | 待验 |
| Mac Studio | 待验 | 单机回环另记 | 离线未验 | 待验 |
| cube | 离线未验 | 离线未验 | 单机回环另记 | 离线未验 |
| HO5 | 待验 | 待验 | 离线未验 | 单机回环另记 |

桌面三个 OS 家族共有 6 个跨平台有向组合，Mac 的两个实体分别留下设备级证据。加入常态 Android 后覆盖 macOS/Linux/Windows/Android 的 12 个跨平台有向组合及对应的实体设备。现有 [FOUR-PLATFORM-MATRIX.json](FOUR-PLATFORM-MATRIX.json) 保留历史/current supplied evidence；本计划不改写旧观察或补成已通过。

同机回环、合成码流、多个进程测试单独标记；它们可验证局部接口，不能计为两个独立物理设备的产品验收。Windows/Linux 单机项不会被泛化成必须添置第二台机器才能推进其他方向。

## 每个方向的功能条目

| 功能 | 必须留下的实际结果 |
| --- | --- |
| 连接、身份与源列表 | 已认证 peer、source ID/revision、应用/窗口列表、错误及断连恢复；源列表不等于帧 |
| 应用窗口采集 | 真正选中一个应用窗口的像素，窗口移动、缩放、关闭、遮挡、快速换源；未支持或未编译分别说明 |
| 多窗口并行 | 两个不同窗口同时推送/拉取；源、输入、音频、文件 scope 不串；关闭一个不会撤销另一个 |
| 原生视频与画质 | 实际 capture→encode→传输→decode→GPUI 显示证据，色彩/SPS、首帧、连续帧；提交 UI 不等于 presentation |
| 输入 | 已呈现的当前源才允许输入；键鼠、坐标/DPI、暂停与换源竞态、无授权/失效源拒绝；窗口不能降级成全桌面注入 |
| 音频 | 采集与播放、静音、连接归属、窗口/会话切换和断开资源释放；未支持不能用视频通过掩盖 |
| 文件传输 | 双向、多窗口身份绑定、进度、取消、错误与连接切换 |
| 生命周期 | 暂停恢复、重复连接、拒绝/取消选择、撤销、transport 断连、worker 退出及 FD/进程/内存归还 |
| 原产品视觉 | 实际可执行文件的 idle/network、连接视频、源选择、传输、错误、windowed/fullscreen、代表性 DPI/分辨率截图及交互 |
| 性能 | 分阶段 CPU/GPU、复制次数、帧龄/实际显示延迟、丢帧、长时内存；不降低画质/输入/音频来换数字 |

状态按单功能记录 passed/failed/partial/not_tested/unsupported；能力未编译时记录 source_unvalidated，不能当作已可用。必须保留运行版本、源码/产物摘要、观测时刻、两个设备身份、实际操作步骤、截图/日志/测量及限制。平台总结果不能覆盖未验子项。签名、安装、源枚举、首帧、呈现、输入、音频和长时验收分别报告。

## 当前 HO5 最小顺序

1. 只读核实一次性 DNS/SDK 本机回执；当前精确域名仍为非公开地址，旧传输保持 paused/0 bytes。保留原 operation，不用其他来源绕过 source guard。
2. 使用 [HO5 DNS+SDK 草稿](../reviews/2026-10-10/HO5-DNS-SDK-ONCE-DRAFT.md) 供主会话统一审阅；本轮不运行管理员命令、改网络或重启设备。
3. 源码传输完成且逐文件 hash 符合批准切片后，保存 delta；在没有并发构建时仅刷新其 changed-file mtime。
4. 复用 `/var/home/liang/workspace/remote_play/target`，先 `cargo check -p host --lib --locked --offline`，再 `cargo check -p remote_play_app --bin remote_play --locked --offline`。缺缓存/依赖时给精确缺项，不更换机器，不新建 target，不启动完整矩阵。
5. 编译成功后才做 HO5 的系统 picker、MemFd/MemPtr、FD 回收与窗口源小步真实验收，再选择在线 Mac 做两个方向的实际帧/输入/音频。其他方向保持各自状态。
6. 原生呈现 feature、打包、签名、部署与全量视觉/性能验收另按现有门禁推进；本轮普通 check 和纯逻辑测试不代替它们。

构建前至少留 8 GiB，运行中保 4 GiB；记录磁盘余量和 target 所有权/现有构建锁。锁文件“存在”不证明被占用，既不删除它，也不据此启动并行构建。保留既有源码改动、可回退签名产物和每设备实际部署状态。
