# Linux portal 与 owned 帧编码接缝，2026-10-10

已完成真实 ScreenCast D-Bus 控制路径、owned YUV 到既有 HEVC 的输入分支，以及 host 的本地分享控制通道。当前仍不是可部署的 Wayland 采集后端：PipeWire worker 尚未实现，Linux 编译、实体采集、GPUI 实际呈现和 GUI 分享按钮均未验收。继续沿用 [已批准的最小方案](../../plans/LINUX-WAYLAND-CAPTURE-ADAPTER.md)，没有更换 GUI、协议、认证、签名或安装程序。

## 可核对的调用路径

`host/src/linux_portal/runtime.rs::request_local_portal_capture` 实际调用 CreateSession → SelectSources → Start → OpenPipeWireRemote。每次请求先订阅 Response，再发调用，绑定 portal 的唯一 D-Bus owner 和随机 request/session token。监听 Session.Closed 与 portal owner 变化。取消、超时、drop 立即撤销 generation，并通过专用任务有界关闭 Request/Session；Close 不等待新的 Response。受限 FD 只转交一次。multiple=false、persist_mode=0，不保存 restore_token，不选择 VIRTUAL，不打开全局 PipeWire remote。

本地 worker 获得 FD、连接和复制实际首帧后，才能调用 `PortalCaptureLease::prepare_owned_capture`。它检查格式、plane 长度、generation 及 FD 已转交状态，并用实际像素尺寸形成 Window/Display 临时 opaque ID。portal 的逻辑 size/position 不作像素尺寸、PID 或输入映射。首张准备帧在 capturer start 时保留；撤销会清空帧槽并阻止后续交付。

`host::service::run_host_service_with_portal_shares` 提供进程内通道：Targets 从现有已认证连接生成不可由远端构造的目标；Commit 再核对认证 nonce、connection、分享 scope 和 source revision。准备/取消不改变旧订阅。提交发布选定源，既有 ListSources/SwitchSource 使用该连接的目录进入 `run_streaming`。只有目标订阅可使用该 grant，supports_input=false。其他连接看不到该源，地址/昵称不能作为换连接依据。连接、订阅和本地控制通道结束会撤销各自持有的资源。

`run_streaming` 的 Linux 内部分支接 FrameMailbox → LinuxVideoCapturer → VideoEncoder::submit_frame → FFmpeg rawvideo stdin → libx265 → Annex-B AU → 既有封包/发送。没有改变公共 wire 格式。原 X11 兼容分支保持独立；它不能接受 owned 帧后将其丢弃。GPUI 当前默认 host 启动函数仍未连接这个新本地通道，不宣称分享按钮已经完成。

## 编码及颜色边界

首轮编码只声明已验证的 NV12/I420。packed RGB 的复制规则已存在，但 RGB 到 YUV 转换还未验证，编码入口明确拒绝；HDR、未映射颜色枚举、未知 range 和不足 16 像素的编码轴同样拒绝。不可编码的输出设置在停旧订阅前检查。未知 matrix/transfer/primaries 使用 codec 的 unspecified 值，不按分辨率推断。

SPA 与 AV 的颜色枚举不同，映射依据 [PipeWire 官方 color.h](https://raw.githubusercontent.com/PipeWire/pipewire/master/spa/include/spa/param/video/color.h) 与 [FFmpeg 官方 pixfmt.h](https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavutil/pixfmt.h)。[FFmpeg 的 libx265 包装](https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavcodec/libx265.c) 会为未知 range 写入默认范围，因此本轮选择拒绝未知 range；没有把默认 limited 当作采集 metadata。

未提交 raw input 只有一张最新帧；writer 已持有的帧与压缩 AU 保持顺序。压缩队列满时背压，不丢参考 AU。parser 保留同帧多个 slice，超出 AU 预算时明确失败。取消先关闭接收/输入，随后 kill/wait/join，避免满队列死锁。格式、颜色、设置变化及显式关键帧请求重启原生编码分支，以新关键帧/SPS 开始。此处不改 Windows/X11 原 reader 的旧满队列行为；不得把其旧行为算成本轮原生路径验收。

原始 SPA sequence/PTS 保留在 owned frame 与 EncodedChunk 的本地 sidecar。网络 timing 使用 host arrival 的单调时钟，未把 SPA 纳秒 PTS 强转为采集微秒。AU ready checkpoint 包括排队时间，不是纯编码 CPU 耗时；没有性能或零拷贝等价声明。

## 已运行的固定检查

所有下列检查在 Mac 的既有 `/Users/jinliang/rust-target` 中运行，仅使用合成帧、私有 dbus-daemon 和环回 socket，没有系统 portal、真实屏幕/音频采集、应用窗口或权限变更。

| cargo test -p host --lib 的过滤器，均使用 --offline --locked | 结果 |
| --- | --- |
| linux_portal::runtime::tests | 9 passed |
| linux_raw_encode::tests | 6 passed |
| linux_video_encode::lifecycle_tests | 2 passed |
| service:: | 8 passed |
| linux_frame::tests | 7 passed |

共 32 个本地分层用例，17 个新增、15 个邻近回归，0 failed。私有总线实际验证提前 Response、无 Response 取消、超时、late response、FD 单次转交、错误 source kind、Session.Closed、owner 丢失、其他 sender 伪造 Response，以及首帧像素几何/撤销。真实 FFmpeg 验证所有象限的 Y/U/V、128×64 到 64×32 的 SPS、limited/full range 样本和标记、未知颜色描述、AU 与 timing 顺序、多个 slice、满队列与取消回收。分享用例验证错 nonce/旧 revision 拒绝、保持旧订阅及连接撤销。

另通过 `cargo check -p host --lib --example linux_portal_control --offline --locked` 的 Mac 普通构建。私有总线测试需要既有 dbus-daemon，codec 固定测试需要既有 FFmpeg/libx265；不会安装依赖。Mac 测试编译兼容模块会产生非目标 cfg 的未使用/不可达警告，保留原 remote_core 的 PermissionsExt 警告。没有把 0-case 过滤结果计为通过。

`host/examples/linux_portal_control.rs` 是公开、需显式 --select-window 或 --select-monitor 的手动控制检查入口。没有参数不会调用 portal；有参数仍依赖正常系统选择及可选 parent handle。示例只检查选择和 FD/Close，不连接 PipeWire、不产生帧，不作为产品或视觉验收。此次只编译 Mac stub，未执行 Linux 分支。

## 未完成的实体边界与保留情况

HO5 的 pkg-config 仍未解析 PipeWire/SPA 开发项，PATH 未找到 clang；现有 toolbox/podman 检查没有取得合法可用的 SDK，podman 回执为 cannot re-exec。Mac 虽已装 Linux Rust target 和 zig，但不是 PipeWire/SPA/ALSA 的目标 C SDK。本机只发现现有 MySQL 容器和镜像，未进入、修改或当作开发环境。没有新建容器、拉取镜像、安装系统包、绕过 NoNewPrivs 或改系统权限。

后续仍需合法 SDK 和原来源传输恢复，才能实现并验证 connect_fd、专用 PipeWire loop、格式/metadata、MemFd/MemPtr copy 与回收。原 HO5 来源传输仍按既定 DNS/用户管理员脚本回执流程处理，本轮没有改用其他传输、重读受保护配置或 sudo。未重做已通过且未改动的 DNS/Python 检查。

实际首帧、连接视频、输入/音频、撤销/暂停、长时性能、分辨率/DPI 和 GPUI 渲染截图仍未知；新本地 API 不等于 GUI 按钮或完整产品恢复。未推送、签名、发布、安装或替换运行中的程序。

原 105 项混合改动的路径状态与 SHA-256 均保持一致。范围审计发现 rustfmt 曾递归格式化 host/src/capture.rs、capture_readiness.rs、video_encode.rs；逐字核实它们等于未变更 HEAD 的 rustfmt 输出后，已撤销这三项格式改动，不纳入提交。精确证据及本轮源码哈希见 [阶段记录](evidence/linux-portal-owned-pipeline.json)；它是本机观察记录，不是 Linux 外部验证或发布授权。
