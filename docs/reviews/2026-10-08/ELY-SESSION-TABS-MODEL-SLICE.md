# Ely / GPUI 会话标签纯模型切片（2026-10-08）

## 范围与接线

本阶段延续单一 GPUI CE 0.3.3 / 固定 Ely 基线。父提交为 `cb81eaab36200a87ab99c65b7a5193ab1f60db89`。只提取会话标签、连接意图、请求代次与标量投影；原 Owner 在既有锁内持有并执行真实连接、媒体、输入、音频、剪贴板和文件资源效果。

| 文件 | 职责 |
| --- | --- |
| `remote_core/src/session_tabs.rs` | 纯标准库状态、打开/复用计划、完成代次检查、选择/关闭规则、标签状态投影、动作/效果、每个 view 的异步回调票据及 10 个测试 |
| `remote_core/src/lib.rs` | 导出跨平台纯模型 |
| `app/src/desktop/original_owner.rs` | 以纯模型替代原有标签状态字段；资源创建、复用、释放和原生帧仍由 Owner 执行 |
| `app/src/desktop/mod.rs` | 移除已迁移的重复复用/关闭选择辅助函数 |
| `app/src/restored_ui.rs` | 消费标签投影、派发类型化动作；新窗口、设备连接、断开和重连回调使用票据隔离迟到状态 |

没有增加运行时、全局锁、绑定 DSL、GUI 或其他依赖。模型只观察连接 ID、设备键、名称、连接/确认/媒体存在等标量；不持有视频帧、纹理、GPU 句柄或网络执行器。连接状态不是实际画面呈现证据。原标签尺寸、胶囊样式、位置以及重连/断开按钮视觉树保持原样；本阶段未新增关闭标签按钮，关闭动作保留为 Owner 接口。

## 行为

- 同一健康会话直接复用；断开、无响应或视频失败的旧会话按原资源释放路径替换。已有未完成请求不重复创建，重新选择它时跟踪其现有代次。
- 切换会话仍释放原输入、停止原 talkback。关闭非当前页保持当前页；关闭当前页选相邻页；最后一页关闭后没有活动页。
- 取消正在连接的 B 不删除已有 A。重连沿用原 Owner 的设备键和宽高/FPS/码率参数。
- 同设备取消后重开、清空后重开均使用递增代次，旧结果无法清除新请求。后台结果不能抢占新的连接意图；新连接失败留下空选择时，迟到的后台追加也不能恢复一个无关页面。
- 容量拒绝保留此前连接意图。迟到的后台失败不覆盖 Owner 当前消息；旧 view 回调不覆盖后来选择/重连的状态，已过期且尚未执行的 UI 连接任务也不执行效果。

## 轻量验证

从本轮最终源文件直接重新编译标准库测试程序，编译成功后才运行：

```sh
rustc --edition 2024 --test remote_core/src/session_tabs.rs -o /tmp/remoteplay-session-tabs-tests-final
/tmp/remoteplay-session-tabs-tests-final --nocapture
CARGO_TARGET_DIR=/Users/jinliang/rust-target cargo check --offline --locked -p remote_play_app --lib --bin remote_play
```

结果：10 个纯模型测试通过；最终主包 lib/bin 增量检查成功（1m43s，现有告警）。本次不重跑之前已完成的阶段测试。最终纯模型源 SHA-256 为 `29a6f40ef35a71592f99e8e1b6c8eba4dd2af0a1c00df03efa623bb405cc00a1`。测试产物仅在 `/tmp`，主包复用现有 target；检查后可用磁盘约 5.36 GiB，保留超过 4 GiB。

源码保留检查确认：105 项原有混合改动的状态和 SHA-256 全部一致；根 Cargo.toml、Cargo.lock、app/Cargo.toml、原生 SessionState、original_presenter 和 design_system 与父提交逐字节一致。Owner poll 只替换纯状态读取表达式；原捕获、媒体、音频、输入和文件方法逐字节一致；重连参数构造在替换选择读取表达式后与原文一致；视频 canvas 到浮动控制岛前的区域逐字节一致。

本地执行通道中断期间通过 Remote Hosts 只读核对原路径与本轮临时记录，确认是同一 MacBook 工作树后完成剩余验证。Mac Studio 原路径不存在，未作修改。未复制仓库或 target，未读取密钥、修改配置、签名、推送或部署。

## 未测边界

本轮没有启动 GUI、真实会话、捕获或输入转发，也没有重试旧 GUI PID 的终止动作。真实截图、交互、呈现、长时性能、远端重连、音频及物理 Windows/Linux/Android 验收仍未测；纯模型测试、源码保留和编译通过不能替代这些验收。已安装程序没有变化。
