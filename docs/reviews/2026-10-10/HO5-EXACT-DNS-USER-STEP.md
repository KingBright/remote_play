# HO5 精确域名 DNS 修正：用户本机执行步骤

此步骤对应已批准的 `sdmntprcentralus.oaiusercontent.com` 精确 DNS 规则。Remote Hosts 普通用户通道的 `NoNewPrivs=1` 不允许通过子进程获得管理员能力；没有安装支持 sing-box 的管理员执行入口。本报告及脚本准备未执行管理员操作，也未恢复暂停的源码传输。

脚本已写入 HO5 用户任务目录：

`/var/home/liang/workspace/.rp-source-picture-20261009/repair-ho5-exact-dns.py`

固定 SHA-256：`9ab92b5ed2368172abc089db26b39ed94317a3c2bf5389c8e03d8ecbcce453c4`。

版本校验写入回执：change-set `e4327e54-72a5-4dcd-b125-ca94eac3dab0`，state=completed；before=`c9561d6e…`，after=上述完整 SHA-256。第一次编辑因参数错误未应用；原操作与文件版本核对后使用 CAS 替换，未重放不确定操作。

## 唯一执行命令

请用户在 **HO5 自己打开的原生终端** 执行下面一条命令，按本机机制完成 sudo 认证。不能通过当前 Remote Hosts PTY 运行，也不能将管理员密码发送给助手。

```sh
sudo /usr/bin/python3 -I -c 'import hashlib,os,stat,sys; p="/var/home/liang/workspace/.rp-source-picture-20261009/repair-ho5-exact-dns.py"; fd=os.open(p,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK); s=os.fstat(fd); (stat.S_ISREG(s.st_mode) and s.st_nlink==1 and s.st_size<=65536) or sys.exit("script identity mismatch"); data=os.fdopen(fd,"rb").read(65536); (len(data)==s.st_size and hashlib.sha256(data).hexdigest()=="9ab92b5ed2368172abc089db26b39ed94317a3c2bf5389c8e03d8ecbcce453c4") or sys.exit("script identity mismatch"); sys.argv=[p,"--apply"]; exec(compile(data,p,"exec"),{"__name__":"__main__","__file__":p})'
```

不带 `--apply` 时仅预检，输出预计差异和哈希，不写配置、不备份、不重启。该模式也读取配置来证明精确差异；本轮助手没有通过工具执行它。

命令中的隔离 bootstrap 在管理员进程内读取固定脚本一次并验证这些同一字节，再直接执行；拒绝符号链接/多硬链接/过大或不匹配内容。`-I` 排除用户目录、当前目录与 PYTHONPATH 对标准库导入的影响。这样不依赖“普通用户先校验、管理员稍后重新打开”之间的文件保持不变，也不会把任务目录中的同名 Python 模块作为管理员代码加载。此 bootstrap 只读固定公共脚本，没有自动提权或管理员 helper 安装。

## 精确语义差异

只在 `/dns/rules/0` 插入：

```json
{"domain":["sdmntprcentralus.oaiusercontent.com"],"action":"route","server":"dns-remote"}
```

规则使用已有 `dns-remote` TLS 服务及已有 `proxy` detour；其余规则、服务器、路由和配置值保持相同。JSON 排版会重新序列化，所以完整文件字节和新哈希会变化。`route/server` 的字段语义来自 [sing-box DNS Rule Action](https://sing-box.sagernet.org/configuration/dns/rule_action/)；现场实际版本仍必须通过其自身 `sing-box check`。

## 脚本门禁与预期回执

脚本仅接受原配置 SHA-256 `4217b7c50bc8032d5f5f854a2e73edc0167851cd3dd013f6b9e3953634337435`。这是此前已确认的预检值，本轮新的读取哈希命令遭自动审批拒绝，未重新核实当前配置。现场有任何漂移则拒绝应用。

应用还要求 root、受保护的目录及普通单链接文件、一个匹配此配置路径的 sing-box ExecStart、受保护的现有可执行文件和活动服务。拒绝重复 JSON 字段、已有该域名规则、服务器差异或已有备份。候选配置校验通过后才建立不覆盖的 0600 原始字节备份 `/etc/sing-box/config.json.rp-exact-dns-20261010.backup`，保留配置 UID/GID、mode 和 xattrs，原子替换，再进行一次应用 restart。失败时仅在配置仍为本次候选且备份哈希正确时回滚原始字节，并进行一次恢复 restart。没有系统包安装、全域名绕过、公开地址检查放宽、缓存全局清理或自动提权。

成功回执为一行 JSON：`state="applied"`、`system_changes_confirmed=true`、`administrator_steps_executed=true`、原/新 SHA-256、备份路径及 resolver 观察结果。输出不含完整配置、凭证、完整 service argv 或可能包含敏感值的配置检查输出。失败回执会给出类型及固定错误码；原配置恢复且服务活动时为 `error_code="original_restored_service_active"`。

`resolver.all_public=true` 才表示这次解析结果全为公开地址。即使配置和服务成功，缓存仍可能令它为 false，此时不能称传输已修复。`source_https_or_transfer_test="not_run"` 明确保留后续 HTTPS/source guard 和原暂停传输的独立检查；本脚本没有下载源码或验证 native build。

## 已运行的独立检查

本地 `python3 -m unittest scripts.tests.test_repair_ho5_dns -v`：8 项通过。它们使用临时非系统配置与 mock 服务，覆盖精确插入、重复字段、漂移与非 root 拒绝、符号链接、重启顺序、备份权限和失败回滚。它们不证明现场管理员认证、实际 sing-box 校验、restart 或 DNS 已成功。

`python3 -m unittest scripts.tests.test_ho5_dns_bootstrap -v`：6 项通过，普通用户在 public fixture 上检查同字节 hash/exec、目录模块不能覆盖标准库、变动脚本/符号链接/FIFO/过大文件拒绝；没有调用 sudo。按 [security-review 技能](/Users/jinliang/.codex/skills/security-review/SKILL.md) 聚焦审查本机 root 入口、用户可写脚本字节及配置写入边界，修正了原“先 hash、后 sudo 重新打开”和普通 Python 用户路径导入的执行入口风险。实际管理员机制、service executable/配置绑定和现场 rollback 仍未验证。两组 HO5 专用测试在非 Unix 环境跳过，不使 Windows regression collection 依赖 Unix 文件接口。

自动审批拒绝的是 HO5 只读预检中的“读取整个受保护配置以计算哈希”；理由为可能读取秘密且必要性未充分说明。命令未执行。随后只进行了本轮明确要求的用户目录脚本准备，未重试该敏感读取、未改变路径绕过拒绝。该拒绝需父会话核验；本机管理员操作仍由用户亲自执行。
