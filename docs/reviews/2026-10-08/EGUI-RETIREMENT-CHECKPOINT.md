# 桌面 GUI 单框架清理：检查点

## 工作位置与资源

- RemotePlay repo / shell working directory: /Users/jinliang/remote_play
- Codex task workspace: /Users/jinliang/Documents/Codex/2026-10-05/task-3
- cargo metadata --no-deps --offline 返回的 target: /Users/jinliang/rust-target。CARGO_TARGET_DIR 未设置；这是本机共享 target，不在 Documents/Codex 下。未移动或清理缓存。
- 2026-10-08 记录时，target 目录约 99 GiB。测试期间磁盘最低剩余约 4.5 GiB；安全中断本轮 Cargo 测试后恢复到约 6.6 GiB。系统拒绝读取进程列表，不能据此断言没有其他构建进程。

## 首个本地基线提交范围

- 当前基线 `408491b` 的 app manifest 已使用 GPUI CE 0.3.3 与 Yororen UI 0.2.0，且 app manifest / 根 Cargo.lock 没有 eframe 或 egui 依赖。本检查点和静态守卫测试记录并防止这条约束回退。
- 首个本地提交只包含本检查点和静态守卫测试；当前 alpha.8 桌面恢复、媒体、host、Android 与协议改动保留在工作树，不混入该提交。

## 本轮改动

- 删除 app 对 eframe 0.31.1 的直接依赖及整套 egui 诊断窗口实现；GPUI CE 0.3.3 + yororen_ui 0.2.0 成为唯一正常桌面依赖路径。
- main.rs 默认直接进入 restored GPUI UI，移除 REMOTE_PLAY_GUI_BACKEND 运行时选择；无 GPUI 的构建显式报错。保留已有 macOS legacy GPUI 启动开关。
- desktop 仅保留 GPUI 共用的会话/请求、原界面 owner/presenter、foreground runtime 与 headless 单实例守卫。移除 eframe 字体与 eframe 输入测试。单实例 renderer 身份现在默认为 GPUI；历史诊断 renderer 仍会被识别为旧进程并拒绝唤醒，避免把旧窗口当作当前界面。
- 保留 --product-info-json，移除诊断 GUI 元数据字段；桌面发布验证脚本检查 GPUI identity。image 移到仅供示例使用的 dev-dependency；删除未使用的 app 直接 libc 依赖。
- Cargo.lock 已由离线 Cargo 解析更新；锁文件没有 eframe/egui package。

## 验证结果与边界

- 通过：python3 -m unittest scripts.tests.test_desktop_gui_single_backend scripts.tests.test_desktop_gui_identity scripts.tests.test_macos_installation_diagnostics（32 项）。
- 通过：cargo tree --offline --locked -p remote_play_app -e normal --depth 1；图中有 gpui-ce 0.3.3、yororen_ui 0.2.0，没有 eframe/egui。
- 通过：Cargo.lock 搜索确认没有 eframe、egui、egui-winit 或 egui_glow package。
- vendored 私有 Cargo.lock 经官方 crates.io Cargo metadata 解析与 --locked 验证：独立副本解析到 449 个包，验证通过后将相同 lock 写回仓库；根 Cargo.lock 861 个包、私有 lock 449 个包，均没有 eframe/egui。全过程未编译，metadata target 位于 /tmp/rp-screencapturekit-metadata-final/unused-target，未触碰共享 target。
- 全仓扫描分类：app 源码没有 eframe::/egui:: widget 实现，app manifest 与根 Cargo.lock 没有 eframe/egui 依赖；仅在实例安全测试及安装诊断中保留旧 renderer 字符串，用于识别旧进程。third_party/screencapturekit 的 eframe dev-dependency 与私有锁中 eframe/egui 包已移除；无对应 example 源文件/target，库 API、许可证和其他示例未改。Cargo.toml.orig、README 与 CHANGELOG 的上游 viewer 描述保留为历史。vendor patch 原因与恢复步骤见 third_party/screencapturekit/REMOTEPLAY-PATCHES.md。
- 未完成：cargo test --offline --locked -p remote_play_app --lib renderer_identity_tests 在大量依赖首次本机编译期间因磁盘余量降到约 4.5 GiB，被安全中断（退出码 130）。没有 Rust 测试通过回执；本轮没有继续 cargo check 或 release build。
- 未部署、未推送。除本检查点与静态守卫测试外，工作树仍有广泛 alpha.8/GPUI/Android/媒体改动与未跟踪文件；未对它们整体暂存或提交。

## 可复现下一步

在确认本机有充足空间且不会与其他合法构建冲突后，复用 /Users/jinliang/rust-target（不要复制或清理它）重跑：

1. cd /Users/jinliang/remote_play && cargo test --offline --locked -p remote_play_app --lib renderer_identity_tests
2. cargo check --offline --locked -p remote_play_app --bin remote_play

随后再决定是否做默认 macOS debug build。当前默认目标是本机 macOS arm64；Linux/Windows 构建和跨设备 GUI 验收本轮未运行。未集成的 eframe 设备切换草案已于 2026-10-08 从源码树退役；原文件保存在 Git stash 959aaf051fda57f47e7c999a8d915686666da672 的第三父提交中，可用 git show 959aaf051fda57f47e7c999a8d915686666da672^3:scripts/acceptance/pending/desktop_device_switch.rs 恢复。该验收若仍需要，应另行迁移到 GPUI 路径。新增 scripts/tests/test_desktop_gui_single_backend.py 静态检查，防止诊断后端和旧源码入口重新进入 app。
