# RemotePlay 安装版本与启动入口诊断（2026-10-05）

## current

本轮只完成只读诊断闭环，没有部署客户端、修改系统权限或改变 GUI 架构。
现有源码恢复目标仍是原版 GPUI + yororen_ui，功能和真实呈现验收要求不变。

本轮经已授权 Remote Hosts MCP 取得的新证据：

- MacBook 正式安装为 `2.0.0-alpha.7 / 20260930.7`。磁盘 LaunchAgent 与加载的服务均指向 `~/Applications/RemotePlay.app/Contents/MacOS/remote_play`，该路径有运行进程。
- MacBook 还运行着 `remote_play/target/package/macos/RemotePlay Unified.app` 中的另一份程序。其磁盘 Info.plist 的 build 为 `0.1.0`，没有 `RemotePlayReleaseVersion`。不能将它视为 alpha.8，也没有停止该进程。
- MacBook 的两份持久身份配置通过 `lstat` 确认为 uid 0、0600；本轮没有打开它们、读取内容或改变所有者/权限。这与前轮安装保护中止的记录一致。
- Studio 正式安装为 `2.0.0-alpha.8 / 20261004.80`，只观察到一个正式路径进程；profile renderer 元数据为 `restored-original-gpui`。
- renderer 元数据 schema 1 不包含 PID，因此它不能单独绑定当前进程或证明窗口真实呈现。进程路径下的磁盘版本也不是运行进程内存中版本的证明。

MacBook 用户继续打开正式入口时仍得到旧安装；额外的旧包装程序使窗口来源进一步混淆。源码 alpha.8、已发布 alpha.8 包与正式安装 alpha.7 是不同事实，不能以构建/发布成功代替设备升级成功。

## repro

在选定 Mac 的正常用户会话中执行：

```sh
python3 scripts/diagnose_macos_installation.py --expected-version 2.0.0-alpha.8
```

脚本只执行 `/bin/ps -axo pid=,comm=` 与 `launchctl print`。它不启动 RemotePlay，不发送窗口激活消息、不读取进程参数或服务环境、不创建 profile、不修改文件。旧二进制可能不认识 `--product-info-json`，因此此观察器不执行该命令。

报告分别给出正式安装、磁盘启动配置、加载服务、进程路径、renderer 元数据和持久文件的所有者/权限。配置文件只使用 `lstat`；不输出配置或密钥内容。找不到/无权限/命令失败保留为未知或 incomplete，不自动修复或重试启动。

退出 0 表示完成观察，可能仍存在版本/路径冲突；退出 2 表示关键观察不完整。两者均不表示签名、视觉、串流、输入或长稳验收通过。

## tests

- 新增 18 项固定测试全部通过，覆盖旧安装及开发副本同时存在、加载服务与 plist 不一致、陈旧/无效 renderer、配置文件不打开、只读符号链接诊断、观察失败/超时与 profile 不创建。
- 在隔离副本中执行现有发行脚本全套回归：98 项，92 通过、6 项原生签名集成测试按既有条件跳过。跳过项不计为通过。
- 新脚本通过 Remote Hosts MCP 在 MacBook 实际运行，完成且退出 0；准确报告旧正式安装、额外旧包装进程和持久文件所有者差异。
- 新增 GitHub Actions，在 macOS 和 Linux fixture 中运行这 18 项诊断测试。CI 最终结果应以该提交的远端回执为准。
- 本轮没有运行新的跨设备串流、原版窗口截图/交互或长稳测试；之前设备专项结果仍是历史结果。

## next

1. 以本脚本固定复核设备安装与启动入口，明确用户实际看到的是哪个窗口。不得把 profile 元数据当窗口证据。
2. MacBook 升级仍需对两份 root 所有的持久配置提出精确、经确认的归属修复动作，然后沿用固定签名安装器；不放宽 0600、不跳过身份比较、不以 sudo 整体运行应用。
3. Windows 的旧安装和 Android 尚未安装 alpha.8 仍需新的授权设备证据；本轮没有复查或部署这两端。
4. 完成各设备正式升级后，分别补真实截图、源选择、视频呈现、输入、文件传输、错误恢复与长稳；每个环节独立保留结果。
5. 本次提交只包含诊断脚本、18 项测试、其 CI 和这份接续记录。原有 144 项未提交改动及旧 GUI/媒体实现不属于本次提交，继续保留，不能描述为已推送。

## 接续与回退

本轮开发和回归使用隔离副本；整合新文件前后比较原有 dirty 文件摘要与 HEAD。
新文件通过独占创建整合，目标已存在或原有文件变化时中止，不覆盖其他任务。
本提交没有应用/服务运行时变更；回退只需撤销本轮提交，不需要停服务、换安装包或修改配置。
