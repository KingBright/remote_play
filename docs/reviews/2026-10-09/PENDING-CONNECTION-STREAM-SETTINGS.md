# 连接期间的串流设置提交

2026-10-09，基线 `1c7b35e3b42ead791fad4d76150ab05b41e3f71e`。

本轮修复产品链路中的一个具体缺口：选择设备后，连接握手尚未结束时修改分辨率、帧率或码率，表单和偏好已经保存，但旧控制器因没有活动连接而拒绝更新。握手完成后仍使用点击设备时捕获的旧值。原适配器在这一阶段以 `None == None` 比较连接，也不能区分两次不同设备的连接意图。

## 实现

- `remote_core/src/stream_settings.rs` 增加纯 `PendingStreamSettingsState`，按设备 ID 和连接代次保存最新已提交值。草稿不进入请求；旧代次不能更新或取走新代次的数据。
- `OriginalOwner` 在原有 Pool 锁内登记请求、同步接收表单提交，并在握手完成、创建媒体订阅前取出最终设置。失败、取消和全部关闭都会回收对应设置；设置缺失时拒绝接入，不默默回退到旧值。
- GPUI 适配器在发送异步任务前准备更新。连接中的提交直接存入对应请求；已连接的更新持有该连接的弱引用，控制器在同一锁内检查具体连接对象并修改其来源请求，避免检查与执行之间切换到另一设备。
- 设备连接任务开始时读取当前已提交设置，不在点击时冻结参数，覆盖点击到异步任务调度之间的设置提交；随后由按代次的待连接状态接管。
- 保留现有表单 receipt、错误反馈、协议验证、输入释放、源列表校验、订阅流程、音频参数及原生媒体缓冲路径。固定 GPUI/Ely、默认入口、线上协议、持久配置格式和依赖均未改变。

本轮涉及上述两个 Rust 文件、GPUI 适配器、既有小回归入口和本记录，另有阻塞 app 测试编译的单行导入修复，共六个文件。

## 验证

```sh
python3 -B scripts/tests/run_gui_slice_regressions.py \
  --preservation-ledger /tmp/remoteplay-ely-preservation.json
```

退出 0，38 项独立测试通过，无失败/跳过：Device 3、Session 12、StreamSettings 7，以及 GUI 依赖和产品入口 Python 检查 16。新增三个生产模型用例覆盖已提交值/未应用草稿、设备/代次隔离，以及请求消费/取消/清空后的拒绝更新。

标准库测试包装器引用当前生产定义和模型模块，不使用替身模型或第二 GUI。最终结果和源码摘要位于 `/var/folders/q_/0bykfl6x79zgp4fkvht8ddmc0000gn/T/remoteplay-gui-three-slice-i42vi0vu/receipt.json`。

首次控制器测试在 95.806 秒后退出 101：旧 `original_owner_live_tests.rs` 使用了 `BTreeMap`，但其父模块在先前迁移中不再导入该类型。即使该真实网络用例被忽略，Rust 仍会编译它，导致全部 lib-test 编译失败。本轮为该文件补显式导入，未执行其网络用例；首次失败日志和回执保留为 `/tmp/remoteplay-pending-settings-owner-tests-first-failure.log`、同名 `.json`。实际重试命令为：

```sh
cargo test --offline --locked --target-dir /Users/jinliang/rust-target \
  -p remote_play_app --lib \
  desktop::original_owner::restoration_regression_tests -- --nocapture
```

最终测试退出 0，耗时 7.993 秒，app lib-test 和实际 GPUI 适配器完成编译；3 项控制器测试通过，0 失败，0 忽略，102 项被筛选掉。最终调度窗口修复后重跑了上述两组，本轮共 41 项独立测试通过，重复运行、首次失败和历史结果均未重复计数。编译期间相关源码摘要保持不变。

该测试只执行内存中的 Pool 和既有文件视图身份断言，不启动 GUI、发现/主机/客户端网络服务、采集或输入。控制器用例验证无活动连接时提交到正确的待连接设备，以及非法设置和缺失请求代次的拒绝行为。日志和退出回执位于 `/tmp/remoteplay-pending-settings-owner-tests.log`、`/tmp/remoteplay-pending-settings-owner-tests.json`。

## 环境和证据边界

真实仓库为 `/Users/jinliang/remote_play`，分支 `codex/gpui-mvvm-migration`。原有 105 项改动的状态和内容摘要在修改前及模型验证后完全一致。没有暂存无关文件、推送、发布、安装或创建 GitHub Actions。

当前事实查询显示旧 smoke PID 62929 不存在，开始前没有活动 Cargo/rustc/RemotePlay 进程或主机维护锁。当前 Cargo metadata 的 target 和 build 都是 `/Users/jinliang/rust-target`；构建复用该目录，保留原 Cargo 参数/config/profile，并监测主机维护锁和 4 GiB 应急保底。重试期间另一个 `remote_cockpit` 构建持有共享产物锁，本轮没有干预它；准备结束自己的等待任务时，事实核实发现本轮 Cargo 已自行完成，因此没有发送终止信号。未清理或搬移缓存，未使用新项目生命周期 `run` 强制更换 target。

生命周期只读接入仍不能代表产品完成。上述结果证明模型和控制器范围；未实测原生窗口操作、远端握手到订阅、真实呈现、输入、音频、性能或各平台长稳，也不证明已安装程序发生变化。

## 链路盘点与下一步

设备筛选/选择动作已有纯模型和 GPUI 接线，连接已有路由尝试、代次取消及设备隔离，串流表单已有校验与异步错误 receipt。本轮补齐了连接握手期间的设置交接。

下一块明确缺口是首次连接失败后的直接重试：`SessionTabsState::complete(..., false, ...)` 保留空选择以避免恢复旧设备画面，但不保留可重试目标；`can_reconnect` 仅对活动连接开放，因此失败后没有直接 Reconnect 入口，只能重新从设备抽屉选择。后续应保留与当前网络范围和意图绑定的失败目标，重试时使用当前已提交设置，并验证取消、切设备和迟到错误不能复活旧目标；原生 GUI 的按钮和错误恢复交互需另外验收。
