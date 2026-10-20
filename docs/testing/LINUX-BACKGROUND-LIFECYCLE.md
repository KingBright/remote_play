# Linux 同身份后台服务 / GUI 分离方案

## 已安装版本的同身份更新

原 `install_native_first_identity.py` 的显式 `--linux-update` 入口使用
`linux_current_update.py` 更新已经存在的独立 GUI 服务。它不切换后台角色，
不修改 unit、drop-in、自启动意图、桌面条目或配对文件，也不重新编译。

先用 `--linux-update --prepare-update`，同时传入独立核验的
`--build-receipt`、`--build-receipt-sha256`、`--compiler-binary`、
`--expected-commit`、`--identity-sha256`、`--old-binary-sha256`。
入口验证原安装 journal、release manifest、实际 `/proc` 映像及空闲会话，
通过原 `build_identity_gate.seal_tree` 封装现有编译产物，并在原私有 journal
的 `linux_update` 中记录计划。只返回不含配对内容的摘要；真实计划不提交到仓库。

准备完成后，审阅计划摘要，再使用同一入口的
`--linux-update --apply --plan-sha256 REVIEWED_SHA`。安装会重新核对 inode、
哈希、权限、xattr、服务策略、profile 和 owner generation，只停止已核验的原服务；
原子交换应用目录后保留原 inode 到原事务目录的固定 `previous-app`。
原 release manifest 和首次安装证据保留。成功必须有新 `/proc` 映像、
连续稳定的 owner/GUI 状态和未变化的配对文件，不能只凭服务 running 宣称完成。

未知、已有或中断的更新不会重新准备或重复 apply。用原计划摘要及
`--linux-update --recover-update --plan-sha256 REVIEWED_SHA` 检查并恢复原映像；
外来文件、配置或身份漂移会保留现场并拒绝恢复。一次更新保留一份必要的安装回滚，
不产生分发归档、历史成品代数或另一套发布登记。后续更新需先处理原事务和回滚归属。
已处于后台分离模式的安装不适用此入口。实际设备发现、扫码、视频呈现和输入验收
仍须分别完成。

若新 owner 已通过实际映像核验，随后仅运行时资源配置
`/run/user/UID/systemd/user.control/weavelo.service.d/50-CPUWeight.conf`
变化而触发保护，可先独立读取其准确内容及当前配置摘要，再显式使用
`--linux-update --reconcile-update --plan-sha256 REVIEWED_SHA
--configuration-sha256 OBSERVED_SHA`。此步骤只允许该文件的 CPUWeight 差异，
再次核对原 owner、配对文件、主 unit、其他 drop-in、新旧映像和 manifest；
它不停止或启动服务、不交换程序、不写任何资源配置。原失败记录和计划保留，
审阅后的配置与执行器摘要写入同一 journal，不能用此入口接受其他配置漂移。
配置差异与写入者归因分别记录，不能从该文件名或启动时间推定具体写入者。

`scripts/linux_background_lifecycle_plan.py` 是原安装流程的纯方案工具，复用 `background_lifecycle_plan.require_separated_entries` 的产品能力检查。没有 apply、systemctl、子进程、profile 读写或凭据创建入口。它只生成一份内容寻址的私有方案及可选回滚预览，不是新安装器或发布权威。能力 JSON、声明的 PID 和输入 inode 都不是原生身份或归属证明；真实操作仍须由原生产者重新核验。

## 具体变化

现有 Linux user unit 以裸可执行文件及 `WEAVELO_HEADLESS=0` 启动单进程 GUI/运行时。拟议变更只替换两行：

```ini
ExecStart=/home/example/.local/opt/weavelo/weavelo --background-service
Environment="WEAVELO_HEADLESS=1"
```

其余 unit 字节保持原样，继续使用原 `weavelo.service`、同一可执行文件和 `~/.config/weavelo/mesh`。原 enabled/disabled 意图保持，未启用的服务不能自动 enable。桌面条目和安装目录不变；已有默认入口在原产品创建后台角色标记后附着后台，显式 GUI 命令为同一可执行文件的 `--gui`。

方案固定原 unit SHA256、device/inode、uid、mode、links；固定同一 profile 目录身份及现有 mesh.conf / mesh.secret 的 device/inode、uid、mode、links、SHA256。只接受目录 0700、两个现有普通文件 0600；不修权限、不复制或导入认证数据。原源码 commit 和二进制 SHA 也被固定，但此工具不重新编译、包装或部署它们。

仅支持原安装器的窄 unit 格式。重复指令、额外 ExecStart、EnvironmentFile、ExecStartPre、User、未知环境、转义/续行、未审阅的 drop-in、未知角色标记、非 Standalone owner、活动/待定会话或租约都会拒绝。配置差异需要原维护者另行审查，不能靠删掉字段绕过。

## 可复现隔离回归

从原 main 工作区运行：

```sh
python3 -B -m unittest discover -s scripts/tests -p test_linux_background_lifecycle_plan.py -v
./scripts/test_release.sh
```

这些测试只使用固定测试哈希占位值、合成 observation 和自有临时目录。CLI 回归只创建一份 0600 proposal，验证已有 unit/facts 字节未变，第二次输出被拒绝；不启动 GUI、运行时、系统服务，不创建任何有效网络身份或凭据。

生成真实目标的只读方案时，先由已有原生验证与原安装 journal 采集同一时点的事实。`facts.json` 必须包含：

- `home`、`binary_sha256`、`source_commit`、既有 `product_info` 能力字段。
- `unit_state`：原 unit 的 device/inode、uid、mode、links、sha256。
- `profile_state.directory`：目录 device/inode、uid、mode；`profile_state.files` 只含 mesh.conf 与 mesh.secret 的上述文件元数据和 sha256，不含文件内容。
- `observation`：Standalone owner 的 pid、generation、binary_sha256，精确 active_sessions / pending_sessions 整数 0、active_lease=false、drop_ins=[]、unit_active=true、原 unit_enabled 布尔值和 marker_present=false。

在已有私有目录中创建小方案；不会更改 unit 或 profile：

```sh
python3 -B scripts/linux_background_lifecycle_plan.py \
  --unit /private/review/weavelo.service.before \
  --facts /private/review/facts.json \
  --output /private/review/linux-background-plan.json
```

路径仅为占位示例；使用实际已授权用户的私有目录，不能读取其他用户文件。输出父目录须为当前用户私有目录，输出文件须不存在。stdout 仅显示方案摘要和路径，原 unit 与认证元数据留在 0600 方案中，不能将该真实方案提交到公开仓库。

## 后续真实操作所需顺序（此工具不执行）

1. 由原安装生产者在原 journal 中绑定此次生命周期子事务和 reviewed plan SHA。重新核验已安装映像、原 profile 与 unit 的字节、inode/权限、原服务状态、精确已审阅 drop-in、标记不存在、无会话/租约；未知或漂移则保留原状。再次确认现有认证对仍存在，不能把缺文件解释为首次生成身份。
2. 只停止已核验的原 unit / PID，等待该实际 owner 退出并确认原 profile 锁与 IPC 可用状态。不能按进程名批量终止，也不能停止其他任务的会话。
3. CAS 替换同一 unit 的上述两行，保留原 mode、ownership 和其他元数据，沿原机制 reload / start。只保留原 enabled 意图。由同一原生产者记录新 unit 的实际 inode 与 SHA，不能用相同内容推断文件归属。
4. 原 `--background-service` 使用同一 profile。核验实际运行映像、OwnerMode::Background、原认证对不变及角色标记的 uid/mode/零长度/device/inode；将该标记的真实归属证据写入原事务。原生入口创建的 `background-service.enabled` 是持久角色标记，不是凭据；本工具从不创建它。
5. 用同一可执行文件 `--gui` 附着既有后台；只对自有 GUI 做实际关闭/重开验收，证明后台 PID/generation 持续存活，后台崩溃恢复时不会生成第二个 GUI/网络 owner。服务配置正确不能替代这项原生测试；视频、Portal 和输入验收仍分别记录。

## 回滚边界

`rollback_preview` 要求外部固定的 plan SHA、原 unit bytes、当前 after-unit bytes 及原生产者绑定的 after-unit inode/metadata；同时要求原认证元数据不变、确认后台已停且本次附着 GUI 全部退出。

如果后台创建了角色标记，只接受与原事务记录相同的路径、device/inode、uid、0600、单链接、零长度和空内容 SHA。相同空内容的外来 inode 不能删除。先移除这一个已证明归属的标记，再恢复原 unit；只恢复 unit 而留下标记，会使默认入口继续要求附着后台，从而破坏单进程回退。原私有 IPC 的活跃/陈旧判断继续用既有产品逻辑，不能手工批量删除 socket。

若后台在创建标记之前就失败，只有 `owned_marker=None`、`current_marker=None` 且 `marker_absence_verified=true` 的已验证缺失分支允许恢复 unit，不执行任何标记删除；未知缺失不能等同不存在。

预览返回要恢复的原 unit 字节、mode、原 enabled 意图及精确 marker/unit CAS 条件，始终 `applied=false`。它不执行停止、删除或重启；真实执行器现已接入下述原 journal 子事务；真实设备验收单独记录。本阶段不会触碰当前 HO5 服务、Windows ACL、被拒的源码上传、共享缓存或成品。


## 原安装 journal 执行器

`scripts/execute_linux_lifecycle.py` 使用既有 `~/.local/state/weavelo-identity-migration/journal.json` 的 `linux_lifecycle` 子对象和同目录固定锁。保留原顶层安装 phase/字段，不创建另一套身份事务。仅支持原 observed receipt 验证的固定已安装二进制/source；不会编译、安装应用或生成 profile。切换前必须重新生成并审阅实际方案，包括 `unit_security_sha256`：对 unit 的 uid/gid/mode 及每个 xattr 值的 SHA256 映射做规范 JSON SHA256。旧的纯预览方案可缺该值，但不能用于真实执行。

在已批准且未被拒绝的原部署路径交付最终固定源码后，原生 Linux 命令为：

```sh
python3 -B scripts/execute_linux_lifecycle.py --plan /private/review/linux-background-plan.json \
  --expected-plan-sha256 REVIEWED_PLAN_SHA256 --apply
```

发现中断时先读取原 journal；同一 `--apply` 不重放未完成动作。明确恢复仍使用同一计划与原 journal：

```sh
python3 -B scripts/execute_linux_lifecycle.py --plan /private/review/linux-background-plan.json \
  --expected-plan-sha256 REVIEWED_PLAN_SHA256 --recover
```

命令中的路径与摘要是占位符，不能直接用于设备。已 committed/rolled_back 的重复请求只复核终态，既不再启动 GUI，也不重做切换。不同 plan SHA、未知 journal/unit/marker/前台锁或配置漂移会拒绝。unit 通过 Linux `renameat2(RENAME_EXCHANGE)` 保留原 inode，固定 pending 保存本次小 unit 及恢复证据，不复制应用或缓存。不支持的安全元数据或需要额外权限时在原服务停止前失败，保留原状。

恢复只使用先前已持久化的后台 generation、marker inode 和 GUI PID/start ticks。使用 pidfd 固定 GUI 进程后才允许终止本次已证明归属的 GUI。后台启动结果未知且 marker 尚无已验证启动回执时，标记即便为空也保持未知，不能倒推归属删除。已存在或锁定的其他前台不能被关闭。中间意图和小 pending 留在原事务归属下，未知文件不清理。

正常路径用安装的同一二进制 `--gui` 与已有五秒 acceptance hook 创建本次 GUI；验证 `/proc` 映像、实际 render receipt、正常退出，再核验后台 PID/generation 仍相同及原 profile 不变。这个自动定时关闭验收不是手动点击关闭、登录/重启或长时稳定性验收，不能替代后者。

隔离验证：

```sh
python3 -B -m unittest discover -s scripts/tests -p test_execute_linux_lifecycle.py -v
```

状态机故障注入不启动进程；存储测试用真实自有临时文件检查 journal CAS、stage、inode 与锁，Linux xattr 和 pidfd 系统边界在 macOS 上使用明确 mock。它们不能替代 HO5 原生 `renameat2`、systemd 和 GUI 验收。本阶段没有实际切换 HO5；若源码交付被平台拒绝，保持原操作并停止生产应用，不能换通道绕过。

前台锁的 Unix `flock` / `LOCK_NB` 兼容依据为 [Rust File::try_lock 官方说明](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock)；实际部署仍要求原产品持有该锁与当前内核行为一致。


## 当前 HO5 原生前置核查

只读核查发现，当前固定 db9fc29 Linux 产物的 `--product-info-json` 实际报告 `separated_lifecycle=false`、background/frontend entry 为空；源码 `gui_backend.rs` 将这些声明限制在 macOS，虽然通用 entrypoint 中存在其他平台的入口分支。执行器以已安装编译产物的真实声明为准，不能用方案里的合成 true 字段提升其能力，不会尝试启动后再解释为支持。需要经原构建及原生验收流程取得明确支持的候选后才能切换，不能改写旧产物信息或用文件旁注重新标记。

该机还存在系统提供的 `TimeoutStopFailureMode` 和现有运行时 `CPUWeight` drop-in。这些覆盖现已按下述精确内容、来源和 inode 合同纳入；未知覆盖仍拒绝。没有删除、写入或改变任何实际覆盖。被拒上传仍保持原操作，未选择其他通道交付执行器。因此当前没有实际执行 GUI 关闭后后台存活验收。


## Linux 能力核验与保留覆盖（后续进展）

已安装旧产物的 false 声明是 `gui_backend.rs` 中 macOS-only 条件造成的，不能据此认定 Linux 没编译入口。HO5 对原二进制的隔离负向实测证明：`--background-service` 被识别，且在自有控制目录被锁定时，在加载/生成身份之前拒绝第二运行时；未触碰真实 profile，也未创建认证对。`restored_ui::run_attached_gui`、后台 IPC 和前台独立锁已有 Unix 实现。

源码把 Mac/Linux 的分离声明绑定到同一 `SEPARATED_LIFECYCLE_COMPILED` 能力常量并引用实际编译的前后台入口，Windows 不扩展。原受控入口真实编译的能力测试 2 项、profile/前台所有权测试 5 项、IPC 契约测试 7 项通过；前台控制对象关闭重建时后台锁仍被持有，测试不生成凭据。这不是 Linux GUI 正向呈现验收，也不能把旧 ELF 的旁注改成 true；仍需原构建者生成新的已验证 Linux 兼容产物，按原固定 provenance 更新候选。

复现固定构建模式：`lifecycle-capability`、`lifecycle-ownership`、`lifecycle-service-contract`；均使用 `scripts/test_stream_settings_model.py` 原入口、原共享 target 和官方只读绑定。`lifecycle-linux-native-check` 使用 x86_64 Linux 目标和完整 `gpui-restoration,native-linux-video` 特性执行真实 `cargo check --tests`。它是编译检查而非 Linux 链接/运行/发布。交叉构建不应混用宿主库；缺目标 sysroot 继续拒绝。

允许的现有覆盖只有以下两个来源，完整文本 SHA、device/inode、uid、mode 全部进入方案：

- `/usr/lib/systemd/user/service.d/10-timeout-abort.conf`：root 所有，只允许 `[Service] TimeoutStopFailureMode=abort`。
- `/run/user/<uid>/systemd/user.control/weavelo.service.d/50-CPUWeight.conf`：当前用户所有，只允许 `[Service] CPUWeight=<1..10000>`；HO5 实测为 10000。

两者均只读取和保留，不执行 set-property、不删除覆盖、不更改 vendor 的停止行为。新字段 `preserved_dropin_semantics` 显示精确保留值。重复/重置/额外指令、未知路径、不可写权限以外的额外 mode、内容或 inode 漂移均拒绝。每次 systemd 操作还核对有效 `TimeoutStopFailureMode`/`CPUWeight` 与计划一致；回滚也要求同一组覆盖，而非恢复或覆盖外部修改。

CPUWeight 保留在既有 `weavelo.service` cgroup 上，不暗中给手动 GUI 另设权重；前台性能仍需实际测量。`abort` 在停止超时后可能触发 SIGABRT，因此只有 `Job` 为空且 ActiveState/SubState 已稳定时，才允许宣告后台启动、停止完成或回滚成功。旧 PID 尚在但 stop job 未结束不能被当作已恢复；执行器保留原事务供显式恢复，不取消未知 job 或关闭 vendor 机制。

新的 facts 中 `observation.drop_ins` 为上述记录数组，每项仅有 path、text、sha256、device、inode、uid、mode。旧空列表方案只适用于实际没有覆盖的主机，不能用来忽略 HO5 的现有覆盖。真实生产切换仍须满足兼容产物和未被拒绝的交付路径，当前没有执行。


## Linux 交叉编译与归档验证

`client/build.rs` 通过锁定的 `cc` crate 选择 C 编译器、归档器和 target 参数，不直接调用宿主 `cc/ar`。生产构建配置由 `client/build_toolchain.rs` 共享给真实工具选择回归；`native-build-toolchain` 模式验证带连字符/下划线的 target 变量、`TARGET_CC/AR`、通用变量的优先级和原生默认值。此回归不启动产品或生成身份。

完整链接用原受控入口的 `lifecycle-linux-native-build`，沿用主 main、`~/rust-target`、源码守护、观察绑定、空间底线和进程组收尾。需要事先验证的 x86_64 Linux sysroot、已安装 Zig/CMake/Clang、原有 Zig 缓存；不安装系统软件，不使用宿主 pkg-config 库。以下环境路径是占位符，必须替换为已验证的目录与工具：

```sh
export WEAVELO_VALIDATION_SYSROOT=/absolute/verified-linux-sysroot
export WEAVELO_VALIDATION_LINUX_CLANG=/absolute/installed/clang
export PKG_CONFIG_SYSROOT_DIR_x86_64_unknown_linux_gnu="$WEAVELO_VALIDATION_SYSROOT"
export PKG_CONFIG_LIBDIR_x86_64_unknown_linux_gnu="$WEAVELO_VALIDATION_SYSROOT/usr/lib64/pkgconfig:$WEAVELO_VALIDATION_SYSROOT/usr/share/pkgconfig"
export PKG_CONFIG_PATH_x86_64_unknown_linux_gnu=
export PKG_CONFIG_ALLOW_CROSS_x86_64_unknown_linux_gnu=1
export BINDGEN_EXTRA_CLANG_ARGS_x86_64_unknown_linux_gnu="--target=x86_64-unknown-linux-gnu --sysroot=$WEAVELO_VALIDATION_SYSROOT"
export CFLAGS_x86_64_unknown_linux_gnu="--sysroot=$WEAVELO_VALIDATION_SYSROOT -isystem $WEAVELO_VALIDATION_SYSROOT/usr/include"
export CXXFLAGS_x86_64_unknown_linux_gnu="$CFLAGS_x86_64_unknown_linux_gnu"
export RP_VERIFIED_FFMPEG_INCLUDE="$WEAVELO_VALIDATION_SYSROOT/usr/include/ffmpeg"
python3 -B scripts/test_stream_settings_model.py --mode lifecycle-linux-native-build \
  --lifecycle-authority "$PWD/docs/plans/ARTIFACT-LIFECYCLE-ADOPTION.json" \
  --lifecycle-binding "$PWD/.rust-storage/linux-cross-validation-binding.json" \
  --lifecycle-helper "$HOME/.codex/skills/rust-project-lifecycle/scripts/rust_storage.py"
```

观察绑定必须由官方 `adopt` 针对当前依赖合同生成；既有绑定不覆盖。上面的跨编译放行仅用于已经验证的目标 sysroot，不允许指向 Homebrew 或其他宿主库。标准输入路径含空格时，CFLAGS/BINDGEN 的路径还需要按各工具参数解析规则引用。

CMake 使用固定目标 `ar/ranlib`。旧缓存的 `CMakeCCompiler.cmake` 会重新加载宿主归档工具，单独增加 toolchain 文件不够；目标 CMake entry 通过官方 post-project include 覆盖实际生成命令，保留原缓存及其生成记录。真实 CMake 夹具验证已有宿主工具缓存能在同一目录重新配置。锁定版本的 audiopus CMake helper 未登记工具链环境变化，验证入口通过其已有受跟踪的 pkg-config 选择变量，固定原 bundled Opus 并随工具配置摘要改变重配；不清理共享 target。

Zig 已启用的 C/C++ 未定义行为检查保持为 trap，避免 Rust 链接器依赖未配置的 Zig UBSan 诊断运行时。最终链接使用目标 Clang、真实 sysroot 和严格共享库符号解析；不会通过允许未解析符号来掩盖依赖缺失。

在匹配 Fedora 44 x86_64 开发包上，已实际完成完整 GPUI/native-linux-video ELF 链接，检查视频桥接和 Opus 归档全部成员为 x86_64 ELF，递归动态库无缺失，成品没有开发 sysroot RPATH/RUNPATH。产物保留在原共享 target 的固定 debug 输出；它是受控开发构建，未生成正式身份回执、未发布或替换已安装应用。下一运行验收必须通过获准的 Linux 路径真实运行候选，继续原身份生产者/原生验收流程；不能用静态检查、Mac 上的测试或传输成功伪造正式回执，不能绕过 HO5 既有上传拒绝。

## 正式 Linux release 候选和延后原生观察

原 `build_identity_gate.py build` 新增显式 `--defer-linux-observation`：仅允许 Linux 的正常 `release` 配置，仍使用原主 main、共享 target、生产者锁、固定临时身份输入、源码/工具环境前后核对和空间底线。它实际运行 Cargo 编译，在最终 ELF 的只读 `.rpbuild` 节核验完整 source/config identity，输出 `compiled-build-awaiting-native`。这类回执的 `native_observation=pending`、`product_info=null`，不能用于正式安装或冒充 `observed-build`。现有无身份 debug ELF 无法导入该入口。

配置和 prepare 沿用原入口。交叉编译环境除前述已核验的目标配置外，还须提供 `WEAVELO_VALIDATION_SYSROOT_MANIFEST` 和它独立固定的 `WEAVELO_VALIDATION_SYSROOT_MANIFEST_SHA256`；环境 identity 绑定工具可执行文件、目标适配器和完整 sysroot 文件清单/实际内容。编译前后必须得到同一摘要。保留原权威/adoption；生成日志、观察绑定、锁和 root-transition journal 作为保护证据，不再被误纳入源码 identity 或造成自引用。

下面的路径和摘要须由实际配置/源码审阅结果替换；输出必须是尚不存在的确定文件。已有 user patch 仍由 prepare 的 `--expected-patch-sha256` 精确固定，不重写或偷偷清理。

```sh
python3 -B scripts/build_identity_gate.py build \
  --repo "$PWD" --identity /private/review/linux-release-identity.json \
  --identity-sha256 REVIEWED_IDENTITY_FILE_SHA256 \
  --lifecycle-binding "$PWD/.rust-storage/linux-cross-validation-binding.json" \
  --defer-linux-observation --output /private/review/linux-release-compiled-receipt.json
python3 -B scripts/build_identity_gate.py candidate-manifest \
  --binary "$HOME/rust-target/x86_64-unknown-linux-gnu/release/weavelo" \
  --compiled-receipt /private/review/linux-release-compiled-receipt.json \
  --compiled-receipt-sha256 REVIEWED_COMPILER_RECEIPT_SHA256 \
  --expected-commit REVIEWED_COMMIT --expected-identity-sha256 REVIEWED_IDENTITY_SHA256 \
  --output /private/review/linux-release-candidate-manifest.json
```

候选 manifest 是 `weavelo-compiled-release-candidate`，固定编译回执文件摘要、其规范内容摘要、ELF、真实版本/源码/配置 identity 及文件大小，明确等待原生观察。它不是已验收的 release manifest；原 `seal`、`installed`、Linux installer 都拒绝它。编译器产物保留在原 release 默认输出，不为候选另复制完整程序、创建归档或维护第二份源码。

原合规通道恢复且具体原生执行获准后，Linux 上的原 gate 使用 `observe-linux`，同时接收上述编译回执和候选 manifest 的独立摘要，先验证全部关系和 ELF identity，然后真正运行同一字节候选的 `--product-info-json`，核对平台、架构、版本、GUI/native-video、分离前后台声明与编译 identity。只有这一步才生成 `observed-build`，保留原 Mac 编译 source/cache 路径作为历史事实，并绑定原编译回执和候选 manifest 摘要。不是在 Linux 重新编造编译历史，也不通过文件旁注替换运行结果。实际 GUI、视频、输入和关闭窗口后后台存活验收仍分别记录。

获得真实观察后，原 `seal` 才生成 `weavelo-release`，并用 `build_receipt_canonical_sha256` 绑定 observed 文档内容。Linux 原 first-install 入口要求显式 `--build-receipt/--build-receipt-sha256`、`--release-manifest/--manifest-sha256`、`--expected-commit`、`--identity-sha256`、`--compiler-binary` 与独立核验的 `--old-binary-sha256`，不再使用固定 db9fc29 的 Linux 候选。它核对实际 ELF 和相同 receipt/manifest，重建内容必须逐字节复现审阅的 release manifest。已有安装和 journal 继续受原拒绝覆盖/恢复约束，参数不能把 first-install 变成隐式覆盖安装。

同 profile 生命周期执行器从原私有安装 journal 的完整 plan/hash 取得已安装 source、binary、identity 和 observed receipt pin，不再导入全局旧 Linux PINS。旧 journal 的原回执位置仍兼容；其真实旧 source 不会被重标为新构建。该逻辑只验证已安装产品，不安装新候选。

这些本地工程和 release 编译步骤均不授权传输或部署。原源地址策略拒绝、文件源授权和传输业务接口问题仍须由原平台处理；不会因 release 体积变化而改通道、分片、伪装文件或绕过拒绝。


## 关闭、全进程退出与重连输入核查（2026-10-17）

- 关闭附着GUI：原 `on_window_should_close` 释放输入并清理本前台会话，退出GUI；后台保留。`SessionState::drop` 另会释放键鼠/修饰键。该调用链已有实现，实际安装程序的手动关闭/重开与远端继续可用仍须真机证明。
- 关闭连接的实现缺口已复现并修复：`WorkspaceConnection::control(Close)` 在返回future前调用原 `InputState::seal`；此前输入只在actor取到Close后清空，队列满时input tick仍能刷新旧held。现在seal立即清空slot，保留请求计数单调。实际Rust用例填满命令队列、不poll Close future，验证旧ready/snapshot/refresh/grant即时失效，再验证新绑定不能采用旧request或恢复旧held。修复前21通过/1失败，修复后22通过；无协议、身份或输入授权变化。
- 历史核查（由下节实现覆盖）：退出整个产品当时仍有独立实现缺口：当前附着GUI的 `Quit` 只调用本前台owner.close_all与cx.quit，原后台Request没有Shutdown；不能将它称为退出全部后台。需在原同用户IPC/原服务监督模型中先明确精确owner停止、活动会话保护、输入/原生资源释放、服务不立即重启及再次启动的语义，再实现全退出。这里没有新增停止命令、杀进程、修改launchd/systemd或改变持久授权。
- 重连后的旧输入：现有epoch/request/source binding及前台hold超时保护继续保留。本次22项连接回归包含关闭重试、释放重传和源切换隔离；只能证明本地代码路径，不能代替macOS/Windows/Linux的目标OS按键释放、断线重连或全进程退出验收。

实际回归使用原main、`test_stream_settings_model.py --mode clock-workspace`、既有官方观察绑定与共享target。早期相对路径/陈旧Cargo.lock观察绑定均在编译前被拒，未改合同；采用当前已有且合同匹配的 `linux-cross-validation-binding.json` 后原门禁通过。没有新建源码副本、缓存、权威或发布产物。冻结二进制基线仍为feba70a；本次源码修复未部署，不能计入其真机通过率。


## 当前全退出契约与实现（源码已补齐，尚未部署）

三个意图明确区分：

| 用户动作 | 前台 | 当前用户的本产品后台 | 回执边界 |
| --- | --- | --- | --- |
| 关闭窗口 | 附着模式退出前台；旧standalone仍按原行为收起窗口 | 不停止独立后台 | 不等于全产品退出，也不新增权限 |
| Quit GUI（退出前台） | 关闭本前台进程的界面和会话 | 独立后台继续运行 | 仅在已有分离模式提供；standalone不冒充独立前台 |
| Quit Weavelo（退出全部） | 本进程所有现有窗口锁定输入，收齐各自连接的已认证Closed，再退出 | 分离模式请求准确后台实例停止；standalone直接清理自己的host | 清理失败、在途连接未确定、超时或实例变化时保留错误，不报成功 |

`FrontendExitGroup` 只是本GUI进程现有owner的弱引用集合，不是持久注册或另一套服务。全退出期间禁止新连接和重新解锁输入，所有窗口都纳入回执；失败后保留原owner及原连接，即使关闭了失败窗口或再次点击退出，也不能以空会话列表掩盖未确认的清理；不按进程名批量杀进程，不停止其他用户或其他应用的服务。正在建立而尚无确定连接的请求须先取消或完成，不能假装已经取得它的远端清理证明。

复用原同用户私有控制端点，local service API minor更新为1.1并增加可选 `shutdown.v1`；原Snapshot schema2和旧请求字节保持兼容，网络Session协议不变。Shutdown固定现有generation与可执行文件hash，只接受Background owner；旧实例、错误hash、Standalone或已进入停止的实例均拒绝，不以兼容协商代替身份验证。Windows当前没有已审阅的独立服务停止策略，不广告此后台能力；其standalone退出仍通过同一host清理路径结束本进程，不引入Windows服务权限。

后台停止在原host控制循环中撤销实际输入owner，沿原peer.shutdown、worker join和native-stop回执清理所有所属连接。失败或native完成未知时保留原失败/资源回执并停止接纳新命令，不以超时伪造成功。前台 `close_verified` 只接受来自已认证匹配连接的Closed；actor结束、连接取消和普通超时均不算远端清理成功。

`StopPrepared` 只表示后台host清理完成，不表示进程已退出。Client先登记原生进程退出事件（macOS kqueue、Linux pidfd；Windows观察器实现也通过编译检查），再发原请求；核对回执generation/hash/PID/cleanup后，等待同一进程退出并检查私有端点没有留下或被替换。没有发送进程信号的fallback，失败不换PID、服务或通道重试。成功返回小型JSON `verified-background-stop`，scope明确为 `background-service-only`，包含准确owner、清理、进程退出和端点消失的观测；不含凭据、帧或输入文本。GUI记录该回执后才请求本前台退出；`WEAVELO_FRONTEND_EXIT_READY` 是前台退出意图，真正GUI进程退出仍由外部原生观察验收。

停止前仅只读检查原用户服务策略：macOS核对原LaunchAgent的Label、精确程序/参数、失败重启条件、无定时/路径/socket/MachServices等独立激活，并核对loaded job PID；Linux核对原weavelo.service MainPID、ActiveState=active/SubState=running、Job为空、Restart=no/on-failure、无TriggeredBy、强制正常退出重启、ExecStopPost或OnSuccess激活。缺失、改变或不可读即拒绝。成功路径正常返回0，沿原失败重启策略保持停止，不执行enable/disable、bootout、配置写入或权限修改。保留原RunAtLoad/enabled和角色标记，因此下次登录仍按原自启动意图运行；退出后若需立刻再启动，使用原服务控制入口，不生成第二个独立owner或新身份。

同一已安装可执行文件的 `--background-stop-json` 暴露上述验证操作，仅停止选定profile的独立后台，不能单独宣称仍在运行的GUI也退出；GUI的Quit Weavelo才串联全部前台连接清理、后台停止及前台退出。参数不接受任意PID或服务名。此轮只实现并回归，没有对真实安装调用该命令，也没有变更真实服务策略。

双窗口回归使用两个独占随机loopback端口、真实WorkspaceConnection和OriginalOwner，分别验证全部确认及一个缺失回执的结果。host正反回归使用现有native-worker/withheld-stop夹具；进程观察用真实自有子进程证明超时不能当退出。原 `gui-input-status`、`host-control`、`lifecycle-service-contract`、`clock-workspace` 入口及macOS/Linux/Windows固定编译检查复用主main、共享缓存、原观察绑定、源码守护和收尾验证。它们不连接真实peer、不注入OS输入、不生成真实配对，不代替已安装产品关窗、全退出、重连和后台存活验收。

冻结feba70a候选未改变，新实现尚不在该二进制中。后续包含这些功能的发布须沿原生产者重新冻结真实源码/配置/identity与回执，不能把文档提交或本地测试重标成已部署的源码来源。当前三组六方向的真机状态仍未测，DNS/ACL/配对和暂停上传边界保持。
