# 显式共享文件与主动拉取

日期：2026-09-24。接续入网改造与独立文件接收。基线是 MacBook `main@f4c7212` 的未提交工作区，不是已经发布的新版本。

## 本轮完成的源码路径

用户在桌面或 Android 工作区主动选择文件并加入共享清单，另一端查询清单，按文件 ID 请求取回。清单与请求使用现有已鉴权会话的文件通道；文件内容继续使用原来的调度器、窗口重传、接收端校验、原子落盘和最终确认。无需开启视频订阅。

桌面工作区增加 Share files with network、Clear my shared list、Retrieve shared files，以及按清单取回的入口。Android 工作区提供对应的文件选择、清单翻页与取回操作。手机要接受新连接，仍需显式开启独立文件接收或已有屏幕共享服务；单纯加入共享清单不自动启动服务，也不获取屏幕权限。已建立的文件会话可用于双向请求。

这是显式文件清单，不是任意远端目录浏览器。用户选中的文件才会暴露名称和大小，远端请求不包含本地路径。共享对象保留本地选择时打开的只读文件句柄，后续同路径替换不会把拉取重定向到其他文件；不是内容冻结快照，原文件仍可能被本地程序修改。大小变化会要求重新发布，同大小内容变化仍由传输完整性检查把关。

## 边界与约束

- 共享清单仅本进程有效，按设备网络密钥隔离；换组清除清单。清除后拒绝新拉取，已经受理的传输可以完成。没有默认扫描磁盘、目录遍历或上传整机文件。
- 当前最多 64 个显式共享文件，每页 8 项，单个文件名最多 128 UTF-8 字节。同一个文件的读取被串行化，防止复制的文件句柄共享偏移造成错误；不同文件可并发。继续沿用至多 4 项发送任务及既有单文件大小限制。
- 请求每 500 ms 重试，8 秒未收到确认则报告超时；每会话最多 8 项未确认请求、256 个请求 ID。请求 ID 不复用，接收端保存去重记录。超出容量要重新建立会话，不无限增长内存。
- 旧控制枚举序号保持不变，新变体追加。旧端不支持新请求时会出现明确超时提示；既有“支持文件传输”广播不等于支持拉取扩展，当前通过实际请求确认；未声称完成所有历史版本互操作验证。
- “请求已受理”与“文件已落盘”分开。文件成功仍依赖原传输完成/最终确认事件。选择器的目标会话与发送/共享模式固定，切换设备后不会将旧选择误投到新会话。
- 本轮没有实现递归授权目录浏览、目录树同步、跨进程断点续传、定向投屏邀请、多入站对端或全平台互控。
- 真实跨网络、长后台、大文件等待首个清单时的保活、功耗与长时间稳定性仍需专项验收；本轮小文件回归不代表这些条件通过。

## 验证

验证环境为 Mac Studio `/Users/jinliang/workspace/.remoteplay-verify-b2`。所有 Cargo 命令在该目录运行，加载仓库内 `.cargo/config.toml`；从仓库外仅给 manifest 会丢失该构建上下文，不能混用缓存结论。

1. `cargo test -p remote_core -p remote_client_bridge --features remote_client_bridge/android --lib --locked --offline -j2 -- --test-threads=2`：core 219、bridge 21，共 240 项通过。回执 `d7fb1416-38f6-46d4-ba5c-7e35c842c933`，日志 `b3-tests.log`。
2. 新增实际 loopback UDP 测试故意丢弃一次清单回复、重复 Fetch，请求重试后成功接收 262144 字节并逐字节一致，发送端只启动一次传输。清空清单后新拉取拒绝；未知 ID 和不同网络的文件不暴露。
3. 选定文件随后被替换路径的测试仍读取原句柄；未显式发布、无网络身份或其他网络的文件不在清单中。
4. 下方记录桌面全目标检查、JNI Clippy 与协议测试的真实退出码。这些不是 Android ABI 构建结果。

- desktop: exit=0, timeout=False, log=`b3-desktop-verified.log`.
  `Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 03s`
- clippy: exit=0, timeout=False, log=`b3-clippy-verified.log`.
  `Finished `dev` profile [unoptimized + debuginfo] target(s) in 54.16s`
- protocol: exit=0, timeout=False, log=`b3-protocol-verified.log`.
  `Finished `test` profile [unoptimized + debuginfo] target(s) in 1m 30s`
  `test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`

## Android 与发布状态

Android Kotlin/JNI 接口和 UI 源码已接入，但本轮未获得完整 Android 编译、Lint、原生 ABI 打包与真机操作的通过证据。MacBook 的 Java 版本输出成功，Gradle 版本查询仍超时；没有据此发布新 APK。已有 APK 不包含本轮功能。

源码回写时必须先对所有目标检查旧 SHA-256，再逐文件原子写入并检查新 SHA-256。主仓库的既有改动，包括 NAS 落地页修改，保持不变。本轮不执行 Git 提交、推送或线上发布。
