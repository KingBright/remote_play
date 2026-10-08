# GPUI / MVVM 源码阶段检查点

## 实际提交基线

本检查点接续 `d28f25a5f94f2ae8c8b6554ef61cc0f9cb4f7e5f`，分支为
`codex/gpui-mvvm-migration`。该父提交仅包含
`EGUI-RETIREMENT-CHECKPOINT.md` 与 `test_desktop_gui_single_backend.py`，
没有提交 GUI 或共享运行时实现。开始核验时暂存区为空，`index.lock` 不存在。
父会话随后确认旧任务仍已完成，由本任务唯一负责阶段提交。

## 本次源码范围

- 提交当前默认 GPUI 入口、编译产品身份、设计系统、原界面、桌面实例所有权、
  会话模型及输入/呈现适配。`restored_ui.rs` 虽原为未跟踪文件，已被默认入口和
  库引用，不能从可构建基线排除。
- 保留已经接入 GPUI 的设备列表 MVVM：纯 `DeviceListState`、action、effect、
  ViewModel 及现有单元测试。effect 仍交给原 `OriginalOwner` 执行。
- 提交此入口所需的客户端/主机媒体、源选择与输入安全、共享文件、寻址 relay
  及共享 bridge 源码，避免源码提交只包含界面而遗漏编译依赖。
- 提交根锁文件、相关包 manifest、Linux 原生 ABI wrapper、明确注册的 GPUI
  示例及所引用的测试 fixture。HEVC 文件是源码测试输入，不是产品构建产物。
- 提交根 manifest 引用的完整本地 patch 包及原始来源记录：GPUI CE 0.3.3、
  Blade 0.7.1、ScreenCaptureKit 1.5.4。337 个 vendor 文件约 13.3 MiB；
  保留包的源码、资源、许可证、manifest 和锁文件，没有 target 或二进制产物。
- 提交 GUI 身份验证脚本及其测试，继续拒绝历史 egui 诊断产品身份。

完整源码路径见 [GPUI-MVVM-SOURCE-PATHS.txt](GPUI-MVVM-SOURCE-PATHS.txt)：
共 431 项，其中非 vendor 94 项。两个检查点文档另计。
Android UI/JNI、NAS 页面、发布/签名脚本、其他探针、历史验收资料、缓存及
未纳入的工作树改动保持原样；没有全仓 add、推送、发布或部署。

## 验证与边界

续接信息已有主包 `cargo check`、3 项 Rust 测试、32 项 Python 测试通过回执，
以及 vendor `cargo metadata --locked` 449 包回执。本轮沿用这些前序结果，
没有重复执行已通过的测试，也没有启动新构建或新 target。
根 Cargo.lock 为 861 包、ScreenCaptureKit 私有锁为 449 包，重新只读核验均无
eframe/egui 系列 package。暂存范围核验另检查文件完整性及源码字节一致性。

本轮新增只读核验：`cargo metadata --offline --locked --no-deps` 成功，
target 仍为 `/Users/jinliang/rust-target`；全部明确注册的 target 和本地 patch
路径均已纳入暂存树。433 项暂存路径与白名单一致，105 项未纳入改动的状态
保持原样。实现源码字节未改；Git 仅对三个上游 Blade Markdown 文件规范化
CRLF。`git diff --cached --check` 返回 2：现存输入测试的空白字符串和上游
vendor 文本存在空白提示，未为通过该检查而修改测试输入或上游来源文件。

本地源码检查点不证明 Linux/Windows 构建、真实画面呈现、输入、音频、性能、
长时间运行或部署验收。尚未迁移到 Ely；后续从这份单一 GPUI 基线继续，
不并行维护 CE/pre 两套产品界面，不移除原生视频或输入能力。
