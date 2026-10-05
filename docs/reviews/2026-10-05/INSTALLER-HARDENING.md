# RemotePlay 安装器的阶段记录与中断回退

本补丁仅在隔离目录验收。原升级 turn 未确认终止前，不集成或部署；磁盘补丁不能撤销旧解释器已加载的代码。用户已批准的升级可独立用现有验证安装器恢复，本补丁不作为该升级的前置条件。

## 现有机制与复现

原安装器已经用 `~/Applications/.remoteplay-install.lock` 的非阻塞 flock 防并发，并在加锁后核对应用版本、build、签名身份和摘要。旧预检在另一个安装完成后失效；同版本同摘要是 already_installed。补丁继续使用这个文件和 inode，不删除、原子替换或重写锁。

原版 fixture 10 项：3 项现有保护通过，2 项行为失败、5 项功能缺口报错。两个行为失败是服务 PID 在加锁后变化仍继续安装、未知 journal 被忽略。缺少的机制包括共同 deadline、持久阶段及 rename 后恢复。原始输出见交付目录 baseline-fixture-result.txt。

## 兼容改动

* `PACKAGE.zip` 默认仍只读规划，`PACKAGE.zip --apply` 仍为显式兼容升级。原签名策略、版本规则、TCC、管理员权限和持久配置保护保持原规则。
* apply 在同一锁内重新核对应用、launch-agent、服务快照和规范路径进程；服务快照变化立即拒绝。already_installed 在锁内再次核对后返回。
* 新增 `--deadline-seconds`；plan/apply 默认 300 秒，恢复默认 120 秒。签名验证子进程、launchctl 查询与操作使用剩余共同预算；摘要、解压、复制和 rename 边界检查 monotonic deadline。
* deadline 是协作式预算。没有强杀整个安装器的 watchdog；无法限制内核或文件系统自身不可中断的等待。正常失败回退有单独且有限的 120 秒安全预算。

## Journal 与恢复

`~/Applications/.remoteplay-install-journal.json` 与锁分离，用同目录临时文件、fsync、原子 replace 和父目录 fsync 写入，权限 0600。包含操作 UUID、阶段、目标/暂存/备份路径、旧/新公开签名身份和应用摘要、原服务快照、预算与更新时间，不写入 NativeMesh 内容或其摘要。

每次服务操作、目录 rename、安装验证和健康检查都有前置 intent 与后置 acknowledgement。完成或回退后保留终态 journal 和备份 receipt。内存中保留配置摘要只用于正常路径的既有本地比较。

未完成或未知 journal 会阻止新的 apply。显式 `--recover` 在同一锁下读取持久记录，验证规范路径、非符号链接及实际旧/新应用签名和摘要，再保守恢复已固定的旧应用；不会继续向前安装。根据实际目录状态处理首次 rename 后目标缺失、新应用已经替换、以及回退 rename 再次中断。恢复不能证明整个中断期间配置未变化，故 `configuration_unchanged=null`。

服务命令 timeout 或 intent 未获 acknowledgement 时，记录 recovery_required 和 uncertain_external_phase，拒绝自动重复 bootout/bootstrap。需先诊断命令实际结果，再安排专门恢复；此版本没有“强制清除未知状态”开关。对于未知/被改动的应用或路径，保留目录并停止。正常 prepared 阶段中断可在不停止服务的情况下清理事务并结束；已终止的回退再次调用是 no-op。

原 `replace_with_rollback` 注入式测试接口保留兼容；生产 install 入口使用新的 journal 事务。没有新增常驻 helper、广域控制端点、退出应用接口或管理员操作。已有 NSRunningApplication 正常退出调用的源封装未保存于检查范围，不能在此补丁中假定外层工具 bootstrap 已受 Python deadline 控制。

## 验收与集成边界

隔离测试替换所有实际 ps、launchctl、应用验证及健康调用。应用、配置和 archive 均为临时合成 fixture，没有运行正式应用、退出进程、读 NativeMesh 或修改 owner/mode。

61 项测试完成，55 项通过；6 项原生签名集成测试按原规则跳过，未申请访问签名私钥。覆盖锁并发与 inode、旧预检、同版本 no-op、锁内服务变化、unknown journal、共同命令预算、apply 预算到期后的有限回退、bootout/bootstrap 结果未知不重放、首次/第二次 rename 后恢复、回退再次中断、恢复幂等、篡改备份拒绝、符号链接 journal 拒绝和正常健康失败回退。

集成前必须确认旧升级 turn 实际终止，并对 baseline-manifest.json 的原文件 hash 做 CAS；另核对仓库 HEAD、外来 staged 变动与既有 dirty 状态。补丁只改变 installer/guard、新 fixture 和本文档；测试依赖脚本及签名策略只是原样复制。当前没有集成、commit、push、安装或权限修复。


## system mesh 服务预检补充

安装器现在用固定的只读 `launchctl print system/com.remoteplay.mesh` 分类外部系统服务，最多使用共同 deadline 的剩余 15 秒。只返回 domain、label、loaded/running、PID、program 等限定字段，不回传原始 arguments、environment 或 stderr。只有明确的 not-found 响应代表不存在；超时、权限拒绝、无法执行及空成功响应均是 unknown。

兼容版本关系与当前可执行性分别表示：plan 保留原 action，增加 system_mesh 和 apply_blockers；系统服务已加载或状态未知时，不声称将重启，并拒绝会修改应用的 apply。锁内和暂存候选完成后再次核对，显式中断恢复也先确认系统服务不存在。已安装同版本的 no-op 可以返回分类结果而不执行服务或应用变更。已加载但暂时未运行的系统服务也会阻止修改，因为它可能自动重启。

GUI Quit 不会卸载 launchd 系统服务，因此该情况不会再被描述为需要反复退出 GUI。安装器没有获得系统服务控制能力：不 bootout/bootstrap system domain、不发送退出信号、不修改 owner、网络权限或管理员配置。系统服务的处理需要独立诊断和适当权限；unknown-noReplay 仍优先拒绝重放，未增加强制清除或管理员常驻 helper。

新增 fixture 覆盖 GUI Quit 无效且系统服务仍加载、用户重复进程仍活跃、系统服务在锁内/暂存后出现、查询权限拒绝/超时/空响应、deadline 已过而不发查询、同版本 no-op、根服务阻止 rename 中断恢复，以及未知服务命令结果优先拒绝重放。所有系统查询和服务操作均由合成 runner 替代，未查询或控制真实系统服务。
