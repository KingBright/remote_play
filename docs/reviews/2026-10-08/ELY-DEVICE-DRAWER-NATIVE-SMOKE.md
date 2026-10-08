# Ely 设备抽屉离线原生 smoke

源码基线：`5673bca206e28c006368c6683b6e11b9b87c14a5`。本轮不扩下一屏。

## 可复用入口

新增 `app/examples/device_drawer_offline_smoke.rs`。直接通过 path 模块复用生产
DeviceDrawer、DeviceList Model/VM 和原设计系统，使用固定测试设备：LAN/Relay
在线各一项、P2P 离线一项；没有另写一套仿真控件。Ely 组件/主题/焦点与产品一致。
它不创建 UnifiedRuntime、OriginalOwner、网络身份或捕获/转发，不读取 mesh/用户配置，
不修改正式安装。按钮 action/effect 只记录选择，Cancel 只清除记录。

可选择空/多项、Dark/Light；真实窗口内支持过滤、选择/取消和 Tab/Shift-Tab。
控制文件 `command.json` 位于唯一新建的输出目录，最多 4096 字节，递增 id。
鼠标和 Tab 仅经自身 NSApplication 的 NSEvent 队列，不调用全局 CGEvent 输入，
每次验证实际窗口号属于自身当前窗口，拒绝越界点。只允许 Tab/Shift-Tab 键。
界面控制及测试设备标明 offline smoke；这不是完整产品或远程会话验收。

运行方式（需正常 GUI 执行上下文；不要在本次失败上下文重复启动）：

```sh
CARGO_TARGET_DIR=/Users/jinliang/rust-target cargo build --offline --locked -p remote_play_app --example device_drawer_offline_smoke
/Users/jinliang/rust-target/debug/examples/device_drawer_offline_smoke /tmp/FRESH-ABSOLUTE-NOT-EXISTING-DIRECTORY
```

状态成功时会写 state.json，含自己的 PID/windowNumber、render 次数、DPI/geometry、
GPU 信息、mode/filter/rows、选择/action、当前焦点及明确的副作用禁用标志。
仅凭 render 次数不能证明实际呈现。AppKit 窗口正常建立时事件循环最多 180 秒；
最终 harness 另有 185 秒独立 watchdog，覆盖事件循环启动前的服务阻塞。
目前仅实现 macOS native smoke；其他平台明确返回不支持，不能算跨平台验收。

## 本轮结果

- 单目标最终离线增量 build 通过；复用现有 target，没有重复前轮 Rust/Python 测试。
- CUA 原生工具确认失败：`js execution timed out; kernel reset`，未再次请求它。
- 原生示例只启动一次。10 秒有界等待未生成 state.json；日志报告
  `Connection Invalid error for service com.apple.hiservices-xpcservice` 和
  LaunchServices `scheduleApplicationNotification` 失败。
- 因没有确认自己的有效窗口号，截图命令没有执行；没有实际截图，也没有用 mock
  或生成图片代替原生窗口。显示、主题、空/多项、焦点、禁用、过滤/选择/取消均未
  完成实际交互验收。实际 IME 候选窗口未测。
- 新建程序 PID 62929：精确可执行路径确认是本轮 smoke。SIGTERM 被系统返回
  `Operation not permitted`，未提升权限；最终是否已退出无法确认。
  该次启动是添加独立 watchdog 前的版本，不能用新 watchdog 声称旧进程已退出。
- 最终编译后未再次启动。需要恢复原生窗口工具/GUI执行上下文后再续接本切片。
  没有证据将此归因于 TCC 勾选框，也没有重置 TCC、系统安全或权限设置。

## 保留与磁盘

生产 tracked 源码无新增差异，三个复用源文件字节等于基线；105 项无关改动的
状态/哈希保持不变。仅新增 harness 与本回执，没有改 GUI 默认入口、依赖锁、
媒体、Owner、网络/签名或安装。磁盘最终约 6.411 GiB，仍高于 4 GiB 保留线。

自动审批曾拒绝直接用 HEAD 覆盖被 rustfmt 跟随引用格式化的设计文件；只读比较
确认 rustfmt(HEAD) 与 rustfmt(工作树) 相同、该文件不属于 105 项原有改动后，
逐行恢复本轮格式差异，并核验字节一致。该项已解决，原生服务限制仍未解决。
