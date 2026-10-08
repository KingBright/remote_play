# Ely 设备抽屉首片 / 单 GPUI 基线

## 决定与范围

从 `7906147a7e51b5cb257b24dd31e291607f49f2a7` 实现已获确认的首片。
固定 Ely 源码 `f756043853ca93407e2da5d07cfe520c84f90963`，保留唯一 CE 0.3.3。
本轮不是整个 GUI/全平台迁移完成回执。

| 路线 | 源码证据与本轮影响 |
| --- | --- |
| Ely 核心适配 CE，已选择 | 固定 Ely 库源码未调用 gpui_platform；选定核心只需焦点签名、BoxShadow、异步更新返回值及按钮宿主样式适配。复用现有 CE/Blade 原生资源扩展。 |
| 原生底座搬到 gpui-pre 0.3.8 | pre 将平台实现拆到 gpui-pre-platform/macOS/Linux/Windows；Linux manifest 使用 gpui-pre-wgpu，Windows DirectX renderer 为独立包。需要重新迁移原生导入、同步和真实呈现验证，超出当前最小首片。 |

证据来自固定 Ely checkout 的原 manifest/源码，以及缓存的 gpui-pre 0.3.8 官方
Cargo.toml/平台源码；不把 target cfg、一个探针或编译成功当作框架能力结论。
没有引入 gpui-pre 第二套运行时、改渲染引擎或变更原生扩展。

## 实现

- 设备目录 Model 借用已有设备/role；State/reducer/action/effect/ViewModel 保持纯数据。
  状态“Session connected”只表示会话，不声称首帧呈现。
- DeviceDrawer 使用固定 Ely Button，发出设备 ID action；原 RestoredDashboard 执行 effect，
  继续调用 OriginalOwner 的 connect_device/connect_files 和原多窗 open_device_window。
  StreamStartOptions、输入释放、文件连接与错误状态保留；不创建第二个网络运行时。
- 生产组件运行依赖从 Yororen 替换为 Ely core profile；app 与 client manifest 都删除
  yororen_ui。原样 344 项 Ely 上游文件、四个补丁文件、profile/许可证说明被纳入源码。
- 使用一个 Ely Theme 全局和根 FocusScope；产品语义主题是值快照，保留原配色与字体。
  首次窗口焦点在根，视频点击仍切换 input_focus；原 capture handler 不变。
- 保留已有标量输入 IME、UTF-16/字素/选择实现和十个原图标作为明确署名的产品适配代码，
  不称其为 Ely 原创输入。Yororen 0.2.0 的固定提交、版权、原 LICENSE/NOTICE 和原哈希
  见 INPUT-UPSTREAM.json、README.md；未通过复制改名隐去来源。
- 默认仍是 restored original GPUI。run_unified_gui 仅为同一入口别名；删除 legacy Mac
  环境入口，ui.rs/workspace_ui.rs 仅保留原设计/历史源码参考，不再注册生产模块。
  gpui-preview 仍为既有兼容别名。没有切换已安装程序或发布包。

## 验证

全部使用已有 `/Users/jinliang/rust-target`，未复制 target/仓库、未联网下载依赖、
未换默认工具链；公开固定 LICENSE/NOTICE 的读取是本轮唯一文档下载。

- `cargo check --offline --locked -p remote_play_app --lib --bin remote_play --example original_gpui_probe`：通过，macOS 当前工具链 Rust 1.94.1。
- `cargo test --offline --locked -p remote_play_app --lib desktop::device_`：5 通过；从根开始 Tab/Shift-Tab、禁用控制、列表重排与稳定目标。
- `cargo test --offline --locked -p remote_play_app --lib desktop::original_owner::restoration_regression_tests`：2 通过；迟到连接、文件身份/网络隔离。
- `cargo test --offline --locked -p remote_play_app --lib restored_ui::input_tests`：15 通过；映射、释放、捕获路由、真实源列表规则等。均为纯测试/GPUI mock，未运行真实会话测试。
- `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -v`：148 项，142 通过、6 个原生签名集成跳过。更新后的单框架防回归 8 项另核对通过。
- `cargo tree --offline --locked -p remote_play_app -e normal`：仅 gpui-ce 0.3.3 和 Ely，无 yororen_ui、gpui-pre、egui/eframe。根 lock 和 manifest 静态门禁通过。
- 426 项保护路径核验：423 项媒体/原生/协议/Owner 源码字节完全相同；另外 3 项 Blade
  文档仍为前一检查点已有的工作树 CRLF/Git LF 差异。根按键捕获与 PointerInputTracker、
  指针/按键转换/释放区块字节不变。105 项无关工作树状态/哈希相同。

暂存范围核验：394 路径与白名单完全一致，工作树与暂存 blob 一致；105 项无关改动
仍保留。`git diff --cached --check` 返回 2，仅两份原样 OFL 字体许可证的上游行尾
空格提示；为保留原许可证/hash 未改其内容。`df -k` 可用 9,307,620 KiB（约 8.877 GiB），
保留线为 4 GiB；没有低磁盘等待、清理共享 target 或启动下一屏构建。

## 未验证和后续

CE 缺少 pre 的 Role/ARIA API：Ely Button 的 role、aria_label、aria_description
明确未实现，单列为 a11y 缺口，不能宣称完整 Ely 兼容。键盘 mock 通过不替代原生
辅助技术验收。补丁与许可证细分见 REMOTEPLAY-PATCHES.md。

没有本轮实际可执行文件截图/交互、原生画面呈现、真实输入/IME/音频、长时间性能、
Linux/Windows 物理构建/测试、全组件或 Android/Web 迁移验收。原色彩/资源导入和
输入安全未被修改，但其此前结果不能自动证明新界面视觉验收。未签名、打包、推送、
部署或改变用户安装；后续仍需真实渲染与分设备验收。

完整提交范围见 ELY-DEVICE-DRAWER-PATHS.txt；只包含本轮源码、资源、归属及测试。
