# 真实 HEVC、原界面与跨机阻断：本轮实现及验证

## 交付结论

本轮恢复了中断前已经落盘的 Windows 原生 HEVC 解码实现，继续完成真实硬解窗口验证、原产品字体兼容、错误可见性、源端颜色参数传递及复制池背压修复。没有将这几项局部通过等同完整跨机产品通过，没有切换默认界面、更新 NAS 或覆盖现用 alpha.7。

原版 GPUI/yororen 设计和共享 Rust 会话方案保持不变。用户已经批准的恢复范围不需要重新确认；Flutter、协议/身份、签名和移动端重写未发生。

## 真实解码帧已经进入原控件窗口

Windows 使用本地生成的 HEVC 码流，经过实际 Media Foundation 硬解、DXGI 原生缓冲和有界 GPU 复制，再由 GPUI 显示；并非上轮的预先生成 GPU 图案。每个输入片段含60帧。8位：60帧硬解、60次GPU复制完成、60个不同帧进入场景、60个不同帧被GPU绘制路径接受。10位：60帧硬解、60次GPU复制完成、60个不同帧进入场景、58个不同帧被GPU绘制路径接受。对应 window-ready-8.json 和 window-ready-10.json 均通过，没有解码/合成错误。

窗口准备使用有界握手：先真正创建并渲染窗口，再播放有限时长片段，冷启动等待单独报告（约310783/297216微秒），没有放宽原来至少55/60的验收阈值。旧测试把冷窗口/字体启动和正常播放混在一起，会在窗口存在前消耗掉前几帧。本次另统计 exact-frame was_submitted，不能只把 Render 被调用当作实际画面呈现。

此测试未测显示器扫描输出延迟，最后一次GPU完成计数可以落后提交快照。原生解码资源到共享可采样纹理有一次明确GPU内复制，不称零拷贝；没有在视频呈现路径读回整帧CPU像素。不是完整工作应用、HDR、4K/60或长稳验收。

## 复制池压力修复及六次真实队列复验

最后复测生产 ClientMediaRuntime 时发现59/60个GPU-ready输出且记录一次丢弃；硬件错误计数为0。这不是可以忽略的“先前测过通过”。源码确认：解码器批量输出后，旧实现没有及时回收已经完成的GPU复制，在有界槽暂满时会立即丢掉新解码输出。

client/src/windows_playback.rs 现在在批量输出间回收完成项，最多额外保留一个最新的、已经解码但尚未提交复制的原生帧。槽暂不可用时由已有待处理轮询重试；有更新的输出才替换这个旧的已解码帧。不会扩大原来的复制槽池，不会丢压缩参考帧，不增加CPU整帧转换。重置、取消和退出时按原生资源所有权顺序释放等待帧、复制池及解码器。

修复后的原严格测试连续运行3轮，每轮8位、10位各一次，共6次全部通过。每次都发布60个GPU-ready帧，重置后非关键帧不能绕过恢复，关键帧使代次2恢复，退出后原生线程数回到基线。latest-only消费者实际观察58、58、58、57、57、59帧，这是消费者可覆盖中间帧的语义，不把观察数量当生产者丢帧，也不当屏幕帧率。

证据：queue-bounded-{1,2,3}-{8,10}.json、copy-pressure-final-tests.log、copy-pressure-final-check.log、windows-copy-pressure-final-status.json。失败前的 production-queue-final-8.err.log 保留。最终 playback SHA-256：d66bb9a314f745883d96e3bae865e013de8e905cbd598675f19eb5b97e1b177a。

## 完整恢复版跨机测试没有通过

测试使用完整 RestoredDashboard，而不是上述专用窗口；只订阅 MacBook 上按PID、窗口ID和标题共同匹配的短时合成窗口，不回退订阅桌面、不操作工作应用、不注入键鼠。修复了 Windows 严格字体解析对 sans-serif/monospace 通用名称的失败，统一使用产品语义字体角色；保留原UI层级及等宽数字，未复制或安装字体。

未优化的完整GUI在默认1MiB Windows栈上发生溢出。一次仅用于诊断的example使用8MiB预留验证了启动；正常Release不需要该覆盖也能启动。该诊断没有写入正式产品构建配置，也不声称Debug问题已普遍消除。

最后有效Release回执 cross-macbook-release-5.json 中，设备和独立窗口选择、订阅回执已到达，但解码没有成功：decoder_status 为“source and decoder did not specify a YUV matrix”。同时记录连接对象仍存在但 peer_responsive=false，并有“plaintext packet rejected for encrypted peer”。这些观测不得合并成已确认唯一根因；本轮没有关闭加密检查、容忍未声明颜色或修改网络身份。

这次跨机运行早于后述源端颜色修复及最终复制池修复，现用Mac仍是旧alpha.7。源端修复还未部署、尚未在真实捕获源上重新跑完整跨机链路；不能报告黑屏已完全解决。即使修好颜色，心跳/P2P问题仍需独立验收。

## 源端颜色参数丢失与颜色变化处理

新增 host/src/video_color.rs，读取实际CoreVideo像素缓冲附带的primaries、transfer、YCbCr matrix，并以相同元数据配置VideoToolbox，不根据画面尺寸或NV12格式猜测、不重标记未知像素、不做CPU颜色转换。缓存相同元数据，正常帧不会重新创建编码器。

真实硬编测试揭示：仅在正在使用的压缩会话上设置新矩阵，即使属性调用返回成功，HEVC SPS仍可能保留旧矩阵。修复后的prepare_source_color在颜色变化时建立并配置新会话、完成旧输出后换代并强制关键帧；分辨率重建也清空该缓存。候选配置失败不允许部分错误标签进入活动输出。

测试通过生产submit_frame编码带明确原生附件的人工NV12帧，并解析真正输出的HEVC SPS：640×360矩阵1/BT.709，随后同分辨率矩阵6/BT.601，再320×240矩阵1；三种均保留primaries=1、transfer=1、limited-range并输出关键帧。最初矩阵切换断言失败的日志保留，修复后Host 51项全部通过（包含复用的6项SPS解析测试，不是51个全新测试）。

此结果不是对任意屏幕色彩管理、HDR或实际捕获源附件的验证。若捕获源本身没有声明矩阵，本修复不会伪造BT.709；还需要检查真实捕获配置/附件。修复尚未进入机器上的运行包。

## 构建证据错误防复发

本轮发现Windows接收的源码摘要正确，但传输保留了早于既有产物的mtime，导致Cargo迅速报告完成却没有编入新增诊断字段。通过缺少字段的实际回执、mtime及重新编译后的字段对照确认；不据此武断认为所有旧测试失效。

新增 scripts/refresh_transferred_sources.py：默认只计划；显式执行时先校验整组delta的路径、文件及after SHA-256，全部成功后只更新这些文件的mtime，不改内容、不清除所有缓存、不改变依赖。6项Python测试通过，覆盖只读计划、字节不变、全组校验后写入、无关文件保护、路径和符号链接拒绝。它不是二进制验证器，正式脚本尚未全部接入该辅助工具；后续仍应验证编译来源和运行行为。

## 最终回归与状态

- Windows：Client 33项、App 96项通过；独立GUI输入记录器1项通过；最新复制池源码再次通过Client/App及全目标编译检查。
- Linux：Client 33项、App 96项通过，独立GUI输入记录器1项通过，全目标编译检查通过。Windows专用worker及Mac专用Host修改不在Linux编译路径内。
- macOS：Client 34项通过；最终源端颜色修改后Host 51项、App 111项通过；恢复版GPUI输入专项另1项通过，全目标编译检查通过。
- source-refresh Python 6项通过。未把默认忽略的跨机测试计为通过；未声明本轮严格Clippy零警告或Android全链路验收。

所有本轮最终构建和测试已退出，合成源窗口及Windows验收程序已关闭。MacBook现用alpha.7/build20260930.7 PID96760，Mac Studio PID78134；HO5 alpha.7 active/PID1899036/NRestarts0；Windows RemotePlay退出，UU仍Running/PID27892，安装exe摘要未变。没有NAS发布、覆盖安装、权限/路由/密钥/签名更改或源码提交推送。

仍需完成：实际捕获元数据与修复包的安全验证；完整P2P/Relay跨机持续与顺序切换；Linux原生视频导入和呈现；全部原功能/输入/文件/声音清单对照；同画质CPU/GPU复制/尾延迟及长稳测试。当前不能以本机硬解成功冒充完整远控产品发布。

## 证据位置

主仓库：/Users/jinliang/remote_play。
MacBook收尾与摘要：/Users/jinliang/Workspace/.rp-real-decode-native-20261003/final-status.json。
Studio测试日志：/Users/jinliang/workspace/.rp-real-decode-native-20261003。
HO5测试日志：/var/home/liang/workspace/.rp-real-decode-native-20261003。
Windows日志：C:/Users/Liang/Workspace/.rp-real-decode-native-20261003。
final-source-hashes-complete.json记录最终修改源码；各before/delta清单保留原样，不覆写历史失败与中间版本摘要。
