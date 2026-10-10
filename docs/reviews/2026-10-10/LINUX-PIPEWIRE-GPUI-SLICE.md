# PipeWire worker 与 GPUI 本地分享接线，2026-10-10

本切片实现了受限 PipeWire worker 的源码、格式/平面/生命周期契约，以及原 GPUI 设备抽屉中的本地分享入口。52 个分层 Rust 用例和普通 Mac 主包检查通过。**Linux worker 与 Linux GUI owner/启动接线尚未在 Linux 类型检查或实体环境执行；不是可部署、已呈现或已通过性能验收的后端。** 继续执行[已批准的最小方案](../../plans/LINUX-WAYLAND-CAPTURE-ADAPTER.md)，父提交为 `cc0262390ea1909dd27968f7aa911f06d728054e`。

## 实现与边界

`host/src/linux_pipewire/native.rs::prepare_pipewire_capture` 将 portal 授予的 FD 单次交给 `ContextRc::connect_fd_rc`，固定连接本次 node。单一专用线程持有 loop/context/core/stream/listener 和所有映射；不打开默认 PipeWire remote。请求 CPU MemFd/MemPtr buffer 及 Header/Crop/Transform 元数据。每个 Buffer 在回调内检查并复制，RAII 归还发生在交付 owned 数据之前。关闭、core/stream 错误和格式错误结束 worker；20ms loop timer 观察关闭，即使没有新像素。线程 join 移到清理线程，避免阻塞 GUI。实际 FD 回收、回调/计时上界仍需要 Linux 观察，不能由这些源码推断。

纯 `CopyContract` 检查单块或多平面 NV12/I420、stride、chunk offset/size、plane 数量、crop/transform、尺寸和内存预算。映射 offset 已由 PipeWire 应用，不再叠加。末行不必包含未使用的 padding。颜色 range/matrix/transfer/primaries 保留原 SPA 值；序号、纳秒 PTS 与 host arrival 分开保存。新的本地 `format_revision` 在 renegotiation、format 清除和 discontinuity 时推进，清旧原始槽并阻止旧票据进入编码器。相同格式的新协商也重新开始实际 HEVC/IDR；未修改 wire 数据结构。

最新原始槽只保留一张未提交帧；首帧准备和 encoder writer 各可持有一张正在交接/写入的 owned 帧。准备首帧不能覆盖已到达的更新像素。后续 pending raw 的期限为 250ms，进 mailbox 或 encoder 不会刷新复制时间；压缩 AU 不按这个期限丢弃。静态、只按 damage 更新的源允许首个 consumer 取一次当前有效快照，保留原 arrival/复制时间，不把它写成新采集。暂停清槽并跳过复制，格式变化清槽，撤销结束交付；真实静态源、暂停/恢复与 compositor 行为仍未测量。

prepared lease 持有 worker，退出监督只持 Weak 引用，避免资源环。worker 退出会撤销 grant，使用已有有界 portal Close 路径。本地取消还会同步拒绝准备、FD 交接、publish、commit、换源和发出 AU 前的交付检查，不依赖后台 cleanup 已处理 watch。私有测试覆盖这段取消竞态。

GPUI 使用现有 Ely 小按钮，放在 390px 设备管理抽屉：Refresh viewers、Window、Display、Cancel selection、Stop。标量 State/Action/Effect 与 Linux 资源 Owner 分离；重复动作禁用，旧异步完成不能覆盖新意图。每个 GPUI 窗口持有自己的选择/取消资源，关闭窗口结束它持有的分享。原主机 runtime 复用进程内有界通道，未启动第二个网络 runtime。

主机候选 grant 改为按订阅保存；认证 nonce、connection、share scope、source revision 在提交时继续复核。不同订阅不能取用对方窗口。取消准备保留旧源，旧流继续持有自己的 grant 直到正常换源 teardown。portal 源 ID 使用 Linux X11 资源范围之外的临时值，不公开 node ID、不写持久身份。输入保持只读，ScreenCast 不授予输入权限。已结束的兼容采集在 Linux 本地控制开启时仅保留有界请求/版本意图，供用户恢复选择；它没有输入 target 或 audio group，退订/连接过期仍清理。其他平台保持原清理规则。

GPUI 的选择器目前使用 ScreenCast 标准允许的空 parent；没有伪造 Wayland export handle、增加 GTK 或改 vendored GPUI。真实窗口绑定/模态行为尚未实现或验收。提交后，接收者通过已有 Sources / SwitchSource 选择新源；没有静默切换另一设备或自动操作系统确认。

## 实际检查

固定脚本 `/tmp/rp-worker-bounded.py` 复用 `/Users/jinliang/rust-target`，预检查 8GiB、运行中保留 4GiB，记录命令、退出码和源码哈希。以下计数为唯一用例数，不累计修复后的重复运行。使用 Mac 的私有 dbus-daemon、合成 YUV、实际 FFmpeg/libx265 与环回 UDP fixture；未启动产品窗口、产品网络 runtime 或系统 portal。

| 过滤器 | 用例数 | 覆盖 |
| --- | ---: | --- |
| `linux_pipewire::tests` | 10 | 布局、padding、复制所有权、协商、背压、过期、初始快照、独立窗口 |
| `linux_portal::runtime::tests` | 12 | 真正私有 D-Bus、授权链、关闭/取消/owner 丢失、worker 监督、最新首帧、同步取消 |
| `service::` | 10 | 身份/版本、双订阅隔离、替换保持旧流、失败意图恢复、输入和 preflight |
| `linux_video_encode::lifecycle_tests` | 3 | 真实 HEVC 色彩/协商重启的 IDR、暂停设置不启动编码器 |
| `linux_raw_encode::tests` | 7 | 实际像素/颜色 SPS、参考 AU 顺序、多 slice、背压、进程回收、原始帧过期 |
| `linux_frame::tests` | 7 | 邻近 plane/crop/stride/内存与 mailbox 回归 |
| `desktop::local_share::tests` | 3 | 标量状态及实际 GPUI 测试环境中的 Ely 点击/禁用/取消 |

合计 52 passed、0 failed，20 个新增用例、32 个受影响邻近回归。普通 `cargo check -p remote_play_app --bin remote_play --offline --locked` 通过，**只代表 Mac cfg**。GPUI 测试组件没有实际 Linux picker、GPUI dashboard/资源 Owner 或呈现验收。未重做未改动的 Python、签名、DNS和部署测试。

检查过程中先修复 GPUI fixture 的可变借用编译错误，再修复普通 Mac 构建中 Linux-only 局部变量的 cfg 保护。首次离线解析缺少 pipewire 缓存后，仅解析公开 crates.io 依赖。未把失败尝试或 0-case 过滤计为通过；现有非目标测试分支、Ely/产品组件与 `PermissionsExt` 警告保留。

仅增加 Linux-only `pipewire = =0.10.1`（`v0_3_65`）；锁中 PipeWire/SPA/sys 均为 0.10.1，新增 13 个包。既有 GPUI/GPUI media、bindgen 0.71.1 和 unicode-width 0.1.14 未升级；锁文件相应行仅新增版本消歧。无 egui/eframe，未替换 GUI 或原生接收呈现。公开 crate 源码按锁内 checksum 检查过，API 阅读不能代替 Linux 编译。

## 尚未覆盖，禁止据此部署

HO5 新只读操作 `85adbe57-4b46-4d4b-88a8-1b4047c3dbc2` 退出 0：未发现 PipeWire/SPA/ALSA 开发头文件、clang 或已安装 Flatpak SDK/LLVM runtime。已有 toolbox/podman 不能取得合法开发环境；Mac 只有现有 MySQL 容器，Rust Linux target/zig 不是目标 C SDK。未安装包、创建容器、拉镜像或绕过 NoNewPrivs。本轮也未以已知缺失的 SDK 重做注定失败的交叉构建。

原 HO5 来源传输 `37c1dfde-5dbc-420e-a19d-aeb49e8702d8` 仍暂停，确认字节数 0；没有重复来源传输、另建仓库或越过 DNS 管理员边界。当前新源码没有出现在 HO5 的旧来源包中。合法恢复后须完成精确 hash delta，再按既定脚本只刷新改变文件的 mtime，之后才构建。

未覆盖：Linux 全部 native/GUI cfg 类型和链接检查；系统 picker、实际 MemFd/MemPtr 映射/归还/FD 生命周期；真实编码色彩/SPS；静态窗口、选择、关闭、撤销、断连、多窗口、暂停恢复；传输、解码与实际 GPUI 呈现；输入、音频、文件、其他平台实体回归；长时 CPU/GPU/延迟/内存、DPI/分辨率/窗口/全屏截图。当前明确拒绝 RGB 转换、DMA-BUF、P010/HDR、未知 range、非零未验证 chroma site、interlace/multiview、负 stride 和非 identity transform，不能宣称覆盖这些源。

在合法 SDK 环境通过类型/链接和私有检查、既定来源恢复完成之前，无需用户操作系统选择器。随后最小实体步骤是：接收端连接 Linux 主机，在该主机原 GPUI 设备抽屉刷新接收者，点击 Window 或 Display，正常完成系统选择，再在接收端 Sources 选新源；另做 Cancel/Stop 和两窗口独立测试。这些步骤未执行，仍需记录实际 executable、呈现证据和各功能/性能结果，之后遵循签名与分阶段部署保护。

原 105 项混合改动的路径状态/哈希保持一致。此次仅隔离提交相关源码/锁文件和本报告/证据；不提交产物、私有配置，不推送、不签名、不发布、不安装，不更改 TCC、系统安全或运行程序。精确源码、测试和环境观察见[证据](evidence/linux-pipewire-gpui-slice.json)；本机记录不是 Linux 外部验证或发布授权。
