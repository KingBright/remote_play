# 流参数表单的纯状态切片（2026-10-08）

父提交：`79379dc8659d17a78e265206c81213ac3afe3ee9`。本轮从已批准的迁移计划和 `GUI-PRESERVATION-CATALOG.json` 的 `media_settings` 项选择分辨率、FPS、码率表单；DeviceDrawer 和会话标签模型不再修改。默认产品继续使用既有单一 GPUI CE + Ely，没有新增框架、依赖、运行时或全局锁。

## 接线与行为

`remote_core::stream_settings` 保存已确认的参数、两项输入草稿、错误和效果票据，只包含数值与文本。ViewModel 借用草稿与错误；原生帧、GPU 资源、连接和持久化仍在原来的适配层。

| 实际入口 | 迁移后的生产路径 |
| --- | --- |
| 从偏好初始化 | 按原字段创建 StreamSettingsState，不改偏好结构或已有值 |
| FPS、码率输入 | EditFps / EditBitrate 只更新草稿，不写偏好、不请求远端；控件投影读取草稿，避免重绘时用旧确认值覆盖编辑内容 |
| Apply custom values | ApplyCustom 使用原 `protocol::validate_video_settings`；无效输入保留确认值并显示原错误，合法输入提交参数并产生效果 |
| 三组预设 | SelectResolution / SelectFps / SelectBitrate 只改对应参数；FPS、码率预设同时同步各自草稿，保留另一项草稿 |
| 效果执行 | UI 适配层沿用 persist_preferences 和 OriginalOwner.update_stream_settings；预设继续尝试更新并静默处理失败，自定义提交继续仅在 Viewing/Connecting 时更新并显示失败 |
| 异步回执 | 新命令替换旧未执行效果；编辑下一份草稿不取消已确认效果。命令与草稿票据阻止旧错误覆盖新编辑，执行前和错误返回时核对既有连接对象，拒绝切换会话后的陈旧回执 |
| 新连接、切换来源、抽屉高亮 | 读取模型确认值，继续使用原 StreamStartOptions、来源切换和渲染入口 |

输入 ID、按钮标签、预设数组、尺寸、层级和控件类型不变。暂停、后台暂停、音量、静音、适配/填充、来源选择、遥测、原生视频和输入实现未迁移。输入组件与 IME 实现未修改；真实输入行为尚未验收。

## 固定小回归与增量检查

```sh
python3 scripts/test_stream_settings_model.py --target-dir /Users/jinliang/rust-target
CARGO_TARGET_DIR=/Users/jinliang/rust-target cargo check --offline --locked -p remote_play_app --lib --bin remote_play
```

固定脚本实际只运行 4 项模型测试，4/4 通过、260 项过滤：草稿投影与确认值隔离；非法值及真实协议边界；预设对应草稿同步；新草稿、新命令与迟到错误的交错。测试不启动 GUI、网络、捕获或输入，不写实际偏好。脚本必须使用已有 target，并在运行中守住 4 GiB 可用空间；到达底线只停止自己创建的构建进程，不清理缓存。

最终主包 lib/bin 增量检查成功，13.21s，保留 9 项现有告警。最终检查期间三项编译源码 SHA-256 保持稳定，提交前再次匹配。测试后的纯模型与协议源码保持稳定；没有重跑上一阶段的宽范围测试，没有新建 target。

保留核对：105 项原有无关改动的状态与 SHA-256 全部一致。OriginalOwner、原生 SessionState 所在 model、original_presenter、session_tabs、WorkspaceConnection、协议校验、preferences、design_system、TextInput 和根/app/core Cargo 清单及 Cargo.lock 与父提交逐字节一致。UI 来源切换只替换参数读取；输入区块、视频区块经相同读取替换归一化、暂停/音量区块和三组预设样式前缀均与父提交一致。检查完成可用空间约 4.40 GiB，后续提交前核对约 6.71 GiB，未执行缓存清理。

## 验收边界

本轮证据限于纯状态回归、实际入口接线、源码保留核对和编译接口。真实 GUI 草稿/IME 交互、连接切换时远端参数生效、实际呈现、输入转发、音频、长时性能及物理平台仍未测；静态样式核对不等于视觉验收。未启动 GUI/真实会话、终止旧 GUI PID、改配置或身份、读取密钥、签名、推送或部署，已安装程序没有变化。

本轮本地提交白名单为：`remote_core/src/stream_settings.rs`、`remote_core/src/lib.rs`、`app/src/restored_ui.rs`、`scripts/test_stream_settings_model.py` 和本记录。
