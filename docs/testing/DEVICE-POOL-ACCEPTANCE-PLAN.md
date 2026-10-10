# 常态设备池与有向功能验收计划

2026-10-10 用户指定两台 Mac、一台 Windows cube、一台 Linux HO5 为常态验证设备，未来加入常态 Android。当前先落实 HO5 的 SDK、准确源码和 Linux 条件编译；现有硬件足以推进这些工作。离线、权限或依赖问题只影响对应单项，不泛化成“缺机器”。

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
