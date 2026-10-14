# 原 UI 恢复续接：原生调用调度、真实 Relay 与权限状态

## 本轮权限结论

四台 Remote Hosts 设备在线，设备与账号均允许 code:read、code:write、terminal:exec。本轮实际完成主仓库写入、三种桌面构建和测试，因此不是整体只读或缺少执行授权。

新真实 Relay 顺序测试在 Mac Studio 鉴权阶段超时。随后用于区分已安装本机主机与 Relay 的 local_host_health_probe 工具调用被平台安全检查拒绝，未执行；没有通过换工具、包装命令、另一个设备或替代相同操作绕过。拒绝后仍完成独立的源码检查和纯本地合成输入测试。没有将平台限制当作用户没开录屏权限，也没有要求重置 TCC 或网络。

## 已恢复的上轮证据，不混算为本轮成功

/Users/jinliang/workspace/.rp-relay-20261002 下的 baseline-clean-runtime-build.json、native-filename-directory-controls.json 和 unchanged-baseline-sequence-summary.json 记录了同一份未修改测试程序的对照。Cargo deps 目录中执行时，原生 VT 初始化曾停留约20秒；复制相同字节到独立运行目录时，初始化明显缩短，两轮各六次 Studio / MacBook 交替的正确颜色画面验证通过。目录相关性已观测，但不能直接指定操作系统内部原因，不能据此宣称关闭某项安全策略能够解决，实际也没有修改策略。

旧 actual-main-I/receipt.json 记录实际 GPUI 开发版一次30秒显示 MacBook Relay 视频，764帧、0解码错误；该文件不是本轮新通过记录，且 scene submitted 不是GPU完成或物理显示测量。

## 本轮实施的生产修复

新增 client/src/native_call.rs；在 MacVideoDecoder 格式/会话创建、同步提交解码和释放等待三个边界使用它。多线程 Tokio 上通过 block_in_place 告知调度器当前线程即将进入同步原生调用，让独立的网络与计时任务继续推进；原生调用、借用的压缩数据、回调上下文和资源仍在原线程，不搬迁裸指针。

未新增视频队列、逐帧复制、像素转换、线程逐帧创建或协议字段，未调整码率/分辨率/帧率。没有更换Tokio、GPUI或编解码器。生产范围只在Mac原生调用；Linux/Windows通过相同逻辑的测试验证及现有平台回归，未改它们的实际解码路径。

边界：block_in_place 保护独立异步任务的调度，并不加速操作系统本身的初始化，也不能取消已经进入的原生调用。单线程运行时维持同步调用约定；产品GUI入口已有多线程运行时要求。不能把此修改说成彻底消除所有卡死或完成整个性能目标。

## 可重复的失败/成功对照

6项新单测覆盖：只有一个异步worker时原生等待不饿死计时/模拟网络响应、提交仍在原线程及原借用缓冲、无运行时调用、单线程调用不panic、释放过程异步任务继续执行、错误值不丢失。

HO5上将同一测试体分别链接“直接阻塞调用”基线和本轮生产helper，前者exit101失败，后者exit0通过。两次差异只在helper实现。此项是合成调度控制，不是产品网络延迟或真实解码性能数据。证据为 .rp-relay-resume-20261002/scheduler-before-after.json。

## 本轮真实 Relay 结果：仍未通过

使用本轮完整冻结源码，生成与Cargo输出逐字节一致的独立运行目录测试程序。原始阈值未变：六轮 Studio/Book 交替、每轮三个连续正确颜色的当前来源帧、连接及来源代次校验、仅订阅唯一命名的测试窗口、始终锁定输入。

第一次新运行在连接 Studio 时约5.126秒后 authentication timed out；没有进入视频订阅，round_count=0，planned的后两次没有执行。该失败发生在解码器之前，不能将它归因为本轮的原生解码边界，也不能宣称本轮18次连接通过。sequence-1.json和sequence-summary.json已保留。

现场只读核对：Studio原alpha.7主机仍运行PID78134、主控制端口39271存在，若干历史loopback路由存在；这些现象尚不能证明路由表已满或找到唯一根因。没有为了制造通过结果重启/覆盖现有主机。

## GUI输入回归补齐

显式执行恢复版GPUI输入回归时，原测试准备“已显示帧”却没有等待新加的解码恢复标记初始化，导致空协议trace并报GPUI swallowed the press。检查确认这是合成fixture预条件与真实媒体任务启动的竞争，并非可以通过删掉产品恢复保护来修复。

测试现在先等待媒体任务报告等待关键帧，再显式建立本地模拟的“已解码/已呈现”前置状态。原来的Down/Up/键盘与控件遮挡断言全部保留，并追加：进行中的拖拽遇到引用恢复时必须ReleaseInput、恢复时不得送键鼠、恢复标记解除但尚无新显示帧时仍不得恢复输入。修订仅在测试文件，不向工作应用输入内容。

## 验证结果

- macOS：client 28、remote_core 236、App 107项常规测试通过；3项默认忽略专项未混进这些计数。
- Linux：client 27、remote_core 236、App 92项通过；恢复版App全目标检查通过。
- Windows：client 27、remote_core 234、App 92项通过；恢复版App全目标检查通过。UU Running/PID27892，RemotePlay进程0。
- client严格Clippy在Mac通过。
- 修订后的GPUI本地输入专项与既有egui本地输入专项分别通过；它们都使用本地协议记录器，不代表跨机真实键鼠验收。

Mac本轮已构建实际restored_gui_acceptance开发程序并生成可校验运行副本，但未执行新的实际GUI跨机验收，不把构建当出图结果。

## Linux/Windows视频呈现的进一步核实

当前锁定gpui-ce 0.3.3的SurfaceSource只有macOS CVPixelBuffer变体，Windows DirectXRenderer::draw_surfaces为无绘制实现。这解释了为什么原控件窗口能运行，而完整低拷贝视频仍需接入。它不等于原设计不能跨平台，更不能用CPU逐帧转换作为完成版交付。

2026-10-02读取的GPUI-CE上游main已有Linux Texture及WindowsCapture形式的SurfaceSource，也发生了平台crate拆分。上游main不是本仓库锁定版本，声明存在不等于与当前解码器及yororen_ui兼容。没有趁此切换依赖、fork框架或转Flutter；若需显著升级/重构，先给用户具体范围确认，避免重演未经确认的UI替换。

公开依据：Tokio task::block_in_place文档（独立任务可继续、原生调用不可取消）；GPUI-CE main/crates/gpui/src/elements/surface.rs、main/Cargo.toml。仅作为API/工程方向依据，不替代真机证据。

## 当前交付边界

恢复原GPUI设计与31组功能保留目标不变。现用alpha.7、原签名、原网络身份、系统授权及UU没有被本轮修改，没有NAS发布、提交或推送。测试窗口均已退出。主要发布阻断仍是当前真实鉴权/Relay长稳、Linux/Windows原生视频通路，以及完整功能/性能和真实界面验收。

源码与原始证据：主仓库 /Users/jinliang/remote_play；各设备 .rp-relay-resume-20261002 目录。基础编译source_id 605c228291fb1384017dfe10a00994d982bff4a3e18669071f13acd6444ee719；之后只有restored_input_tests.rs的fixture与恢复断言被修订，记录在final-source-scope.json。旧失败日志保留，不覆盖为新成功。
