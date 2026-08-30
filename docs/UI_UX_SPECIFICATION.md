# RemotePlay 跨平台 UI & UX 设计系统总览 (Desktop & Mobile)

本文档归档了 RemotePlay 全平台的 UI & 交互设计系统。

---

## 目录索引

1. **桌面端设计规范 (GPUI / macOS / Windows / Linux)**
   - 核心原则：以画面为绝对主体（Screen-First / 100% Canvas）
   - 顶部悬浮动态控制岛（Dynamic Island Capsule）
   - 左侧滑出式半透明管理抽屉（Slide-over Glass Drawer）
   - 独立画中画弹出小窗（Pop-out PiP Window）
   - 5 信道并发网络遥测与延迟瀑布流 HUD
   - 详见本文档下文及 [walkthrough.md](file:///Users/jinliang/.gemini/antigravity/brain/a55ce59b-1cef-4f11-9fe6-01ad1cf11456/walkthrough.md)

2. **移动端设计规范 (iOS / Android / iPadOS)**
   - 竖屏控制中心 (Mobile Portrait Dashboard)
   - 横屏 100% 满屏远程触控操控 (Mobile Landscape Screen-First Viewport)
   - 触控与手势模式体系（直触 Direct Touch / 虚拟触控板 Virtual Trackpad / 虚拟手柄 Gamepad）
   - 移动端扫码免密加入 EasyTier Mesh 组网
   - 详见完整文档：[docs/MOBILE_UI_UX_DESIGN.md](file:///Users/jinliang/remote_play/docs/MOBILE_UI_UX_DESIGN.md)

---

## 🎨 视觉语言与设计系统 (Obsidian Stream)

| 设计元素 | 规范定义 |
| :--- | :--- |
| **主背景 (Base Surface)** | Obsidian 深黑 `#131313` / `#050706` |
| **毛玻璃浮层 (Glassmorphism)** | 65% 不透明深色背景 + `backdrop-filter: blur(20px)` + 1px 细微高光微边框 |
| **主品牌色 (Cyan Accent)** | `#00F2FF`（用于激活指示、连接按钮、重点数据流） |
| **健康/在线色 (Emerald Green)** | `#00FF41`（用于 120FPS 稳定状态、低延迟、Mesh 正常） |
| **警告/警示色 (Amber / Coral)** | `#FFB4AB` / `#FF6B4A`（用于网络抖动、掉帧、提权提示） |
| **技术字体 (Typography)** | UI 标签：`Inter`；遥测/延迟/键码/邀请码：`JetBrains Mono`（等宽防抖动） |
