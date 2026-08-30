# RemotePlay 移动端 (iOS / Android / iPadOS) UI & 交互设计规范

> [!IMPORTANT]
> **跨端统一设计哲学 (Unified Obsidian Stream Architecture)**：
> 1. **严苛的工业级极简美学**：彻底消除杂乱霓虹光与廉价按键堆砌，采用与桌面端 100% 统一的 **Obsidian 深空冷炭黑（#0D0F12 / #131417）**、**0.5px 发丝级微边框（`rgba(255,255,255,0.08)`）** 与 **超高漫反射毛玻璃（Backdrop Blur 24px）**。
> 2. **画面即绝对主体 (Screen-First / 100% Canvas)**：横屏操控时画面 100% 铺满视口，零黑边挤压。
> 3. **极简微型悬浮岛 (Dynamic Touch Island)**：以小巧优雅的磨砂药丸浮层悬浮在画面边缘，单行不折行显示主机名、微小呼吸绿灯与 `120 FPS · 3.8ms` 遥测，轻触即用。
> 4. **触控与大拇指工效学 (Ergonomics)**：触控热区 $\ge 44\text{pt}$，常用手势与虚拟辅助键聚焦于大拇指舒适弧形覆盖区。

---

## 📱 最新统一高保真设计稿

### 1. 移动端竖屏极简控制中心 (Mobile Portrait Unified Console)

> 专为单手握持设计，极简管理设备流、EasyTier Mesh 虚拟组网与 5 信道网络性能。

![移动端竖屏极简控制中心](/Users/jinliang/.gemini/antigravity/brain/a55ce59b-1cef-4f11-9fe6-01ad1cf11456/mobile_screen1_portrait.png)

* **外部高清预览链接**：[点击在浏览器中查看高清大图](https://lh3.googleusercontent.com/aida/AP1WRLtVxxA5-v70QiXePzr_ISPGIvSuVA5t-L6OfGcXL5dN7C8B0GFsFHA1jcuSAnrdSlaNUruFb2YsPCvydqEyDH7ADTuNH21MKJ5L7yC-vhdyqj2SiG_yjKC4-lfahWmnr3wFGPybp0biQVxrnSJuGhCbzmhKk_byXkGd5rRRVriCGFC-CdmL8OXB00VSO04F2BK6UEQk0KzjJV1nSivVgviKPrLjvyQ7XePRbTJN8GTzEalEAOqI8_hMmXnD)

#### 结构与功能模块：
- **状态顶部栏 (Status Header)**：
  - 极简标题 `RemotePlay`；
  - 右侧微型磨砂胶囊 `Mesh: 10.144.0.5`，带 4px 微动呼吸绿点；
- **快速动作栏 (Quick Actions)**：
  - `[Scan QR to Pair 扫码配对]`：手机扫电脑端二维码免密加入私有 Mesh 组网；
  - `[New Mesh Group 新建群组]`；
- **发现的主机流卡片 (Discovered Hosts)**：
  - 继承桌面端一致的卡片风格（深色 Obsidian 底色、发丝级细微边框、呼吸感间距）；
  - `Gaming Rig RTX 4090`（单行标题、`LAN P2P` 灰色标签、`Direct UDP · 4K@120Hz Ready · 3.8ms RTT`、右侧高亮青色胶囊 `Connect` 按钮）；
  - `MacBook Pro M3 Max`（`EasyTier Mesh` 标签、`Virtual P2P · Standby`、`Connect` 按钮）；
- **5 信道多路复用网络卡片 (5-Lane Multiplexed Network)**：
  - 极简微型水平条形图展示 Realtime Video (42.5 Mbps)、Audio (128 kbps)、Control (64 kbps)、File (2.4 Mbps)；
- **底部悬浮磨砂导航坞 (Bottom Floating Glass Dock)**：
  - 4 个高雅图标 Tab：`[Devices 设备 (活跃)]` · `[Mesh 私有组网]` · `[Security 安全矩阵]` · `[Settings 设置]`。

---

### 2. 移动端横屏 100% 满屏远程触控操控 (Mobile Landscape Screen-First Viewport)

> 连接远程主机后横屏，进入极致沉浸的满屏触控工作台。

![移动端横屏满屏触控操控](/Users/jinliang/.gemini/antigravity/brain/a55ce59b-1cef-4f11-9fe6-01ad1cf11456/mobile_screen2_landscape.png)

* **外部高清预览链接**：[点击在浏览器中查看高清大图](https://lh3.googleusercontent.com/aida/AP1WRLsO9T5g_SxUnTtQh6fF5HU_jBRto-WgMDu_GLUrA1hfQ6AjTXcOhYp671VBgTdHaIwjowkXyHngcUjFllzYIc3J2tyD7TtbOupE3VvTA0xTIaIx4BrJOeUmoemlw70pz2b_eWPVoEGawCPtJiZVpFBA8f4i7rtIE-VfiQwsP_j00SZZIghYMKwmGQ2oadSJVwp8xGhLQiVHLfl-xlxBMxsF-TExmwUDlXe_HNWrgBkrhsG4zjnNLKEgmauJ)

#### 核心交互与结构细节：
- **100% 满屏无黑边**：远程桌面直接由 GPU 硬件解码纹理铺满屏幕，零遮挡；
- **顶部微型悬浮触控岛 (Dynamic Touch Island)**：
  - 继承桌面端同款设计：磨砂黑底色、0.5px 发丝边框、圆润胶囊；
  - 左侧：主机名 `Gaming Rig RTX 4090`、4px 呼吸绿点、`120 FPS · 3.8ms` 遥测；
  - 中间：细微 1px 分隔线；
  - 右侧：触控模式切换、对讲麦克风、剪贴板同步、断开连接（克制微红）；
  - 支持手指长按拖拽吸附至屏幕四角或边缘；
- **底部浮动虚拟修饰键栏 (Floating Modifier Bar)**：
  - 极简半透明磨砂条，包含桌面级核心键位：`[Esc]`, `[Ctrl]`, `[Alt]`, `[Win/Cmd]`, `[Shift]`, `[Tab]`, `[F5]`, `[F11]` 及方向键；
  - 纯黑磨砂键帽 + 发丝边框，按键间距呼吸感均匀，支持轻触锁定（Sticky Modifiers）；
- **微手势系统 (Micro-Gestures)**：
  - **三指轻点**：一键呼出/隐藏所有 HUD 浮层；
  - **左边缘右滑**：滑出半透明设备与网络管理抽屉；
  - **双指捏合**：点对点 1:1 视口平移缩放。

---

## 🕹️ 触控与手势模式体系设计

| 交互模式 | 核心手势与操作逻辑 | 适用场景 |
| :--- | :--- | :--- |
| **直触模式 (Direct Touch)** | 单指轻点 = 鼠标左键点击<br>单指长按 = 鼠标右键点击 / 拖拽<br>双指滑动 = 滚轮滚动<br>双指捏合 = 视口放大缩小与平移 | 浏览网页、日常文件查看、点按 UI 控件 |
| **虚拟触控板 (Virtual Trackpad)** | 屏幕任意区域作为相对指针触控板<br>单指滑动 = 移动指针（带加速度引擎）<br>双指轻点 = 鼠标右键<br>双指拖拽 = 精确框选 | 精确文字选取、代码编辑、设计软件微调 |
| **虚拟游戏手柄 (Gamepad Overlay)** | 左下角虚拟半透明摇杆（方向移动）<br>右侧半透明技能/动作按键（ABXY / 扳机）<br>多点独立无冲突触控 | 远程 3A 游戏、模拟器与手柄游戏 |

---

## 📁 规范文档索引

- 移动端设计规范：[`docs/MOBILE_UI_UX_DESIGN.md`](file:///Users/jinliang/remote_play/docs/MOBILE_UI_UX_DESIGN.md)
- 全平台设计总览：[`docs/UI_UX_SPECIFICATION.md`](file:///Users/jinliang/remote_play/docs/UI_UX_SPECIFICATION.md)
- 桌面端验收报告：[`walkthrough.md`](file:///Users/jinliang/.gemini/antigravity/brain/a55ce59b-1cef-4f11-9fe6-01ad1cf11456/walkthrough.md)
