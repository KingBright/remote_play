# HO5 DNS 与 SDK 一次性本机操作草稿

状态：供主会话审阅，尚未向用户发出执行请求，尚未执行。本轮已有 DNS/PipeWire 授权继续有效；这里说明执行通道确实缺少的本机管理员能力，不要求用户重新批准同一范围。

## 当前证据与选择

HO5 为 Bazzite 44/Kinoite，booted 44.20260921；已存在 staged 44.20261006.1，当前没有 rpm-ostree transaction。Rust 1.98.0、GCC/G++ 和 GNU ld、pkg-config 已在 host。空闲 759744372736 bytes（约 708 GiB）。稳定源码是 `/var/home/liang/workspace/remote_play`，当前 HEAD `969ca10922eb17e705877b7ca4cf5ce3bec836c1`，已跟踪工作树干净；已有 `target/`。

Remote Hosts 进程仍是 UID 1000、CapEff=0、NoNewPrivs=1；不存在本轮可用的管理员执行器。原生 rootless Podman/toolbox re-exec 的历史失败保持原状态；现有 Ubuntu 26.04 `antigravity-box` 记录不证明可进入或具有 SDK，本轮读取其 rootfs 被 PermissionError 拒绝并停止。未启动、修改或复制任何容器，也未转向 NAS。

host 缺 `pipewire-devel`（包含 PipeWire/SPA）、`alsa-lib-devel`、Clang/libclang、CMake，以及产品 X11 keyboard/font 开发链接依赖。旧 `~/.local/lib/pkgconfig/alsa.pc` 指向运行库、没有 Cflags，不能证明完整 ALSA SDK。用户目录及 Linuxbrew 的已知 Clang/CMake/相关 header 路径本轮也未发现。

建议在主会话统一审阅后，用 HO5 原生终端进行正常 Fedora package layering，使此后普通 agent 可以直接在 host 持续编译，无需增加提权 helper、开放容器服务或改变 NoNewPrivs。Bazzite 官方通常优先开发容器；本次现有通道不能正常进入其 user namespace，因此该 host SDK 路径需要明确接受正常 layering 的维护和重启前提。[Bazzite package layering](https://docs.bazzite.gg/Installing_and_Managing_Software/rpm-ostree/)，[Bazzite Distrobox](https://docs.bazzite.gg/Installing_and_Managing_Software/Distrobox/)

## 最小包范围

Host 适配需要 `pipewire-devel alsa-lib-devel clang clang-libs cmake`。GPUI/Ely 保持现有 X11/Wayland 功能，需要 `libxkbcommon-x11-devel fontconfig-devel freetype-devel`；xkbcommon-x11-devel 的依赖提供 libxkbcommon-devel/libxcb-devel。不安装另一种 GUI 框架，不加入新 repo，不关闭 GPG/TLS 检查，不以 pkg-config 占位文件冒充 SDK。[Fedora PipeWire/SPA 开发包](https://packages.fedoraproject.org/pkgs/pipewire/pipewire-devel/fedora-44.html)，[ALSA 开发包](https://packages.fedoraproject.org/pkgs/alsa-lib/alsa-lib-devel/fedora-44.html)，[xkbcommon-x11 开发包](https://packages.fedoraproject.org/pkgs/libxkbcommon/libxkbcommon-x11-devel/fedora-44.html)

实际包版本及完整事务由 HO5 的 rpm-ostree 解析，本轮未下载 metadata、未求解或安装事务，不能保证候选包当前可解析。先最小 host check、再 app check；后续 native-video feature 需要的额外 SDK 单独核实，不能凭这组包宣称原生媒体发布构建通过。

## 供主会话审阅的一次粘贴操作

此块复用已部署、固定 hash 的 DNS 脚本入口，添加一个普通用户 DNS 结果门禁，然后正常 staging SDK。只在 HO5 用户自己打开的原生终端输入；不能通过当前 Remote Hosts PTY。DNS 原配置漂移、备份已存在或任一前提失败会停止，不自动重试。原脚本的受保护配置检查、备份和回滚边界见 [原步骤](HO5-EXACT-DNS-USER-STEP.md)。

```sh
sudo /usr/bin/python3 -I -c 'import hashlib,os,stat,sys; p="/var/home/liang/workspace/.rp-source-picture-20261009/repair-ho5-exact-dns.py"; fd=os.open(p,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK); s=os.fstat(fd); (stat.S_ISREG(s.st_mode) and s.st_nlink==1 and s.st_size<=65536) or sys.exit("script identity mismatch"); data=os.fdopen(fd,"rb").read(65536); (len(data)==s.st_size and hashlib.sha256(data).hexdigest()=="9ab92b5ed2368172abc089db26b39ed94317a3c2bf5389c8e03d8ecbcce453c4") or sys.exit("script identity mismatch"); sys.argv=[p,"--apply"]; exec(compile(data,p,"exec"),{"__name__":"__main__","__file__":p})' &&
/usr/bin/python3 -I -c 'import socket,ipaddress,json,sys; a=sorted({v[4][0] for v in socket.getaddrinfo("sdmntprcentralus.oaiusercontent.com",443,type=socket.SOCK_STREAM)}); ok=bool(a) and all(ipaddress.ip_address(v).is_global for v in a); print(json.dumps({"resolver_addresses":a,"all_public":ok,"source_https_or_transfer_test":"not_run"})); sys.exit(0 if ok else 1)' &&
sudo /usr/bin/rpm-ostree install --idempotent pipewire-devel alsa-lib-devel clang clang-libs cmake libxkbcommon-x11-devel fontconfig-devel freetype-devel
```

**已有 staged OS 更新必须在主会话明确说明。** 此 package install 会作用于 pending deployment；正常重启后 SDK 与既有 44.20261006.1 更新一起生效。草稿没有 `--apply-live`、`--reboot`、upgrade/rebase/reset/rollback 或自动重启。用户应自行选择合适的重启时机；若不接受合并这项已存在的更新，停止此路径，先处理该具体部署决策，不私自取消/替换 pending deployment。

保留 DNS 一行 JSON 回执、rpm-ostree 事务结果和重启后实际状态；无需复制完整配置或管理员密码。DNS 失败导致第一项停止时，不能手工跳过门禁并声称来源已修复。安装失败也不取消已成功的 DNS 修复；分别记录，避免盲目重放原 DNS 应用。

## 后续由 agent 完成的核验

本机回执到达后，只读刷新 booted/staged 状态，确认 header 文件、pkg-config `libpipewire-0.3 libspa-0.2 alsa xkbcommon xkbcommon-x11 freetype2 fontconfig`、Clang/libclang 和 CMake 实际可用。没有管理员能力变化也可以正常 rustc/cargo 构建，不需关闭 NoNewPrivs。

源码域名本轮仍为 `198.18.0.25` 和 `fc00::f`，resolver.all_public=false。原传输 operation `37c1dfde-5dbc-420e-a19d-aeb49e8702d8` 仍 paused、confirmed_bytes=0、source_authorization=required。只有合法来源状态及新授权都确认后才处理这个原 operation；本块没有 HTTPS 下载、重放传输或更换地址绕过 SSRF。旧传输的 X11 delta 不是 a5e45ac/后续审查修复的完整源码，完成它也不能证明最新切片已放置。须另外核对完整合法 delta、mtime 与编译 provenance。

然后复用现有 target，按 [设备池计划](../../testing/DEVICE-POOL-ACCEPTANCE-PLAN.md) 做最小 host→app 条件编译，并保留源码/命令/SDK/target/exit 的回执。当前没有 Linux 编译或实际 picker、像素、presentation、输入/音频的成功回执。

此前自动审批拒绝了读取整个受保护 sing-box 配置来计算 hash，理由为可能读取秘密且必要性未充分说明。本轮未重试该读取，也未更改路径绕过拒绝；只查询公共 DNS 和用户目录/系统包元数据。管理员配置读取仅属于已审阅脚本在用户本机亲自执行时的内部步骤。
