# Linux原生导入、会话拒绝隔离与部署状态

## 实际交付状态

用户要求完成后续并部署全部设备。本轮完成了新的会话错误隔离修复及三端核心回归，Linux硬解帧直接导出与Vulkan真实平铺纹理导入验证。尚未完成完整GPUI Linux呈现、完整跨机功能/性能验收或任何新版本部署，不应把本记录理解成“全部完成”。

Mac捕获矩阵代码已准备，但对应Mac Studio源码同步、Host/App测试和候选构建调用被平台安全检查拒绝，没有执行。随后只进行了独立的合成网络单元测试、Linux本地原生视频工作及只读状态核查，没有换命令/设备/工具绕过同一候选构建。最终只读核查确认Studio目标候选目录没有创建，现用alpha.7与PID78134仍在运行。

## 会话数据报错误隔离

实际复现了一个缺陷：UdpReceiver正确拒绝明文伪造包后，Actor却把拒绝原因作为SessionCommand::Error发给界面。八个未经鉴权的伪造关闭报文之后仍能收到合法加密Pong，但测试捕获到被转交给界面的“plaintext packet rejected for encrypted peer”，旧实现断言失败。

现在区分UdpPacketRejection与真正Socket错误；被拒绝报文不再成为主机错误、不更新心跳、不改变会话或输入状态，只做限频诊断。合法、已经鉴权的主机错误仍原样传递。打开连接时也不再因坏包或无关数据耗掉重试次数，固定超时预算不延长。未修改包格式、共享网络身份、加密要求或路由选择。

相同反例修复后通过，并增加合法主机错误以及open阶段重试预算的正反校验。最终Core库：macOS248、Linux248、Windows246项全通过，0失败、0忽略。此结果是本地协议回归，不是实际P2P/Relay卡流根因全部确认，也不是完整跨机已修复。

证据：rejection-before.log、rejection-final.log；两台目标机器core测试日志。最终net.rs SHA为a0fc877c30577a6215cde208a8e843bb14e2f3de6a64d24da279c4baf07a84c5；workspace_session.rs SHA为1f729ac4b5721d3c00baf3d49add2415180bfec6f42719e64bfdee18354297b0。

## Linux真实硬解导出与GPU导入

取得与HO5现有FFmpeg8.1.2相匹配的官方源包头文件，在独立GPG目录验证签名及指纹，配置头文件但没有安装/替换系统FFmpeg、驱动或开发包。新增小型C ABI桥，只封装原生AVFrame/DRM对象所有权与解码库调用，不移走Rust的会话、队列、输入或调度逻辑。

使用VAAPI硬件像素格式，拒绝软件解码fallback；AV_HWFRAME_MAP_READ|DIRECT同步并导出DRM PRIME。未请求完整CPU像素缓冲。初步60帧导出/释放检查的自身FD数为4到4，验证结束没有保留测试进程。

真实解码器导出的资源带AMD modifier 0x200000010401b04，luma与chroma步长可以不同，不能将其冒充线性NV12或普通optimal Vulkan纹理。独立Rust验证程序核对同一个renderD128物理GPU、Vk格式/DRM modifier支持、每层offset/pitch、导入内存类型、资源大小和foreign队列所有权，再建立Vulkan图像与纹理视图。

8位与10位HEVC各执行三次。每次60帧硬解并直接映射；首帧的两平面在左上、中央、右下各抽查4×4区域，每平面48个像素，与同一个生成码流的软件参考逐值比较，所有差值为0。GPU只在该验证中读回144字节或288字节的小样本，没有把CPU参考/读回加入播放链路。测试使用自生成片段，不涉及用户屏幕、工作文件、网络或键鼠注入。

这些是独立Vulkan上下文验证，不是完整GPUI窗口。每次GPU像素验证仅首帧，其余59帧验证硬解/直接导出；不得称为“60帧全部GPU呈现”。测试未按真实播放节奏运行，不将几十毫秒批量处理时间换算成FPS或端到端延迟，没有监测scan-out、长稳或完整颜色合成。

证据：HO5 .rp-linux-native-20261003 下的direct-drm-first.log、vulkan-multipoint-{1,2,3}-{8,10}.log、linux-native-import-final.json。当前源与实际测试源摘要核对一致，独立验证Cargo.lock已保存到仓库；产品主Cargo.lock未改变。

## 当前Renderer接口边界

同一固定Blade0.7.1的public TextureDesc只有external handle，没有DRM modifier显式平面布局。其Vulkan create_texture默认optimal tiling，原生VkImage/Device/View字段为私有，未公开与foreign资源匹配的导入/所有权接口。不能直接复用普通texture constructor、伪造私有类型布局、改成CPU图像或换一个渲染器来冒充完成。

建议将已证明的原生导入规则以最小、固定版本的interop扩展接到这个实际后端，再由原GPUI scene负责遮罩、图层、画中画与输入。原批准范围明确不包含未经另行说明的其他图形依赖替换；本轮未修改/替换Blade依赖、未升级版本或引入新渲染引擎。这一新增依赖级修改范围应明确确认后再采用，不重新讨论已经获批的原UI恢复方案。

## 捕获转换信号

新增capture_yuv_matrix读取系统公开CG矩阵常量，实际常量只读值为ITU_R_709_2；准备让ScreenCaptureKit自身进行明确的RGB到YCbCr转换，不是猜测后重标记码流。不修改源色域和传递函数。此新增配置的编译、SCK实际捕获输出和联机编码回归没有执行，不能计为通过，代码保留为待验证改动。

## 设备/发布边界

MacBook与Mac Studio仍为alpha.7/build20260930.7，PID96760/78134；HO5 alpha.7 active/PID1899036/NRestarts0；Windows RemotePlay为0进程，UU Running/PID27892。测试未操作工作应用，没有重置TCC、设备身份、网络、签名或后台配置。未发布NAS、未生成新签名发行包、未替换安装，也未构建/安装新的Android包。代码没有自动提交/推送。

下一发布门槛仍包含：捕获与当前修复的候选测试；Linux后端真实图层呈现及播放worker接入；Mac/Windows/Linux真实持续与顺序切换；原有完整功能清单/输入/声音/文件对齐；同画质性能和资源回收/长稳；通过后逐机部署并验证运行版本。权限元数据支持执行不保证每条调用都通过平台审查，平台拦截与技术未完成是两个独立事实。

本轮来源主记录：/Users/jinliang/Workspace/.rp-complete-rollout-20261003/rollout-status.json。
