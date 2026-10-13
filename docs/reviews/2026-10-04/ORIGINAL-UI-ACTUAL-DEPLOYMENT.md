# 原版 GUI 的实际部署：alpha.8

## 当前真正发生的变化

HO5和Mac Studio已安装并运行原版GPUI桌面产品，不是控件演示程序或gpui-preview开关。版本alpha.8，Mac构建号20261004.80；实际main二进制、服务/启动入口与运行实例renderer均核对。HO5正常入口重复启动保留同一PID1520582；Studio正常打开RemotePlay.app复用PID25060。旧包/启动配置均有回滚备份，网络身份与TCC授权没有重置。

alpha.8发布包默认restored-original-gpui，Mac/Linux/Windows原生视频feature逐平台构建并检查。简化egui只保留显式诊断入口，不允许自动回退，也不允许新原版界面只唤回旧诊断进程后报告成功。

## 尚未完成的两台电脑与手机

MacBook仍运行alpha.7/build20260930.7。升级器在核对持久配置时遇到Permission denied：用户目录NativeMesh中的mesh.conf和mesh.secret实际owner_uid=0、mode=0600；目录属于用户501。安装保护正确中止，没有跳过身份校验或重建网络。只读元数据诊断后，尝试使用已授权的非交互管理员能力进行两文件归属修复，但该能力不存在，因此chown没有执行，没有改变权限位或内容。需要用户在MacBook本机以管理员身份将这两文件归属恢复为当前用户，再由正常用户运行固定签名升级器。不要以sudo整体运行应用/安装器导致文件再次归root。

Windows cube仍为旧alpha.7安装，更新exe已经构建/验证并发布。第一次输入专项缺少测试进程旁的私有FFmpeg；通过显式指定已安装的固定摘要私有runtime重跑通过，未改产品逻辑或减弱测试。PowerShell脚本加载策略拒绝.ps1校验器，没有修改执行策略；随后使用实际exe的无副作用product-info命令核对全部预期字段。真正的覆盖安装调用被平台安全检查拒绝，没有执行，未用拆分命令、其他工具或新设备绕过该安装。Windows新包可供用户常规覆盖或便携运行，不能写成设备已升级。UU及旧安装没有被改动。

Android实体设备离线；alpha.8正式签名ARM64 APK完成Release/Lint/JNI/16KB原生库/签名验证并发布，versionCode9，不将发布算成手机已安装。没有替换移动端UI为GPUI。

## 本轮回归与验收边界

Mac：Client34/Host52/Core248/App122及独立原版输入专项1通过。Linux：Client33/Host18/Core248/App108及原版输入专项1通过。Windows：Client33/Host33/Core246/App108及原版输入专项1通过。默认忽略的设备专项未计为通过。发布/签名/GUI入口相关Python测试80通过。

本轮复查的实际旧开发版HO5截图是原GPUI设备抽屉/控制胶囊，而不是egui；其渲染源码与alpha.8相同，只有发布号变化，但未将旧截图冒称安装后的新截图。升级后的截图/拉流测试未取得新通过回执：X11截图运行未继承授权环境而失败；后续正常Wayland读取专用测试源的调用被平台拦截且未执行；Studio截图元数据helper超时，截图命令未执行。正式HO5服务保持正常Wayland，未为截图修改显示授权，也没停正常程序。此前相同渲染源码经Relay真实799帧的记录仍是此前结果，不冒充本轮安装后的验收。不得把当前运行renderer标识、单测数和编译能力等同所有功能/长稳/端到端性能通过。

## 发行保护与修正

三端来自同一编译source_id 67bf20c6295b898e95c803c7898ec7f5e547bb162bae6d3d7c90fe42dfd37c33。仅app产品版本与Android版本码升级，没有改变线上协议版本。依赖、网络隔离、原生视频、输入安全实现沿用核对的源代码，不回滚到旧通信路径。

Mac返回的远端构建通过固定摘要和原GUI身份验证后，经正常package_macos_unified.sh签名，不导出签名私钥、不采用ad-hoc签名。为此新增可选prebuilt输入和4项测试，普通构建流程不变，产品/版本/签名gate不跳过。编译来源与发行脚本的小变更分别记录。

最初设置build20261004.8低于Studio已安装20261004.71，被防回滚正确拒绝。已将未发布包隔离，使用同一验证二进制重新签名为单调更大的20261004.80，未弱化版本保护。HO5第一次安装验证误期待renderer.json包含版本字段，触发回滚；修正为用已校验exe/ExecStart核对版本、renderer.json仅核对renderer后再次安装成功。

## NAS

统一入口 https://relay.hackerlife.fun:8443/download/ 已包含alpha.8三桌面包和Android签名包。三桌面包均重新从公开URL获取并校验SHA。Mac包未做Apple公证；Linux包声明HO5匹配的系统库依赖，Windows包含私有FFmpeg及许可证且默认关闭本机capture/autostart。旧发行文件未覆盖，新包不包含设备密钥、证书私钥或系统字体文件。

macOS: RemotePlay-macos-arm64-2.0.0-alpha.8-20261004.80.zip。
Linux: RemotePlay-Linux-x64-2.0.0-alpha.8.tar.gz。
Windows: RemotePlay-Windows-x64-2.0.0-alpha.8.zip。
Android: RemotePlay-Android-latest.apk（当前alpha.8/versionCode9）。

## 证据

主目录 /Users/jinliang/Workspace/.rp-original-release-20261004，含source-manifest.json、各失败/签名/发布日志、final-deployment-status.json。Linux/Studio/Windows对应工作目录中的同名stage保存平台实测与安装回执。源码未提交推送。全部平台部署尚未完成；实际安装成功仅HO5和Studio。
