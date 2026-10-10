# HO5 SDK 核验与 PipeWire 输入审查，2026-10-10

本轮构建目标固定 HO5。只读查询确认它是 Bazzite/Fedora 44，Rust 1.98/GCC 已有，磁盘余量约 708 GiB，稳定源码及 target 已存在。未启动 Linux 构建：host 缺真实 PipeWire/SPA/ALSA 和 Clang/CMake SDK；最新切片尚未放置，精确源码域名仍解析为私有地址，原传输仍暂停。现有容器 rootfs 读取被 PermissionError 拒绝后停止，没有换机器、容器或来源绕过。此处是具体依赖、通道和来源状态，不是硬件不足。

环境查询、旧 operation 状态、三台在线/两项离线登记和最小测试源码摘要见 [机器可读证据](evidence/ho5-sdk-and-pipewire-review.json)。HO5 运行 user service 为 active/running、PID 2232948；配置路径指向 alpha.8，实际进程版本/摘要仍未知。没有替换它或重启服务。

## 已确认缺陷与修复

`native.rs` 原先只检查 ChunkFlags::CORRUPTED；锁定 libspa 0.10.1 没有为 EMPTY 命名，但保留原始位。SPA 的 EMPTY 表示媒体中性数据，不能据此把复用 buffer 中残留的旧窗口像素当作当前帧。[官方 SPA Buffer 标志](https://docs.pipewire.org/group__spa__buffer.html)

现在共享 `validate_chunk_flags` 拒绝 CORRUPTED/EMPTY，native 回调在创建 raw byte slice **之前**调用；CopyContract 也在复制前校验相同 metadata。这样不发布一张由无效或中性 storage 伪造的窗口帧。native 已有错误路径负责归还 Buffer、关闭 mailbox、退出 worker 并撤销对应 lease；未加入中性帧合成、颜色猜测或另一种媒体后端。

新增案例用结构合法、留有旧像素的 NV12 storage 验证 EMPTY、CORRUPTED 及组合标志都拒绝。受影响 `linux_pipewire::tests` 11 项通过（其中 1 项新增），Mac 普通 target、offline/locked、复用原 rust-target、退出 0，7.61 秒。覆盖共享 copy contract；Linux native callback 并未在 Mac 编译。旧的 52 项阶段证据保持原记录，不相加或重跑成新的 Linux 成功证据。

## 聚焦审查

| 边界 | 本轮结论与仍待验证项 |
| --- | --- |
| 原生 pointer / 映射 | 只接受 MemPtr/MemFd、READABLE、非 null data/chunk、maxsize≤128 MiB；EMPTY 在 slice 前拒绝。只读 slice 可覆盖重叠 plane，scope 留在 dequeue Buffer 生命周期内。实际 pointer 和 mapping span 依赖 PipeWire MAP_BUFFERS 的原生契约，未通过本地纯逻辑测试独立验证。 |
| mapoffset / chunk / stride | mapoffset 留作 metadata，不第二次加到 data.data；纯逻辑校验 plane count、offset/size、stride、每行有效字节、crop、算术及总 budget 后才复制。ring-wrapped chunk、负 stride/不支持格式等严格拒绝，不能声称已支持。 |
| 跨线程与归还 | 复制结果为自有 bytes；native pointer 不跨线程。回调在交付之前 drop Buffer，绑定 Drop 负责 queue。实际映射/归还压力与 producer 行为待 HO5 fixture。 |
| FD 归属 | Portal FD 单次由 OwnedFd 交给 connect_fd_rc；锁定绑定 into_raw_fd 后由 C core 管理。官方 API 明确 disconnect/error 自动关闭，不能额外手工 close 造成双关。实际错误、取消和断连的 FD 数量回收待 HO5 观察。[PipeWire core FD 契约](https://docs.pipewire.org/group__pw__core.html) |
| 撤销/断连 | Lease 的本地取消同步检查不等后台 actor；准备、publish、commit、AU 发出前拒绝已撤销范围。core/stream 失败关闭 mailbox；done supervisor 持 Weak 并撤销 lease；20 ms timer 处理无新帧关闭。未测 native callback/清理耗时，不能宣称物理上界或所有 FD 已回收。 |

此次没有确认需要更改 pointer/FD 所有权的额外缺陷，也没有修改这些未编译路径来猜测“修好”。队列仍只合并 raw owned 帧，不丢压缩参考帧；现有 GPUI/Ely、原生接收、输入、audio、协议、身份、签名和部署流程保持原源码范围。

## 可持续 HO5 路径

[一次性 DNS+SDK 本机草稿](HO5-DNS-SDK-ONCE-DRAFT.md) 供主会话统一审阅，复用已经部署且固定 hash 的 DNS 入口。它明确当前 NoNewPrivs 无管理员能力，说明已有 staged OS 更新与普通 package layering/reboot 前提；没有执行管理员操作、automatic reboot、容器 re-exec、来源 guard 放宽或新的 helper。

[设备池测试计划](../../testing/DEVICE-POOL-ACCEPTANCE-PLAN.md) 已登记两台 Mac、cube、HO5 和未来 Android。四台桌面设备 12 个有向组合、三个桌面 OS 的 6 个跨平台方向分别记账；扩到 Android 后为 12 个跨平台方向。窗口采集、多窗口并行、身份隔离、输入、音频、文件、真实呈现、视觉和长时性能各自留实证。离线单项未验，历史在线不等于当前可用。

接下来必须先有 HO5 本机 DNS/SDK 回执及合法最新 source delta，再验证源 hash、刷新 changed-file mtime、复用 target 做最小 host→app Linux check。不把“已有 target 中的旧构建”当作 a5e45ac 或本修复已经编译。真实 picker、MemFd/MemPtr/FD 回收、窗口 pixels、显示、输入/audio 和平台矩阵仍未验；没有发布或部署。

此前自动审批拒绝了受保护 sing-box 配置的整体读取，理由为可能读取秘密且必要性未充分说明。本轮未重复或更换路径读取该配置；只读公共 DNS、包元数据、用户目录和现有 operation。
