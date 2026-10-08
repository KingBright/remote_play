# 会话接线审计与当前会话选择切片（2026-10-08）

父提交：`df286ada89a43839c068f802c35472f203acaffd`。审计对象为默认产品的 `restored_ui → OriginalOwner → remote_core::session_tabs`，没有启动 GUI 或真实会话。

## 实际接线

| 实际入口 | 生产路径与纯模型决策 |
| --- | --- |
| 新窗口、设备抽屉打开视频/文件、受控测试的 silent files | 三种 Owner connect 方法统一进入 `connect_mode`；`plan_open` 决定复用、已有请求、替换或拒绝，Owner 只执行资源效果 |
| 重复打开 | 同一 `plan_open` 路径；健康旧会话复用，未完成代次不重建，失效旧会话按模型返回的索引释放 |
| 标签切换 | 类型化 Select → Owner.select → `tabs.select`；原释放输入和 talkback 效果保留 |
| 按连接 ID 关闭 | 类型化 Close → Owner.close_session → `close_index` → 原会话 drop；当前/非当前选择由纯模型决定。本轮没有新增关闭按钮 |
| 当前页断开、取消待连接请求 | UI Disconnect → Owner.disconnect_active → `tabs.disconnect`；取消只移除目标请求，模型返回关闭索引后 Owner 才删除资源 |
| 重连 | UI 的可重连投影与 Owner 的活动会话读取现在共同经过 `active_index`，再沿用原 disconnect/connect 路径和原参数 |
| Quit、Owner drop、受控测试清理 | `close_all` → `tabs.clear` → 原资源清理；代次保持递增 |
| 迟到连接结果 | `tabs.complete` 在原生会话构造前检查设备键和代次；Cancelled 分支释放返回连接并提前退出；有效结果 `attached` 决定选择。生产 UI 回调的票据仍在执行前和返回后检查 |

资源数组只在模型决定关闭/替换/附加后修改。测试专用 recorder 附加接口也调用 `tabs.select`。原媒体 poll、捕获/输入/音频/文件方法继续由 Owner 执行，本轮不迁移它们。

## 实质发现与修复

1. **取消与首个后台到达的交错遗漏。** A、B 都在连接时，A 先作为首个后台结果到达；旧 `attached` 把选择移到空位置 1，取消 B 后无法回到 A。先在父提交源码上用标准库测试复现，期望 0、实际 1，测试退出 101。现在待连接期间保留首个可用标签作取消后的备用选择，但 `active_index` 在待连接期间仍返回 None，媒体/输入不会提前路由到它。取消 B 后恢复 A；B 的迟到结果仍拒绝。前台已失败时仍保留空选择，未放松上一阶段的隔离断言。
2. **重连入口绕过过渡期限制。** UI 已隐藏过渡期的重连按钮，但 Owner.reconnect_active 直接读 selected 索引，直接调用仍可能取得旧 A。现在使用 Pool.active；Pool 的只读/可变活动会话读取和 UI 可重连投影都使用纯模型 `active_index`。过渡期、空选择和最后页关闭后返回 None，在读取参数和执行 disconnect/connect 前就拒绝。

这也是本轮最小相邻迁移：将“当前可路由会话”判定从 Owner 收进纯 Rust，只有索引和数量，没有新增锁、运行时、依赖、状态大复制或 GPU/帧对象。

## 固定回归与检查

纯模型内固定了两个新回归：取消后恢复刚到达的首个后台会话、资源路由与重连投影共享过渡期/空选择限制。

```sh
rustc --edition 2024 --test remote_core/src/session_tabs.rs -o /tmp/remoteplay-session-tabs-audit-tests
/tmp/remoteplay-session-tabs-audit-tests --nocapture
CARGO_TARGET_DIR=/Users/jinliang/rust-target cargo check --offline --locked -p remote_play_app --lib --bin remote_play
```

实际结果：12/12 模型测试通过；原独立失败复现重新编译后通过（1/1）；主包 lib/bin 增量检查成功（9.38s，现有告警）。没有重新执行其他已完成的阶段测试，没有新建或复制 target。

保留核对：105 项原有改动的状态与 SHA-256 全部保持一致。`restored_ui.rs`、原生 SessionState、original_presenter、design_system、根/app Cargo 清单和 Cargo.lock 与父提交逐字节相同。Owner 的 poll、connect_mode/代次处理、取消/断开、捕获/媒体/音频/输入/文件方法和 close_all 也逐字节相同；Owner 仅修改 Pool 活动读取与重连读取入口。可用磁盘约 5.3 GiB，高于 4 GiB 底线。

## 验收边界

本轮证明纯状态行为、实际源码接线和编译接口；没有执行真实网络重连或 Owner 原生资源集成测试。GUI、截图、实际呈现、输入转发、音频、长时性能和物理平台验收仍未测。未启动第二窗口、终止旧 GUI PID、修改配置/身份、签名、推送或部署；已安装程序没有变化。
