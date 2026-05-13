# OpenTypeless · 安装与首次配置 / Install & First-Run Guide

> 中文版在前，English version below.

---

## 🇨🇳 中文

### 1. 安装

1. 双击挂载 `OpenTypeless_0.1.0_arm64.dmg`
2. 把 `OpenTypeless.app` **拖到** `Applications` 文件夹（不要直接在 DMG 里双击运行）
3. 弹出 DMG（右键 → 推出）

> ⚠️ 本应用未经 Apple 公证（个人/小团队使用，未加入 $99/年的 Developer Program）。第一次启动会被 Gatekeeper 拦：
>
> - **方法 A**：在 Finder 里**右键** `OpenTypeless.app` → 选「打开」→ 弹窗里**再点一次「打开」**
> - **方法 B（更彻底）**：终端跑一次 `xattr -dr com.apple.quarantine /Applications/OpenTypeless.app`，之后双击就能正常打开

### 2. 必须授予的系统权限

OpenTypeless 需要 **3 个权限** 才能完整工作。前两个 macOS 会在首次用到时自动弹对话框，**第三个需要你主动到设置里勾选**。

| # | 权限 | 用途 | 系统设置位置 | 触发方式 |
|---|---|---|---|---|
| 1 | **麦克风** | 录音 | 隐私与安全性 → 麦克风 | 首次按热键录音时弹窗 |
| 2 | **自动化 → System Events** | 调用系统全局键盘事件 | 隐私与安全性 → 自动化 | 首次粘贴时弹窗 |
| 3 | **辅助功能（Accessibility）** | 真正让 osascript / enigo 能下发 Cmd+V 键盘事件 | 隐私与安全性 → **辅助功能** | **不会自动弹窗，你必须自己加 + 勾选** |

#### ⚠️ 最容易踩坑的点

**「自动化 → System Events」≠「辅助功能」**。这是 macOS 两个完全独立的权限面板，名字看起来像但作用不同：

- **自动化** 控制 *允许 App A 调用 App B 的 AppleEvent 接口*
- **辅助功能** 控制 *允许 App 发送键盘 / 鼠标事件*

`tell application "System Events" to keystroke "v" using command down` 同时需要这两个权限。**只授「自动化」会失败**，macOS 报错码 `1002 - not allowed to send keystrokes`。

#### 主动授「辅助功能」权限的步骤

1. 苹果菜单 → **系统设置**
2. 左侧 **隐私与安全性**
3. 右侧滚动到 **辅助功能**
4. 如果列表里**没有** OpenTypeless：
   - 点底部的 **`+`**
   - 在弹窗里选 `Applications` → `OpenTypeless.app` → 「打开」
5. 把 OpenTypeless 的**开关打开**

App 启动时如果检测到没授权，会在主窗口顶部弹**琥珀色横幅**提示，点上面的「打开「辅助功能」设置」按钮可以一键跳到正确的设置页。

### 3. 首次运行需要配置的东西

进 App 后会自动进入 Onboarding，按引导设置：

- **STT 服务商**（SiliconFlow / Deepgram / GLM-ASR / Whisper 等都行，推荐 SiliconFlow 中文场景）+ 你自己的 API key
- **LLM 服务商**（润色/翻译用，OpenRouter / SiliconFlow / 各厂商直连都行）+ 你自己的 API key
- **热键**：默认录音 `Alt+/`，翻译切换 `Alt+Shift+.`（Option+>），Agent `Alt+Shift+/`（Option+?），都可改

### 4. 卸载

- 拖 `/Applications/OpenTypeless.app` 到废纸篓
- （可选）清除用户数据：`rm -rf ~/Library/Application\ Support/com.opentypeless.app`
- （可选）撤销权限：系统设置 → 隐私与安全性 → 各面板下手动移除

---

## 🇺🇸 English

### 1. Install

1. Double-click `OpenTypeless_0.1.0_arm64.dmg` to mount it.
2. **Drag** `OpenTypeless.app` into the `Applications` folder (do NOT run it directly from the mounted DMG — macOS treats it as a transient quarantined instance, so permissions won't stick).
3. Eject the DMG (right-click → Eject).

> ⚠️ This build is **not notarized by Apple** (we are not on Apple's $99/year Developer Program). First launch will be blocked by Gatekeeper:
>
> - **Option A**: In Finder, **right-click** `OpenTypeless.app` → "Open" → click "Open" again in the confirmation dialog.
> - **Option B** (cleaner): Run `xattr -dr com.apple.quarantine /Applications/OpenTypeless.app` in Terminal, then double-click normally.

### 2. Required system permissions

OpenTypeless needs **three** permissions. macOS will prompt you for the first two automatically when they're first used. **The third one (Accessibility) you must add yourself** — macOS never auto-prompts for it.

| # | Permission | Why | Settings location | How triggered |
|---|---|---|---|---|
| 1 | **Microphone** | Recording | Privacy & Security → Microphone | Auto-prompt on first record |
| 2 | **Automation → System Events** | Drive system-wide keyboard events | Privacy & Security → Automation | Auto-prompt on first paste |
| 3 | **Accessibility** | Actually allows osascript / enigo to send Cmd+V keystrokes | Privacy & Security → **Accessibility** | **NEVER auto-prompts. You must add + toggle it.** |

#### ⚠️ The gotcha

**Automation → System Events** ≠ **Accessibility**. These are *two completely separate* permission panels in macOS Privacy & Security, with confusingly similar names but very different purposes:

- **Automation** controls *whether App A can call App B's AppleEvent interface*.
- **Accessibility** controls *whether an app can send keyboard / mouse events*.

`tell application "System Events" to keystroke "v" using command down` **requires both**. Granting only "Automation" yields error code `1002 — not allowed to send keystrokes`.

#### Steps to grant Accessibility

1. Apple menu → **System Settings**
2. Left sidebar: **Privacy & Security**
3. Scroll to **Accessibility**
4. If OpenTypeless is **not** in the list:
   - Click the **`+`** button at the bottom
   - Pick `Applications` → `OpenTypeless.app` → "Open"
5. Toggle OpenTypeless **on**

On launch, if the app detects this permission is missing, it shows an **amber banner** at the top of the main window with a one-click button to open the correct settings pane.

### 3. First-run configuration

The app walks you through Onboarding on first launch:

- **STT provider** (SiliconFlow / Deepgram / GLM-ASR / Whisper variants — SiliconFlow recommended for Chinese) + your API key
- **LLM provider** (used for polish + translation — OpenRouter / SiliconFlow / direct vendor APIs all work) + your API key
- **Hotkeys**: default record `Alt+/`, toggle translate `Alt+Shift+.` (Option+>), force agent `Alt+Shift+/` (Option+?). All configurable in Settings → General.

### 4. Uninstall

- Drag `/Applications/OpenTypeless.app` to the Trash.
- (Optional) Clear user data: `rm -rf ~/Library/Application\ Support/com.opentypeless.app`
- (Optional) Revoke permissions: System Settings → Privacy & Security → remove OpenTypeless from each pane manually.

---

## Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| 「Auto-paste failed (exit 1): … (1002)」 | Accessibility permission missing | Step 2 #3 above |
| 「Auto-paste blocked: not authorized」 | Automation → System Events not granted | Privacy & Security → Automation → OpenTypeless → toggle System Events on |
| 「OpenTypeless can't be opened because it is from an unidentified developer」 | Gatekeeper / quarantine | See Section 1 (right-click → Open, or `xattr -dr com.apple.quarantine`) |
| Recording starts but Capsule isn't visible | Capsule may have drifted behind the Dock (rare) | Should auto-clamp now; otherwise drag it up |
| App fails to launch on Intel Mac | This DMG is arm64-only | Need to rebuild with `--target universal-apple-darwin` |
