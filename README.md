<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/weavelo/mark-dark.svg">
  <img src="assets/brand/weavelo/mark-light.svg" alt="Weavelo 半织结 W 标志" width="96" height="96">
</picture>

# Weavelo

把多台设备织成一个工作空间。

Weavelo 是面向 macOS、Windows、Linux、Android 和 Web 的跨设备远程访问项目。它以共享 Rust 内核连接设备发现、加密配对、远程画面与输入，并提供文件和剪贴板等协作能力。桌面采用原有 GPUI 界面与各平台原生视频路径。

应用身份为 `fun.hackerlife.weavelo`，项目与主程序名为 `weavelo`。品牌使用 V7 半织结 W；[源资产与来源记录](assets/brand/weavelo/SOURCE.json)覆盖应用、网站和各平台图标。

## 当前状态

品牌迁移按设备分别核验。编译、签名、安装、系统权限和实际画面/输入验收分别记录；历史版本的链路结果不代表 Weavelo 新版验收。实际迁移使用已审核的计划和原事务记录。

仓库为 [KingBright/weavelo](https://github.com/KingBright/weavelo)，已完成原仓库的原地改名并保留 Git 历史。最新代码、当前部署、URL 与历史证据的边界见[品牌切换清单](docs/plans/WEAVELO-BRAND-CUTOVER.md)；首次身份迁移机制见[身份迁移计划](docs/plans/WEAVELO-IDENTITY-MIGRATION.md)。

## 构建

先读 [AGENTS.md](AGENTS.md)、[主工作区约定](docs/testing/MAIN-WORKSPACE-VALIDATION.md)和[构建身份门禁](docs/testing/BUILD-IDENTITY-GATE.md)。使用原 main 和已声明的共享缓存；不要复制第二份源码或绕过交付 authority。

```bash
# 查看正式入口所需的独立源码与身份参数；这些帮助命令不会构建或安装。
python3 -B scripts/build_identity_gate.py --help
python3 -B scripts/build_identity_gate.py prepare --help
python3 -B scripts/build_identity_gate.py build --help
```

| 平台 | 正式入口与要求 |
|---|---|
| macOS | 身份绑定构建后走 `scripts/package_macos_unified.sh`，使用现有固定签名证书与原 `~/rust-target` 缓存。 |
| Windows | 原生 MSVC 环境使用 `scripts/build_windows_current.py`；`--build-only` 只产生构建凭证。正式 PowerShell 打包遵守当前策略，不自行更改策略。 |
| Linux | 使用 `build_identity_gate.py build` 的原生 Linux 路径和原缓存；Portal 权限由登录用户通过系统界面确认。 |
| Android | 原生 JNI 与 APK 分别走 `scripts/build_android_native.sh`、`scripts/build_android_release.sh`；沿用现有签名材料，不因品牌改名创建新密钥。 |
| Web | 在 `web/` 运行 `npm test`；`npm run dev` 启动本地页面。WASM 构建使用该目录的 `build:wasm` 脚本。 |

主程序 crate 为 `weavelo_app`，Android JNI 库为 `weavelo_android`。各平台依赖与当前验证范围以[平台验证矩阵](docs/testing/FOUR-PLATFORM-MATRIX.json)为准。

## 使用与迁移

安装并通过身份验证后，从平台正式入口打开应用，配对设备并选择远程画面。macOS 屏幕录制/辅助功能、Linux Portal 和 Android 投屏等权限使用正常系统同意流程。拒绝权限应显示诊断，不关闭加密或鉴权。

- 已有 Weavelo 的 macOS 升级使用 `scripts/install_macos_unified.py`，先生成计划。
- 首次旧身份迁移使用独立的 `scripts/migrate_macos_identity.py`；原 compatible-upgrade 门禁不接受不同 Bundle ID。
- Linux desktop/hicolor 首次安装使用 `scripts/install_linux_desktop.py`。目标独占创建；同内容既有文件只读保留，不同内容拒绝覆盖。
- 任何实际迁移都需要已审核的计划哈希、产物证据与备份。源码目录和默认交付名称变化必须经过官方生命周期事务，已有交付记录保持受保护。

历史 `start.sh` 和 `setup_stable_signing.sh` 已退役，不再构建拆分 host/client、生成签名身份或结束其他进程。

## 验证与文档

```bash
PYTHONDONTWRITEBYTECODE=1 python3 -B -m unittest discover -s scripts/tests -v
cd web
npm test
```

[可重复桌面验证](docs/testing/REPEATABLE-DESKTOP-CI.md) · [受控窗口输入](docs/testing/CONTROLLED-WINDOW-INPUT.md) · [HO5 Portal 接收流程](docs/testing/HO5-PORTAL-RECEIVER.md) · [迁移计划](docs/plans/WEAVELO-IDENTITY-MIGRATION.md)
