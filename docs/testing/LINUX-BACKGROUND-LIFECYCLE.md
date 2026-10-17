# Linux 同身份后台服务 / GUI 分离方案

`scripts/linux_background_lifecycle_plan.py` 是原安装流程的纯方案工具，复用 `background_lifecycle_plan.require_separated_entries` 的产品能力检查。没有 apply、systemctl、子进程、profile 读写或凭据创建入口。它只生成一份内容寻址的私有方案及可选回滚预览，不是新安装器或发布权威。能力 JSON、声明的 PID 和输入 inode 都不是原生身份或归属证明；真实操作仍须由原生产者重新核验。

## 具体变化

现有 Linux user unit 以裸可执行文件及 `WEAVELO_HEADLESS=0` 启动单进程 GUI/运行时。拟议变更只替换两行：

```ini
ExecStart=/home/example/.local/opt/weavelo/weavelo --background-service
Environment="WEAVELO_HEADLESS=1"
```

其余 unit 字节保持原样，继续使用原 `weavelo.service`、同一可执行文件和 `~/.config/weavelo/mesh`。原 enabled/disabled 意图保持，未启用的服务不能自动 enable。桌面条目和安装目录不变；已有默认入口在原产品创建后台角色标记后附着后台，显式 GUI 命令为同一可执行文件的 `--gui`。

方案固定原 unit SHA256、device/inode、uid、mode、links；固定同一 profile 目录身份及现有 mesh.conf / mesh.secret 的 device/inode、uid、mode、links、SHA256。只接受目录 0700、两个现有普通文件 0600；不修权限、不复制或导入认证数据。原源码 commit 和二进制 SHA 也被固定，但此工具不重新编译、包装或部署它们。

仅支持原安装器的窄 unit 格式。重复指令、额外 ExecStart、EnvironmentFile、ExecStartPre、User、未知环境、转义/续行、额外 drop-in、未知角色标记、非 Standalone owner、活动/待定会话或租约都会拒绝。配置差异需要原维护者另行审查，不能靠删掉字段绕过。

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

1. 由原安装生产者在原 journal 中绑定此次生命周期子事务和 reviewed plan SHA。重新核验已安装映像、原 profile 与 unit 的字节、inode/权限、原服务状态、无 drop-in、标记不存在、无会话/租约；未知或漂移则保留原状。再次确认现有认证对仍存在，不能把缺文件解释为首次生成身份。
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

该机还存在系统提供的 `TimeoutStopFailureMode` 和现有运行时 `CPUWeight` drop-in。当前窄方案拒绝任何未纳入的 drop-in；这不表示应删除它们或改变系统权限。后续应单独审阅、固定这些既有覆盖的内容/来源并保持其效果，再扩展允许范围。没有执行此项配置修改。被拒上传仍保持原操作，未选择其他通道交付执行器。因此当前没有实际执行 GUI 关闭后后台存活验收。
