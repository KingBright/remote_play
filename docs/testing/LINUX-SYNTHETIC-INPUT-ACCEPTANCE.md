# Linux 合成窗口输入副作用观察器

`scripts/acceptance/linux_input_fixture.py` 复用现有 Linux 捕获 fixture 的 GTK3/PyGObject 环境，只观察它自己创建的一个测试窗口。没有全局键盘、鼠标、焦点或其他应用读取器，不创建输入设备，不申请系统权限，不连接 Weavelo 私有 profile，不发起联机会话。

它统计蓝色测试区域内的一次主键点击、该窗口的焦点，以及预先显示的八位公开测试数字串的按下/释放匹配进度。没有文本输入框，不保留原始键值、任意字符、剪贴板、按键事件列表或按键时序。非预期键被消费并忽略，不触发证据写入。`synthetic_text` 完全从公开测试串和匹配计数生成；真实输入来源不能通过 GTK 辨认，不能把此字段当作发送方归属证明。不要在测试窗口输入个人内容。

观察器不会在显示窗口时抢焦点，只在测试区域被点击后获取自己控件的焦点。输出为新建 0700 目录中的一个原子更新的 0600 `state.json`，单份最多 32 KiB；未知目录、pending、外部改写和链接文件均保留并拒绝覆盖。退出时间为 5–180 秒。关闭窗口仅销毁本次 fixture，不停止产品或其他窗口。失焦时如果仍有测试输入未释放，记录 `coverage_lost`，不会清空状态伪造释放。

## 不影响桌面的准备自测

在原 main 工作区运行纯模型和文件保护回归：

```sh
python3 -B -m unittest discover -s scripts/tests -p test_linux_input_fixture.py -v
```

在已有 PyGObject/GTK3 和正常桌面连接的 Linux 主机运行：

```sh
/usr/bin/python3 -B scripts/acceptance/linux_input_fixture.py self-test
```

这只使用 `Gtk.OffscreenWindow`，核对 GDK offscreen 类型后，经真实 GTK `widget.event` 派发带 `send_event` 标记的合成事件。不会显示桌面顶层窗口，不聚焦真实窗口，不通过 uinput、XTest、Portal、xdotool 或系统输入 API 注入。它验证 GTK 回调、点击、公开数字进度及焦点变化，但不验证 Wayland 合成器输入、产品传输或实际视频呈现。证据模式为 `gtk-offscreen-selftest`；`verify` 明确拒绝这种模式。缺 GTK/桌面连接时失败，不安装依赖、不改变会话配置。

## 配对后再执行：真实产品链路验收

以下命令尚不代表已运行或已通过。先在 MacBook 和 HO5 正常 UI 配对，并记录两端实际运行映像和 build identity；使用当前安装产品，不能把 standalone probe 的结果计入产品验收。确认本次唯一测试会话没有冲突，用户通过正常 Portal 只选择本 fixture，保留其他会话。

在 HO5 的主工作区终端启动自有窗口；输出目录必须不存在，若已存在则保留其证据，由原归属清理流程处理，不能直接覆盖或 `rm -rf`：

```sh
run_nonce="$(/usr/bin/python3 -c 'import secrets; print(secrets.token_hex(16))')"
/usr/bin/python3 -B scripts/acceptance/linux_input_fixture.py observe \
  --output-dir /tmp/weavelo-linux-input-acceptance \
  --nonce "$run_nonce" --challenge 73122905 --seconds 90
```

另一个已授权终端在发送任何输入之前保留基线；只复制本次合成 fixture 自己的证据：

```sh
cp -n /tmp/weavelo-linux-input-acceptance/state.json \
  /tmp/weavelo-linux-input-acceptance/before.json
```

后续自动发送端必须先绑定随机 nonce 标题、目标 fixture PID 与 `/proc` start ticks、实际源 selector/revision、会话及控制租约。通过安装产品建立该源视频，验证实际呈现的蓝色区域、公开数字串和计数。按实际源内容尺寸将蓝色区域中心映射到产品坐标；不能猜测整屏坐标或控制桌面上其他应用。

然后仅通过产品控制链路发送一次左键 down/up，等待 `clicks=1` 且窗口获得焦点；再发送公开数字 `73122905` 的逐键 down/up，不带 Shift/Ctrl/Alt/Super。数字串用于避免 IME 和键盘布局把字母映射差异误诊为输入失败。不能用本机直连注入替代产品输入。观察器不含发送入口，不能自行建立控制权限。

在窗口仍聚焦、八位进度完成且所有测试键/按钮已释放时，保留结果并校验：

```sh
cp -n /tmp/weavelo-linux-input-acceptance/state.json \
  /tmp/weavelo-linux-input-acceptance/after.json
/usr/bin/python3 -B scripts/acceptance/linux_input_fixture.py verify \
  --before /tmp/weavelo-linux-input-acceptance/before.json \
  --after /tmp/weavelo-linux-input-acceptance/after.json
```

校验要求相同 PID/start ticks、nonce、标题、公开测试串、backend 与观察器开始时间；基线无输入；180 秒内恰有一次完整点击、八次预期按下/释放、焦点成立，无覆盖丢失或未释放状态。它只证明该窗口观察到预期副作用，始终报告 `product_sender_attribution_verified=false` 和 `end_to_end_acceptance=false`。GTK/Wayland 不暴露发送者 PID；实际端到端验收须将这一结果与独立产品映像、会话/source revision、发送回执和实际帧呈现证据汇合，不能通过修改该标志“补齐”归属。

本轮范围不包括拖动、滚动、任意文本、修饰键或故意切到其他应用。这些仍需分别设计并验证。反向 HO5 → MacBook 使用已有 macOS 自有窗口与原生输入验证器，另外记录每个方向；Linux 观察器不能证明 macOS 或 Windows 的副作用。

完成后停止并释放本次产品控制租约与订阅，等待 fixture 自行关闭或仅关闭该窗口，核验双方原进程、原 profile 和其他会话未变。保留小证据；本工具不编译、不复制成品/缓存，也不清理未知文件或任何共享构建目录。
