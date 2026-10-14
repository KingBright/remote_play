# 原版 GUI 恢复：现场进度核查

核查时间：2026-10-01T12:31:21.204105+08:00。本文核对现有代码、原始测试日志与实际安装状态，不将历史测试记作本次重新执行。

## 当前阶段

方案A-D已获用户“好的请完成”确认，不在等待重复审批。已从跨平台原控件探针推进到原产品视图重接现有会话的开发预览；尚未达到替换默认安装版本的条件。

app/src/restored_ui.rs 已接回原设计系统、设备抽屉、胶囊控制、来源选择、媒体设置、画中画与文件界面。app/src/desktop/original_owner.rs 将这些入口连接到现有鉴权WorkspaceConnection、来源版本、输入保护、文件身份绑定。源码入口存在不等于所有功能已经验收。

主程序仅在编译gpui-restoration且显式选择gpui-preview时进入该开发路径；默认仍为当前已安装界面。

## 已核对的最新成功证据

Mac Studio .rp-gpui-approved-20261001/visual-device-drawer-5/gui-receipt.json：原GPUI开发版连接本机实际运行的主机服务（LAN地址192.168.2.110:39271），选择专用合成窗口，30秒解码804帧、0解码错误、来源确认成功。实际窗口截图已存在于同目录restored-actual-window.png。帧记录边界是gpui_scene_submission，不是804次屏幕真正显示测量，也不是HO5跨机长稳验收。此路径使用Mac原生CVPixelBuffer surface，不走当前简化GUI的全帧CPU转RGBA路径。

Mac final-app-tests.log：107通过、3默认忽略；final-native-input.log和final-legacy-input.log各1项已单独执行通过。新GPUI测试模拟原GUI鼠标/键盘事件并记录协议，含控件覆盖隔离；没有向用户真实应用注入输入。Mac final-clippy.log通过。Core最终串行与重跑并行各231项通过，较早并行运行出现过文件/媒体优先级测试失败，不能据重跑通过推断长稳无问题。

Linux与Windows .rp-gpui-approved-20261001/app-tests-final.log各92通过、1默认忽略；existing-input-test-final.log中的该项分别单独通过。all-targets-final.log均编译检查通过。不能把这些当作两平台新的原生视频通路已可用。

foreground_runtime.rs 已增加原生事件循环与Tokio协作调度隔离，并保留UI调用线程；修复后Mac预览能持续更新。相关代码与测试存在，但不据此宣称整体性能最优。

## 仍然阻断发布

1. 恢复版控制器的真实Relay顺序设备切换未通过：restored-relay-sequence.json先得到Studio正确蓝色窗口，切MacBook记录0,0,0；restored-relay-sequence-B.json未取得Studio连续有效帧。根因尚未确认，既不能宣布成功，也不能直接断言就是旧设备错投复发。后续sequence-diagnostic-build.log证明诊断测试编译成功，没有新的通过回执。
2. restored_ui.rs 非macOS的decoded_video_frame_surface_with_fit仍返回Native video adapter pending提示。Linux/Windows原生视频资源适配尚未完成，是实质性功能缺口，不是只差打包。
3. GUI-PRESERVATION-CATALOG.json的31个功能组迁移验收仍为pending，完整功能、视觉交互、性能同条件对比与长时间测试尚未完成。Android没有新增实体机验收，未改换Flutter。

## 实际设备

两台Mac仍运行alpha.7/build20260930.7；HO5用户服务仍运行已发布alpha.7，PID1899036、重启0；Windows安装alpha.7，RemotePlay进程0，UU Running/PID27892。没有部署恢复版，没有将开发预览偷偷覆盖原安装。

本次仅做现场核查和更新本记录，没有重新运行上述测试、修改生产实现、切换服务、启动捕获或注入键鼠。现有构建与验收任务没有在后台继续执行。下一步优先修复恢复版Relay设备切换失败，再完成Linux/Windows原生视频通路，随后按31项清单和性能条件验收，最后逐设备切换。未经新确认不扩展到Flutter/协议/身份/权限变更。
