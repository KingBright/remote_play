# 三片迁移收束核对（2026-10-08）

产品源码基线：`3485fa13e4863eff78fa899d984300050a3bb7ae`。三片审计未发现当前范围内的确定缺陷。本次只持久化固定聚合脚本与本覆盖说明，重跑相同 35 项后做独立本地提交；没有新增产品改动。以下是源码接线覆盖，不是完整产品、视觉或媒体验收。

## 覆盖与剩余

| 切片 | 已 MVVM 化的纯数据模型 | 已用 Ely 的控件 | 仍由产品 GPUI 代码实现 / 未迁移 |
| --- | --- | --- | --- |
| 设备抽屉 | DeviceListModel/State/VM：目录投影、四种过滤、连接状态与禁用条件；action → effect，以设备键执行 | 直接 Ely Button：过滤、Connect Stream、Files、Open multi-window workspace | 外层抽屉、卡片、徽章、标签、空态、间距与布局仍是 GPUI div；没有替换成 Ely Drawer/Card。其他抽屉页不在本次模型迁移内 |
| 会话标签 | SessionTabsState：复用/替换、容量、请求代次、取消/迟到结果、选择/关闭、active_index、标量投影；每 view 的回调票据 | session/reconnect/disconnect/quit 按钮经 command_button → product_components::Button → Ely Button | 标签行位置、胶囊尺寸与布局是既有产品样式，没有 Ely Tabs。Close action 和 Owner 接口存在，生产视图没有标签关闭按钮；不是新增关闭交互已验收 |
| 流参数表单 | StreamSettingsState：确认值、FPS/码率草稿、原校验器、预设、错误与命令/草稿票据；借用 VM；效果由 UI/Owner 执行 | Apply custom values 与分辨率/FPS/码率预设按钮经过同一 Ely 适配层 | 两项 TextInput 仍是署名保留的 GPUI 产品输入/IME 实现，未换成 Ely Input；分组、文本、错误与布局仍为 GPUI。适配/填充、暂停、音量、来源、遥测不在此纯模型迁移内 |

唯一框架仍是 GPUI CE 0.3.3 + 固定 Ely core；默认入口仍是 restored_ui::run_restored_gui。Ely Theme 与根 FocusScope 已接入，产品语义主题为值适配。Ely core 的选定组件并不等于全库迁移；CE 缺少 Role/ARIA API，是已知未实现能力，不可标为辅助技术验收通过。ui.rs/workspace_ui.rs 是未注册的历史设计参考，不是另一个生产 GUI。

## 实际入口核对

- 设备：Render 的 slide_over_management_drawer / Devices 分支 → drawer_devices_tab → DeviceListModel.project → DeviceDrawer → dispatch_device_action.reduce → 原 Owner.connect_device/connect_files/open_device_window。连接参数取流参数模型确认值，文件路径保持不启视频的既有行为，回调仍经过 view 票据。
- 会话：Render → session_switcher → Owner.session_tabs → tabs.project；标签选择、重连和控制岛断开进入 dispatch_session_action。Owner 的 connect_mode/complete/attached、select、close_session、disconnect_active、reconnect_active/Pool.active 与 close_all 仍执行真实资源效果；active/active_mut 与重连投影共同经过 active_index。模型的连接标志不是已显示帧证据。
- 表单：media_controls 读取 project 草稿/错误，两项输入、Apply 与三组预设派发类型化动作；dispatch_stream_settings_action 写原偏好并调用原 update_stream_settings。执行前核对当前命令和既有连接对象，错误返回核对连接与票据。新连接、来源切换和预设高亮读取确认值；草稿编辑不写实际偏好。此为接线核对，尚不证明远端参数实际生效。

源码入口：`app/src/desktop/device_list.rs`、`app/src/desktop/device_drawer.rs`、`app/src/desktop/original_owner.rs`、`app/src/restored_ui.rs`、`remote_core/src/session_tabs.rs`、`remote_core/src/stream_settings.rs`；按钮适配在 `app/src/product_components.rs`，保留的输入在 `app/src/product_components/text_input.rs`。这些均未被本次脚本/说明提交修改。

## 固定回归与去重计数

脚本：`scripts/tests/run_gui_slice_regressions.py`。从脚本位置自动定位仓库，没有固定 HEAD/分支、用户绝对路径、凭据或对旧临时文件的必需依赖。以系统 tempfile 新建输出目录并保留日志，不写入仓库或共享 target，不清理目录。它只编译标准库测试封装，path 引用当前三个生产模型；AppDevice、DiscoveryScope、Role 类型和协议校验函数均从当前生产源码逐字提取，未使用替代实现。没有调用 Cargo、旧测试二进制、GUI 或网络。该封装验证模型行为，不能替代完整 crate/Owner 原生资源集成测试。

从仓库中运行：

```sh
python3 scripts/tests/run_gui_slice_regressions.py
```

标准输出给出临时输出目录，包含 receipt.json、编译/测试日志和源封装。脚本核对运行前后 HEAD、分支、空 index、index.lock、工作树状态和列明生产源码/脚本的 SHA-256 稳定性；主进程不会读取未列明的配置或密钥。临时目录与仓库所在卷均守住 4 GiB，单个自建子进程有 60 秒上限，只在超限时停止该子进程组，绝不触碰旧 smoke 或其他现有进程。

如已有明确授权的保留账本，可追加 `--preservation-ledger PATH`；JSON 以仓库相对文件路径为键，值包含 status 和 sha256，拒绝绝对/越界路径。该选项才逐文件核对账本内容，不把仅工作树状态相同当作未知文件内容相同。本次使用既有账本核对全部 105 项原改动，但未把账本或用户临时路径提交到仓库。没有账本时仍可独立运行相同 35 项。

| 本次独立 Test ID | 通过 | 失败 / ignored / skip |
| --- | ---: | --- |
| device_list 当前纯模型 | 3 | 0 / 0 / 0 |
| session_tabs 当前纯模型 | 12 | 0 / 0 / 0 |
| stream_settings 当前纯模型 | 4 | 0 / 0 / 0 |
| 单框架源码/清单门禁 | 8 | 0 / 0 / 0 |
| 产品身份校验器 fixture（非实际二进制验收） | 8 | 0 / 0 / 0 |
| 合计，按完整 ID 去重 | **35** | **0 / 0 / 0** |

Rust 的 19 个 ID 逐项以 --exact 运行，各自输出的其余 18 filtered 不累加为跳过。Python 16 项无内置 skip。完整 ID、各项日志、编译来源哈希及结果在脚本打印的临时目录 receipt.json 和日志中。最终持久化脚本的完整 35 个 ID 与前一临时脚本一致，未新增用例或产品变更。

前序会话 10 项已经包含在当前 12 项内；独立后台到达复现已进入模型回归，不再额外 +1。前序设备 5 项中的 3 个模型在此重跑，2 个 GPUI mock 本次没有执行。前序流参数 4 项、Rust 输入 15 项、Owner 2 项及 Python 148 项历史结果均不叠加进 35，也不称为本次通过。签名集成此前的 6 个 skip 没有被这次测试消除；本次根本没有加载签名集成套件。

本轮未跑 GPUI mock/native 窗口、Owner live/真实资源测试、宽范围 release/signing 测试、Cargo check/build。前一轮最终主包 lib/bin check 成功（13.21s）的三项源码哈希仍与当前 HEAD 匹配，仅沿用那条编译证据，不把本次标准库封装称为新的主包检查。

## 必须以真实程序继续验证的项目

| 验证面 | 下一步具体证据 |
| --- | --- |
| 设备抽屉 | 恢复正常原生 GUI 上下文后，从实际可执行文件取得空/多设备、四过滤、在线/离线、连接中/失败、Dark/Light 的截图与点击结果；Tab/Shift-Tab 跳过禁用项，重排后设备目标仍正确；确认旧 smoke 状态，勿重复启动第二 runtime |
| 会话/资源隔离 | 实际 A/B 切换、重复连接/复用、取消 B 且 A 首个后台到达、失败/迟到回调、断开/重连及关闭接口的资源释放；输入、音频/talkback、文件和剪贴板不得路由到旧设备，连接健康/代次不能只看 UI 标签 |
| 流参数/IME | 真实 FPS/码率输入、光标/选择、粘贴、中文/组合输入与重绘保持草稿；无效输入、三组预设、错误回执、新草稿/新命令/会话切换交错；连接中 Apply 与连接完成交错时参数是否实际生效，以及再次启动后的原偏好字段 |
| 原生媒体与输入 | 分开记录源枚举、压缩码流/颜色 metadata/SPS、解码输出、实际显示帧和输入响应；参数/源更改后的 encoder generation、源 revision、旧帧隔离、背压保留最新解码帧、GPU 原生资源导入/同步；不能以提交给 UI 代替显示 |
| 功能与性能 | 音频/静音/音量、暂停/后台恢复、talkback、文件传输身份、剪贴板、多窗/PiP/全屏、错误覆盖层；在画质与功能保持的前提下测延迟、帧率、CPU/GPU/内存、长时稳定性，不仅报 decode FPS |
| 物理平台/视觉 | MacBook、Mac Studio、HO5/cube 等实际 macOS/Linux/Windows 设备分别跑窗口/全屏、代表性分辨率与 DPI、键鼠/IME、原生媒体和失败恢复，保留原设计的前后截图；键盘 mock 不替代原生辅助技术，Role/ARIA 缺口需明确处理 |

旧离线 smoke 的 CUA/LaunchServices 失败仍是未解决的原生窗口执行边界。本轮只读查询 PID 62929 的可执行名和 UTC 启动时间，未查询完整参数：device_drawer_offline_smoke、2026-10-08 05:17:24 UTC，均匹配旧启动记录，进程仍存在，GUI 前置清理尚未解除。没有尝试终止、重新启动或归因于权限勾选。此为进程身份核对，不证明窗口存在、显示或交互成功。表中项目均未因模型通过而转为绿色；签名/安装与实际运行产品版本另行验收，本次没有发布或安装行为。

## 保留证明

最终脚本运行前后核对 105 项原工作树状态与 SHA-256 全部一致，列明生产源码/脚本在编译和测试期间保持稳定。独立提交只纳入本说明和 scripts/tests/run_gui_slice_regressions.py，不提交临时产物、账本、私有配置或其他工作树改动。未改依赖、默认入口、协议/配置/身份、原生媒体/输入或签名；未联网、未启动 GUI、未清理、未推送或部署。本轮保留至少 4 GiB 磁盘；不扩展新的 GUI 切片。
