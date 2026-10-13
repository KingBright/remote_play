# P1 UI 复审：最终回归与生命周期补充

日期：2026-09-27。主仓库：`/Users/jinliang/remote_play`。
本记录补充 `P1-UI-REVIEW.md`，取代其中尚未结束的回归状态。代码与自动化检查完成，不等同于真机产品验收。没有提交、推送、部署或恢复 Windows RemotePlay。

## 续接后追加修复

1. **暂停中切源不再先启动采集再暂停。** `host/src/lib.rs` 在申请原生捕获、编码器及显示电源资源前等待恢复；期间保留零帧率存活回报。恢复采用最新分辨率与码率。取消或关闭设置通道则直接结束，不创建采集资源。
2. **恢复时同时改参数不再启动旧参数进程再立即重建。** Windows/Linux 先在停止状态更新参数，再执行恢复。
3. **编码器替换不再短暂并存两套进程。** Windows/Linux 共用先释放旧来源、再构建新来源的替换步骤。创建失败向上传递错误，不用旧编码器假装已经应用新参数。
4. **严格静态检查收尾。** 将 Host 测试模块移至实现之后，修复 `items_after_test_module`；没有添加 allow 属性来压制该警告。

新增测试包括初始暂停等待恢复、恢复使用最新参数、取消等待、不再有设置生产者时退出、旧资源先释放再创建、新资源失败后槽位为空，以及暂停时改设置不启动 FFmpeg。Windows/Linux 的 FFmpeg 替换顺序测试使用模拟资源，不采集真实桌面。

此前已核对并保留的 P1 修复包括：真实显示器 ID 与来源列表、主视口 Apps 来源选择、独立切源请求/回执匹配与超时、只读来源输入隔离、触控鼠标按键编号与 u16 归一化坐标、双指手势判定与取消尾部、自隐藏工具条的局部鼠标遮挡。

## 最终源码标识

Rust 构建快照覆盖 120 个文件，标识：
`65f5883eab215dbbb41ba393097bde59cca2a0067c8524cd23fe1cea668773a3`。

归档 SHA-256：
`20179048b5cb0258d888e1240679d7f39550dc722880a0124ac17325b1399b7f`。

Windows 和 HO5 对各文件逐一校验 SHA-256 后才构建；主仓库最终再次核对一致。Android 本轮读取的五个输入文件也核对未被并发修改。跨平台验证使用独立源码目录，不覆盖原安装。

## 已退出的验证结果

| 环境／检查 | 结果 |
|---|---|
| Mac App 库测试 | 65 passed / 0 failed |
| Mac Host 库测试 | 26 passed / 0 failed |
| Mac Protocol 库测试 | 33 passed / 0 failed |
| Mac Bridge（android feature）库测试 | 29 passed / 0 failed |
| Mac client_session 专项 | 11 passed / 0 failed |
| Mac App + Host + Bridge all-targets Clippy，`-D warnings` | 退出 0 |
| Windows App 库测试 | 49 passed / 0 failed |
| Windows Host 库测试 | 30 passed / 0 failed |
| Windows Protocol 库测试 | 33 passed / 0 failed |
| Windows Bridge（android feature）库测试 | 29 passed / 0 failed |
| Windows App + Bridge all-targets check | 退出 0 |
| HO5 原生 Linux Host 库测试 | 15 passed / 0 failed |
| HO5 App all-targets check | 退出 0 |
| Android compileDebugKotlin + testDebugUnitTest | 退出 0；24 项测试无失败 |
| 最终 git diff --check | 退出 0 |

Android 的 24 项为：ViewportTransform 11、HostDeviceActions 8、VideoDimensions 4、BackgroundConnectionPolicy 1。JVM/Kotlin 编译与单测不等于 APK/NDK 构建或真机触屏测试。Bridge 的 android feature 检查在桌面目标执行，不冒充 Android ARM64 原生库验收。

Mac 库测试合计 153，Windows 库测试合计 141；大量测试跨平台重复，不能相加作为独立功能覆盖数。本轮执行的是列出的目标检查，不宣称全工作区/所有平台/所有场景全部通过。

Windows 构建仍输出已有 unused import / unused field 等警告，Android 仍有 CameraX 旧 API 弃用及 Gradle 提示。通过严格 Clippy 的范围是上述 Mac 目标，不声称全平台零警告。

## 可核对的操作回执

- Mac 最终库测试与 Clippy：`02c57eed-0abc-4b1f-b6e2-606ed83eca9f`。
- Mac client_session 专项：`b3b612a0-c356-4900-824f-cd65468298f7`。该操作中的早期 Clippy 因测试模块排列失败，后由最终操作修复并通过，不把早期失败隐藏为通过。
- Windows 最终同源测试与检查：`02943d76-0ca1-4012-b61d-ab8791bab000`。
- HO5 最终同源测试与检查：`c17221ad-c8a7-43a8-9d96-d6546b0bca52`。
- Android：`7970a0b4-5674-44f7-a5b1-c206fe6cc997`。
- Windows 最后状态：`5373b669-2015-434b-b84c-01d822c628f3`；RemotePlay 进程数 0、自启动 false、UU 服务 Running。
- 源码一致性复核：`2da26b88-c4fd-40b6-94c5-15adf65ea4b4`。

完整本地日志：Mac 的 `/Users/jinliang/Workspace/.rp-p1-review-20260927-cont/`；Windows/HO5 的工作区 `.rp-p1-final-20260927/`。HO5 原用户服务保持 active、MainPID 2369、NRestarts 0；本轮没有重启它。

## 未进行的产品验收

- Android 实机捏合、横竖屏、SurfaceView 缩放与触点一致性。
- 真正多显示器切换的首帧时延、连续切屏长稳以及网络丢包下的画面恢复。
- Windows RemotePlay 与正在串流的 UU 的受控共存 A/B。
- 新 APK、正式桌面安装包构建与发布。

`SourceSwitched` 是主机接受切源的回执，不等于新来源首帧已经显示；副屏与应用窗口的控制仍按后端公布的能力限制。当前 Windows 安全后端不会因为 UI 有来源菜单就自动获得完整多显示器/窗口捕获能力。

产品行为调整明确保留：远程双击继续传给远端，视口复位用 RESET 控件，避免同一次双击同时操作远端文件与本地缩放。1x 的双指平移是远端滚动，识别为捏合后由本地视口接管，放大后的双指平移移动本地视口。
