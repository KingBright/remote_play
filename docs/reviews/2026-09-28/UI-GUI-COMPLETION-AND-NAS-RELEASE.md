# RemotePlay UI/GUI 复审与 NAS 发布收尾

## 本次交付结论

2.0.0-alpha.2 的 Android、macOS、Windows 主机端、Linux 主机端安装包已经发布到现有 NAS 下载目录。已验证版本化地址的公开下载内容与本地产物 SHA-256 一致，并复核 Android latest 链接。下载页保留历史版本，包含电脑端下载和真机验收清单。

这是构建、打包与分发完成，不是全平台全部功能或所有真机交互验收完成。没有替换四台电脑或手机上已安装的应用，没有修改设备网络、UU、驱动或自启动。

## Anti Gravity 完成度与实际边界

- macOS：主视口、抽屉、工具条、遥测与来源选择的 UI 主干已实现；后续复审修正实际来源 ID、切源回执、只读输入保护、点击穿透与生命周期。
- Android：四个实际导航页面、配对、视口、缩放和平移、坐标逆映射、暂停/恢复与错误显示已实现并经过定向修复。本轮增加可折叠工具、默认隐藏修饰键栏、粘滞修饰键及停用/退出时释放，避免把按键面板做成无效的一次性修饰键点击。
- 软件键盘保留 IME 组合态，但仍不是完整任意 Unicode 注入方案。不能宣称中文、emoji、任意输入法已完整验收。
- Windows/Linux 的当前 App 目标是主机端，完整观看 GUI 仍只在 macOS 目标编译。下载页与包内说明明确标注 host-only，不以跨平台编译成功冒充跨平台 GUI 完成。
- iOS、虚拟游戏手柄和设计稿中的部分浮层交互，不属于已经交付的完整功能。

## 本轮发布可靠性修复

1. Android versionCode 从 2 升为 3，版本统一为 2.0.0-alpha.2，签名沿用已发布包，避免需卸载重装。
2. 重建 Rust JNI，仅分发完整的 ARM64 ABI。旧包 x86_64 目录只有第三方库而没有产品 JNI，不再将它误作为完整支持的 ABI。
3. JNI 动态符号表校验：25 个 Kotlin native 声明均有导出，并校验 APK 内产品库与此次 staged .so 哈希相同。
4. 原生产品库与 CameraX 库均满足 16 KB ELF 对齐，APK zipalign 校验通过。
5. 发布检查不再使用会解析系统图标资源的 aapt badging，改为读取编译后的 AndroidManifest.xml 结构。保留应用 ID、版本、debuggable、签名、ABI 和 JNI 校验，没有忽略工具非零退出码或放松安全检查。
6. 新增 8 项 manifest 校验器测试；实际旧 debug APK 确认被拒绝发布。签名固定核对原发布证书指纹。
7. NAS 使用不可覆盖版本化产物、原子替换元数据、保留历史版本，并逐包公开下载复核 SHA-256。

## 已完成验证

| 范围 | 结果 |
| --- | --- |
| macOS App/Host/Protocol/Bridge 库测试 | 65 + 26 + 33 + 32 = 156 项通过 |
| macOS 对相关包的全目标 Clippy | -D warnings 通过 |
| Windows App/Host/Protocol/Bridge 库测试 | 49 + 30 + 33 + 32 = 144 项通过 |
| Linux Host/Protocol/Bridge 库测试 | 15 + 33 + 32 = 80 项通过 |
| Android JVM 单测 | 30 项，0 失败、0 错误 |
| Android assembleRelease / lintRelease | 完成；签名校验通过 |
| Android 发布校验器 | 8 项通过；debug APK 拒绝用例通过 |
| macOS/Windows/Linux Release 包 | 全部构建完成 |

上述平台测试存在重复，不能相加为独立功能覆盖总数。Android Lint 仍有非阻断提示，不能宣称所有平台零警告。

## 尚需真机验收

14 项 Android instrumentation 用例已编写并在上一轮编译；模拟器启动 ANR 的执行没有通过，本轮未将其改写为成功。手机真实 SurfaceView/MediaCodec 渲染与触点一致性、横竖屏、多显示器首帧/长稳和 Windows/UU 并发串流仍需受控验收。

建议先更新 Mac 主机和 Android 配套版本，用 Mac 验收 UI/交互，不先启动 Windows 与 UU 并发串流。macOS 包使用 RemotePlay Local 本地签名，未做 Apple 公证。

## 已发布文件

- `RemotePlay-Android-2.0.0-alpha.2-f4c7212-release.apk`，15516277 bytes，SHA-256 `52b833b5beaecf7ae0a9d658d6b6652327874df1a8377e20f83ccd0800d1d508`。
- `RemotePlay-macos-arm64-2.0.0-alpha.2.zip`，7002284 bytes，SHA-256 `478491a5aed914b5d53571d522e388dc55e2ba78012bd35d558b8bad056b7988`。
- `RemotePlay-Windows-x64-2.0.0-alpha.2-host.zip`，41839602 bytes，SHA-256 `455c90abc44a9943edac10dd85206b7ea7aa7ce7be8e8da93126e9408efd65d2`。
- `RemotePlay-Linux-x64-2.0.0-alpha.2-host.tar.gz`，3496000 bytes，SHA-256 `3a021457373206e3d274ee94e707fd0f3c2e8ca4c9b57dab0c485d8686974509`。

下载页：https://relay.hackerlife.fun:8443/download/

验收清单：https://relay.hackerlife.fun:8443/download/acceptance-2.0.0-alpha.2.html

冻结编译源标识：`65f2253cec3da691a4031eb39e63855585ff7407b2d595dea8d6b97a477a6372`。文件名中的 f4c7212 是基线提交，不代表未提交修复不存在；release.json 明确记录 commit_is_base 与 source_id。

源码保留在 `/Users/jinliang/remote_play`，未自动提交/推送。最终回执保存于 `Workspace/.rp-ui-nas-20260928/final-nas-release-receipt.json`。
