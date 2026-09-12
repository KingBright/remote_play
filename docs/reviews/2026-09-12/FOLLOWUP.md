# 第二轮 Review 与优化

对应“还有哪些缺口？请继续优化”。保留第一轮改动，继续处理偏好持久化、移动端会话、Android 解码和实际界面问题。修改尚未提交或部署。第一轮传输基准没有重跑，本轮不宣称新的整体提速百分比。

## 已完成

| 优先级 | 原问题 | 结果与验证 |
| --- | --- | --- |
| P1 | 偏好设置多处 load/modify/save，固定 `.tmp` 导致竞争和字段覆盖 | 所有运行时写入改用加锁的整次更新；稳定的同目录锁文件支持进程间互斥；唯一临时文件写入、同步后原子替换；80 次并发累加无丢失，8 个独立字段全部保留 |
| P1 | 配置损坏后可能用默认值覆盖，写入失败留下临时文件 | 更新明确返回读入/解析错误；验证损坏内容不变、替换失败保留目标且清理临时文件；无实际变更不写盘；原有运行时 owner 单测的默认偏好路径也隔离到临时目录 |
| P1 | 连接覆盖会话句柄，旧收发和遥测任务继续运行；StopStream 入队后立即取消发送器 | 串行管理连接生命周期，取消并等待全部六类会话任务退出，再直接限时发送 StopStream；清除音视频队列和旧统计；本地 UDP 模拟主机验证替换顺序、全部旧任务结束和断开消息到达 |
| P1 | 任意 Annex B 起始码被识别为关键帧；积压后继续提交缺失参考帧的预测链 | 按完整 HEVC 头识别 IRAP 16–21；覆盖全部 64 种 NAL 类型及坏头；8 帧队列溢出后清空预测链、请求关键帧，从关键帧恢复；真实 UDP 回归验证该过程 |
| P1 | 发出 StartStream 就显示 Streaming；视频失联可被新遥测掩盖 | 收到可用关键帧才进入 Streaming；JNI 将真实状态回传界面，后台线程执行连接与重试；12 秒无视频触发恢复；已建立会话收到新的零 FPS 遥测时允许静态桌面空闲，避免无意义重连；取消后不继续重试 |
| P1 | Android HEVC 失败后直接改用 AVC，配置失败/停止失败泄漏资源；视频与音频错误静默 | 保持 HEVC 协议要求并显示解码失败；候选 codec 失败释放，stop 与 release 独立执行；每个 Surface 使用独立取消标记，媒体线程 finally 清理；音频错误单独显示并保留视频 |
| P1 | JNI 丢失关键帧信息与时间戳；喂入缓冲不足会吞帧；音频写出阻塞视频 | JNI 保留关键帧标识和原始 PTS，Kotlin 按偏移喂码流，避免再复制完整视频载荷；单个待提交视频帧最多等待 100 ms，失效后请求关键帧；检查输入容量，空闲时也排出解码结果；音频直接读取输出 ByteBuffer，非阻塞写出，补全 Opus 初始化数据 |
| P2 | Android 缺失插件版本与 Gradle wrapper，未启用 AndroidX，QR 页面存在编译错误 | 固定 Gradle 8.13、AGP 8.7.3、Kotlin/Compose 插件 2.0.21；校验 Gradle 分发 SHA-256；补齐 AndroidX 与 QR 扩展调用；ARM64 JNI、APK 和 lint 通过，构建步骤见 android/README.md |
| P2 | Android 点按没有发送事件，竖屏画面被拉伸，断开及部分虚拟键被挤出屏幕 | 点按/移动/抬起/取消均有对应事件，按实际视频区域映射；视频保持协商的 16:9；顶部按钮换行，虚拟键横向可滚动；避让系统栏；模拟器横竖屏和断开流程验证 |
| P2 | 空发现列表伪造 Loopback 主机，硬编码 Mesh 地址和显卡名，未接入控制仍可切换 | 增加手动 IP:port 连接与空列表提示；去掉虚构设备信息和无动作入口；MIC/CLIP 标记不可用；未测量的码率显示 Not measured/Unavailable；Decoded FPS 来自实际 codec 输出计数，不再把主机 FPS 或编码耗时当作客户端呈现指标 |

## 验证与证据

- Rust workspace/all-targets：**379 项通过**，见 `followup-rust-tests.txt`。在偏好路径隔离后重新执行完整工作区测试。
- JNI 功能构建下的桥接测试：**12 项通过**，见 `followup-bridge-tests.txt`。这些项目与 workspace 结果有重叠，不相加。
- 严格 Clippy：workspace/all-targets 通过；Android feature 也进行了检查。证据见 `followup-clippy.txt`。
- Android ARM64 原生库重新从最终源码构建并打包。`assembleDebug`、`lintDebug` 通过，**0 errors / 11 warnings**；警告为 target SDK 和依赖版本更新提示，未据此批量升级全部依赖。见 `followup-android-build.txt`、`followup-android-native.txt`、`followup-android-lint.txt`。
- Android API 35 / ARM64 模拟器实际连接本机 macOS 主机，经原生 UDP/JNI/MediaCodec 路径解码桌面画面；检查手动连接、横竖屏比例、Surface 重建、断开和重新连接。主机 StopStream 和 UI 观测摘要见 `followup-android-runtime.json`。这次不是 Web 测试网关。
- 补丁通过 `git diff --check`；偏好模块和桥接 Rust 文件通过 rustfmt。app 的 lib/ui 仍有既有格式差异，没有为了本轮修改重排整份 UI。

解码帧率按 500 ms 单调时钟窗口内实际释放到 Surface 的输出缓冲数计算。静态桌面可能没有新输出，0 有实际含义；该数字不是显示器扫描刷新率，更不是端到端延迟。没有用模拟器数据证明真机 60 FPS、4K/120 FPS 或 sub-3ms。

最初 OpenJ9 JDK 17 下 Gradle daemon 两次意外退出；换用 HotSpot JDK 17、4 个 worker 和 2 GiB Gradle 堆后构建通过。此处仅记录可复现的成功配置，不把退出原因断言为某个已定位的 JVM 缺陷。

## 仍然存在的缺口

1. **Web 原生链路**：现有浏览器客户端仍需要 relay 协议适配、发现和音频/文件等通道接入。第一轮的 WebCodecs 网关验证不等于 native relay 联调。
2. **编解码与协商**：Android 当前要求 HEVC，使用固定 1920×1080 请求；AVC 协商、设备能力选择、分辨率切换和更完整的音画同步仍需实现及验证。零 FPS 遥测只能作为空闲信号，不能独立证明采集器健康。
3. **跨平台验收**：本轮构建和运行覆盖 Android ARM64 模拟器；x86_64 Rust Android target 未安装，原生脚本明确跳过该 ABI。Windows/Linux、Android 真机硬件解码、扬声器、输入注入、屏幕锁定/后台恢复、温升与功耗尚未完成验收。
4. **长稳与量化**：仍需丢包/抖动/断网注入、长时间运行、音画同步误差、内存曲线及端到端 p95/p99 时延证据。本轮的单测、模拟器联调和第一轮本机回环微基准不能替代这些数据。

构建产物是本地 debug 验证产物，不是已发布版本。没有提交、推送、发布或替换线上服务。
