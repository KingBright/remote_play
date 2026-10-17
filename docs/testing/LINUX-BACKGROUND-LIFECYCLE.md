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

预览返回要恢复的原 unit 字节、mode、原 enabled 意图及精确 marker/unit CAS 条件，始终 `applied=false`。它不执行停止、删除或重启；真实执行器尚未接入。本阶段不会触碰当前 HO5 服务、Windows ACL、被拒的源码上传、共享缓存或成品。
