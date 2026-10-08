# RP 产物生命周期接入前记录（2026-10-08）

产品基线为 `9f2f80dd636acb77065c18a8335535788b8d6a43`。用户要求已有 target、4 GiB 保护、最新两个 hash 变体、active、交付和回滚保持，主机维护优先；接口不适合旧项目时先停止接线，不修改公共 skill。当前结论：完成只读核对与项目内保护记录，机械 check/dry-run/build 接线尚未完成，不能报告生命周期已接入。

## 已核对与保留

- 已读 rust-project-lifecycle 的 SKILL.md、adoption.md、operations.md、policy.md 和当前 helper 接口；没有修改该 skill 或清理 skill。
- 本机 Cargo 1.94.1；offline/locked/no-deps metadata 表明 RP 的 build_directory 与 target_directory 相同，位于项目外的已有多项目共享根。未重扫根内大小、按 hash 猜归属、修改全局配置、移动/复制/旋转缓存或创建新 target。
- 根 Cargo.toml 没有显式 profile；现有 workspace .cargo/config.toml 的 CMake 兼容设置和 macOS linker flags 保持。未改变 debug/incremental/profile 取舍。
- scripts/test_stream_settings_model.py 及其现有 4 GiB 监测原样保留；本轮未运行 Cargo 编译。短小接口 fixture 与既有 35 项纯模型/静态聚合单列，不替代 GUI、媒体或独立 skill 安全验收。

项目内记录为 `docs/plans/ARTIFACT-LIFECYCLE-ADOPTION.json`，schema 是 adoption-plan，不是 helper 的 schema 1 policy/registry。没有创建 .rust-artifacts、.owner.json 或伪造可通过的原生管理状态。共享根只登记为 shared-dependency 引用，owned=false、delete_now=false、dry_run_candidate=false；该项目不得 cargo clean 或把整个根交给清理器。

## 具体接口缺口

| 当前接口证据 | RP 的必要条件 | 本轮处理 |
| --- | --- | --- |
| init 拒绝任何已有目录；CLI 无 adopt | 保留已有源码、AGENTS、配置与产物 | 不向 RP 调用 init，不复制初始化器 |
| load 要求 artifact_root 恰为 project/.rust-artifacts，owner 与 project_root 完全匹配；registry 只能登记该根内相对单位 | 共享根只能是非独占引用；当前/回滚发布与安装记录可能在外部 | 记录引用及保护状态，不把外部根冒称项目专属 |
| run 强制 CARGO_TARGET_DIR、CARGO_BUILD_TARGET_DIR、CARGO_BUILD_BUILD_DIR 到新专属根，verify 也另开 units/verify-cache | 保持现有 target，并复用原测试与发布入口 | 未接通 run/check/dry-run，不改原脚本或换缓存 |
| check/report 以专属根总大小进行预算和增长判断 | 多项目共享容量不能从文件名归因；长期 reserve/headroom 必须实测选择 | 项目预算、长期 reserve、增长保持 pending；4 GiB 仅是既有应急底线，不冒充通用预算 |
| 旧清理器无该 registry 的精确候选 apply API；直接 Cargo/IDE/CI 未协调 | 最新两 hash 变体、活动、current/rollback、唯一交付和未验证备份都须保护 | 只记录规则；不构造另一候选选择器/清理器，不执行 apply |

技能原文要求“现有活动共享目录只做只读接入规划”；用户亦明确要求“若接口不适合旧项目给具体缺口先停接线”。因此未用本地 wrapper 把拒绝结果伪装成 check/dry-run 成功。下一步需公共 helper 支持非覆盖式已有项目接入与 shared-reference、保留 target 的构建前后协调，以及外部交付保护的引用方式；该接口工作属于 skill 维护任务，本项目不自行修改。

## 保护与尚缺证据

保留规则同时包含旧清理器每名称/扩展名最新两 hash 变体与业务 family 最新两历史版本，两者不混为归属证明。current、必要 rollback、active、唯一交付、未知 owner/activity、源码/Git/配置/身份/用户数据和未验证备份始终保护；最新故障证据至少一份。所有已记录资源 delete_now=false，没有清理候选或可释放量估计。

本轮未枚举已安装 app、读取运行配置/密钥、查询签名私钥、访问 NAS 或重验实际交付/备份。签名包 current/rollback 的具体版本、每设备部署回执、唯一交付与备份可用性仍待所属发布流程核对；unknown-retain 不是已完成登记，也不授予删除权。没有进行 GUI、媒体、签名、部署或旧 smoke 进程操作。

## 最小验证

```sh
PYTHONDONTWRITEBYTECODE=1 python3 -m unittest scripts.tests.test_artifact_lifecycle_adoption -v
python3 scripts/tests/run_gui_slice_regressions.py --preservation-ledger PATH
```

三个 fixture 只调用已安装 helper 的 init 拒绝分支与 load：已有项目不覆盖、专属临时管理根可以读取且既有 target 配置不变、外部共享根不能冒称项目所有。不会调用 helper.run/register/rotate/check/report、清理器、Cargo 编译或真实项目初始化；TemporaryDirectory 仅回收测试自建的小 fixture。缺少安装/POSIX 环境时明确 skip，不视作通过。

本机实际结果：接口 fixture 3/3 通过（0.015s，无 skip）；既有聚合 35/35 通过，完整 ID 与前次一致，无失败/ignored/skip，两组没有重复 ID。聚合从当前生产源码重编译标准库封装，不调用 Cargo、不写共享 target。helper 源 SHA-256 为 `393fb23fb47b7f52ed0af381afc9f40240e5f21e59a0afa934a6f442a0c09806`；仅说明本次接口证据的版本，不宣称 helper 独立安全验收通过。测试前确认既有主机维护锁未占用，测试后可用空间约 22.19 GiB；没有重新扫描共享缓存或执行实际清理。

105 项原工作树改动继续按状态与 SHA-256 核对；只提交保护记录、接口 fixture 和本缺口说明。主机维护占用时让位；未清理真实产物、迁移共享缓存或扩大到新的 GUI 切片。
