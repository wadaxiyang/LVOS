# LVOS Lightweight UI Lifecycle & Agent Separation Specification

**Status:** Proposed  
**Target:** LVOS Desktop V1.x architecture refactor  
**Primary platforms:** Windows 11 x86_64, macOS 15 arm64  
**UI framework:** Slint + Quadrant-Kit  
**Default Popup idle timeout:** 30 seconds  
**Document scope:** Agent / GUI process separation, lazy UI construction, Popup warm-retention lifecycle, configurable Popup destruction timeout, process shutdown rules, IPC boundaries, migration and acceptance criteria.

---

## 1. Background

LVOS 当前桌面端在启动时由 `UiController::new()` 一次性创建多个 Slint Window，包括：

- `MainWindow`
- `QuickLookupPopup`
- `PermissionWindow`

当前关闭主窗口与查询 Popup 的主要行为是 `hide()`，窗口组件及 Slint / winit / femtovg 相关运行时仍由长期存在的强引用持有。

这会导致即使用户已经关闭所有可见窗口，后台进程仍保留完整 GUI runtime 与 UI 对象。当前观察到后台内存约为 50 MB 左右，这不符合 LVOS “极轻量、长期常驻”的产品目标。

LVOS 的实际使用模式天然包含三类完全不同的生命周期：

1. **Agent / 后台服务**
   - 高频或长期运行。
   - 应始终常驻。
   - 不应依赖 Slint、Quadrant-Kit、winit、femtovg。

2. **Quick Lookup Popup**
   - 高频触发。
   - 每次显示时间很短。
   - 连续查询时需要极低延迟。
   - 不适合永久常驻，也不适合每次关闭后立即重建。

3. **Main Management UI**
   - History / Favorites / Settings。
   - 低频使用。
   - 用户关闭后没有保留完整 Window 的必要。

因此，本次重构以 **进程边界 + UI Host 生命周期** 为核心，而不是继续围绕 `hide()` 与 `show()` 做局部优化。

---

# 2. Goals

本 Spec 的目标如下。

## 2.1 Primary Goals

### G1. 将后台 Agent 与 GUI runtime 分离

后台常驻进程不得初始化或链接运行 Slint GUI backend，不得因为 Tray、Hotkey、Sync、Database、Provider 等业务而加载完整 GUI runtime。

### G2. UI 必须按需创建

LVOS 启动后，在用户没有触发任何 GUI 操作时：

```text
MainWindow = None
QuickLookupPopup = None
PermissionWindow = None
```

不得在应用启动路径中 eager construct 任意 Slint Window。

### G3. Main UI 关闭后真正销毁

History / Favorites / Settings 所属的 `MainWindow` 必须采用 Lazy Construction。

用户关闭 Main UI 后，应释放对应的 `MainUiHost`，而不是仅调用 `hide()` 后永久保留。

### G4. Quick Lookup Popup 采用 Lazy + Warm Retention

Popup 第一次使用时创建。

Popup 被关闭后：

1. 先 `hide()`；
2. 进入 Warm 状态；
3. 默认保留 **30 秒**；
4. 30 秒内再次查询则直接复用；
5. 每次查询或重新显示都重置 idle timer；
6. idle timeout 到期后销毁 Popup；
7. 如果 GUI 进程中没有其他 UI Host，则随后退出 GUI 进程。

### G5. Popup idle timeout 必须可配置

用户可以在 Settings 中调整 Popup 空闲保留时间。

默认值：

```text
30 seconds
```

### G6. 保持统一 UI

`MainWindow`、`QuickLookupPopup`、`PermissionWindow` 继续统一使用 **Quadrant-Kit**。

进程和生命周期解耦不得导致视觉组件分叉。

---

# 3. Non-Goals

本次重构暂不包含以下内容。

### NG1. 暂不引入第三个 `lvos-popup.exe`

本阶段目标架构为：

```text
lvos-agent
lvos-ui
```

而不是：

```text
lvos-agent
lvos-popup
lvos-desktop
```

Popup 必须从 Main UI 的生命周期中解耦，但暂时仍运行于统一的 `lvos-ui` GUI 进程中。

未来如果 benchmark 证明 Popup-only GUI 进程仍过重，可以进一步拆出独立 `lvos-popup` 进程。

### NG2. 不用原生 Win32 / AppKit 重写 Popup

Quick Lookup Popup 继续使用 Slint + Quadrant-Kit。

### NG3. 不改变现有 Lookup 业务语义

本次重构只改变：

- 进程职责
- UI 生命周期
- IPC
- 资源释放策略

不得改变用户已有的翻译、收藏、统计、历史记录、同步等业务行为。

### NG4. 不要求立即优化 Quadrant-Kit 本身的内存占用

本次优先通过生命周期设计解决长期驻留问题。

---

# 4. Target Architecture

## 4.1 Process Layout

目标架构：

```text
┌───────────────────────────────────────────────────┐
│                    lvos-agent                     │
│                                                   │
│  Tray                                             │
│  Global Hotkey                                    │
│  Selection Capture                                │
│  Database                                         │
│  Lookup / Translation                             │
│  Cache                                            │
│  Favorites                                        │
│  Query Statistics                                 │
│  Sync                                             │
│  Provider                                         │
│  Update Coordination                              │
│  Preferences                                      │
│  IPC Server                                       │
│  Native Tray / Hotkey Event Loop                  │
│                                                   │
│  NO Slint                                         │
│  NO Quadrant-Kit                                  │
│  NO femtovg                                       │
└──────────────────────┬────────────────────────────┘
                       │ Local IPC
                       │
                       ▼
┌───────────────────────────────────────────────────┐
│                     lvos-ui                       │
│                                                   │
│  UiProcessCoordinator                             │
│                                                   │
│  ┌─────────────────┐   ┌──────────────────────┐   │
│  │ PopupHost       │   │ MainUiHost           │   │
│  │                 │   │                      │   │
│  │ QuickLookup     │   │ History              │   │
│  │ Popup           │   │ Favorites            │   │
│  │                 │   │ Settings             │   │
│  │ Lazy + Warm     │   │ Lazy + Destroy       │   │
│  └─────────────────┘   └──────────────────────┘   │
│                                                   │
│  ┌─────────────────┐                              │
│  │ PermissionHost  │                              │
│  │ Lazy + Destroy  │                              │
│  └─────────────────┘                              │
│                                                   │
│  Slint + Quadrant-Kit + winit + femtovg           │
└───────────────────────────────────────────────────┘
```

---

# 5. Process Responsibilities

## 5.1 `lvos-agent`

`lvos-agent` 是唯一长期常驻进程。

### Required Responsibilities

- 单实例控制
- Tray
- Global Hotkey
- Selection Capture
- SQLite
- History
- Favorites
- Query Statistics
- Translation Provider
- Provider Credentials
- Sync
- Device Identity
- Update Check Coordination
- Local Preferences
- Startup / Launch-at-login
- IPC Server
- 启动与管理 `lvos-ui`

### Native Event Loop and Forbidden Dependencies

Agent 必须拥有独立的原生事件循环，用于 Tray、Global Hotkey、第二实例激活和平台权限动作。Windows 可使用 winit user event 将后台任务 marshal 到事件循环线程；macOS 必须在进程主线程运行该循环并执行 AppKit 要求的主线程工作。

该事件循环属于后台平台集成，不得创建窗口或初始化 UI renderer。

`lvos-agent` 的运行路径中不得初始化：

```text
slint
femtovg
Quadrant-Kit
```

如果 workspace 依赖关系允许，建议从 crate 依赖层面避免这些 UI crate 被 `lvos-agent` 引入。

---

## 5.2 `lvos-ui`

`lvos-ui` 是按需启动的 GUI 进程。

其职责仅包括：

- UI presentation
- 用户交互
- UI state mapping
- Quadrant-Kit theme / components
- Window lifecycle
- UI-specific local state
- 与 Agent 的 IPC

业务事实的 authoritative state 应位于 Agent。

---

# 6. UI Host Model

不得继续保留一个在构造时 eager 创建全部窗口的 `UiController`。

当前类似：

```rust
pub struct UiController {
    popup: QuickLookupPopup,
    confirmations: ConfirmationBroker,
    main_window: MainWindow,
    permission_window: PermissionWindow,
}
```

应重构为生命周期独立的 Host。

建议结构：

```rust
pub struct UiProcessCoordinator {
    popup: Option<PopupHost>,
    main: Option<MainUiHost>,
    permission: Option<PermissionHost>,
}

pub struct PopupHost {
    popup: QuickLookupPopup,
    lifecycle: PopupLifecycle,
}

pub struct MainUiHost {
    main_window: MainWindow,
    confirmations: ConfirmationBroker,
}

pub struct PermissionHost {
    permission_window: PermissionWindow,
}
```

核心原则：

> Host 是否存在，必须能够直接反映对应 UI 是否实际占用资源。

---

# 7. Main UI Lifecycle

## 7.1 Creation

初始状态：

```text
MainUiHost = None
```

当 Agent 请求：

```text
OpenMainWindow
```

时：

```text
None
 ↓
MainUiHost::new()
 ↓
load current UI snapshot
 ↓
show MainWindow
```

---

## 7.2 Closing

用户点击关闭：

```text
CloseRequested
 ↓
invalidate pending confirmation
 ↓
hide if required by native close sequence
 ↓
drop MainUiHost
```

最终状态：

```text
MainUiHost = None
```

不得以永久 `HideWindow` 作为 Main UI 的最终生命周期语义。

---

## 7.3 Reopen

下一次用户打开 Settings / History：

```text
MainUiHost::new()
```

重新从 Agent 获取 authoritative state。

Main UI 不依赖旧 Window 中遗留的状态恢复业务正确性。

---

# 8. Popup Lifecycle

## 8.1 Design Principle

Quick Lookup Popup 使用：

> **Lazy Construction + Temporary Warm Retention + Idle Destruction**

不得：

- 应用启动时创建；
- 永久常驻；
- 每次 dismiss 后立即重建；
- 与 MainWindow 生命周期绑定。

---

# 9. Popup State Machine

Popup 必须至少具有以下状态。

```text
Cold
Active
Warm
```

状态图：

```text
                 Lookup Request
          ┌──────────────────────────┐
          │                          ▼
       ┌──────┐                  ┌────────┐
       │ Cold │                  │ Active │
       └──────┘                  └────────┘
                                    │
                                    │ dismiss / outside click / escape
                                    ▼
                                ┌────────┐
                                │  Warm  │
                                └────────┘
                                 │      │
                     new lookup  │      │ idle timeout
                                 │      │
                                 ▼      ▼
                              Active   Cold
```

---

## 9.1 Cold

```text
PopupHost = None
```

无 `QuickLookupPopup` 实例。

---

## 9.2 Active

Popup 已存在且正在显示。

可能处于：

- Loading
- Ready
- Error
- Interactive

Active 状态下：

- 不运行 idle destruction timer；
- 不允许因为 timeout 销毁 Popup；
- lookup generation 必须继续有效。

---

## 9.3 Warm

Popup 已隐藏，但 Host 保留。

```text
PopupHost = Some(...)
window visible = false
idle timer = running
```

Warm 的目的只包括：

- 降低连续查询 latency；
- 复用 Window；
- 复用 Slint component；
- 避免短时间内频繁 create / destroy。

Warm 不是永久驻留状态。

---

# 10. Default Popup Timeout

默认：

```text
30 seconds
```

配置键建议：

```text
popup_idle_timeout_secs
```

Rust 类型建议：

```rust
u32
```

默认值：

```rust
const DEFAULT_POPUP_IDLE_TIMEOUT_SECS: u32 = 30;
```

---

# 11. Popup Timeout Semantics

## 11.1 Timer Start

Timer 仅在 Popup 从可见状态进入隐藏状态后启动。

例如：

- outside click
- Escape
- dismiss button
- native close request
- programmatic dismiss

---

## 11.2 Timer Reset

以下事件必须取消旧 timer，并重新计算生命周期：

### New Lookup Request

```text
Warm
 ↓
cancel timer
 ↓
Active
```

### Popup Re-show

任何重新显示 Popup 的行为都视为新的 Active session。

---

## 11.3 Timeout

当 timer 到期：

```text
Warm
 ↓
release native monitor
 ↓
invalidate pending UI callbacks
 ↓
drop PopupHost
 ↓
PopupHost = None
```

随后调用：

```text
UiProcessCoordinator::evaluate_process_exit()
```

---

## 11.4 Timeout = 0

如果用户配置：

```text
popup_idle_timeout_secs = 0
```

语义为：

> Popup dismiss 后立即销毁，不进入 Warm retention。

流程：

```text
Active
 ↓ dismiss
Cold
```

---

# 12. Configurable Popup Retention

## 12.1 Settings Location

建议放置于：

```text
Settings
└── General / Behavior
    └── Lookup Popup
```

或者现有 Settings 页面中最接近：

- Global Hotkey
- Launch minimized
- Startup behavior

的区域。

---

## 12.2 UI Label

建议中文语义：

```text
查询窗口空闲保留时间
```

英文：

```text
Lookup popup idle retention
```

说明文案：

```text
查询窗口关闭后会短暂保留，以加快连续查询。
超过设定时间后将自动释放界面资源。
```

---

## 12.3 Recommended UI

优先使用 Quadrant-Kit 中现有可复用的选择控件。

推荐预设：

```text
立即销毁
15 秒
30 秒（默认）
60 秒
120 秒
```

对应值：

```text
0
15
30
60
120
```

不建议在第一版提供任意自由输入，以避免不必要的输入校验和极端值。

底层配置仍使用整数秒，便于未来扩展自定义值。

---

## 12.4 Runtime Update

用户修改 retention 时必须立即生效。

### Popup = Cold

仅保存新值。

### Popup = Active

保存新值，不启动 timer。

等本次 Popup dismiss 时使用新值。

### Popup = Warm

立即取消当前 timer，并根据新值重新处理。

如果新值：

```text
0
```

则：

```text
drop PopupHost immediately
```

如果新值：

```text
> 0
```

则从设置修改时刻重新计时。

---

# 13. Popup Timer Implementation Requirements

不得使用阻塞 sleep。

应使用：

- Slint timer；
- Tokio async timer；
- 或适配 GUI event loop 的非阻塞定时机制。

必须处理 stale timer。

建议引入 generation：

```rust
struct PopupLifecycle {
    generation: u64,
    state: PopupState,
}
```

每次：

- show
- hide
- timeout restart
- destroy

更新 generation。

Timer callback 只有 generation 仍匹配时才允许执行 destroy。

示意：

```rust
let generation = self.lifecycle.next_generation();

schedule(timeout, move || {
    if manager.current_generation() != generation {
        return;
    }

    manager.destroy_popup();
});
```

避免：

```text
旧 timer
 ↓
误销毁刚刚重新显示的 Popup
```

---

# 14. Popup Creation

建议：

```rust
impl UiProcessCoordinator {
    fn ensure_popup(&mut self) -> Result<&mut PopupHost, UiError> {
        if self.popup.is_none() {
            self.popup = Some(PopupHost::new()?);
        }

        Ok(self.popup.as_mut().unwrap())
    }
}
```

任何 Lookup 请求：

```text
ensure_popup()
 ↓
cancel idle timeout
 ↓
apply state
 ↓
show popup
 ↓
state = Active
```

---

# 15. Popup Dismiss

Popup dismiss 必须统一经过一个入口。

禁止不同事件各自直接调用：

```rust
popup.hide()
```

建议：

```rust
fn dismiss_popup(
    &mut self,
    reason: PopupDismissReason,
) -> Result<(), UiError>
```

可能原因：

```rust
enum PopupDismissReason {
    OutsideClick,
    Escape,
    CloseRequested,
    UserDismiss,
    Programmatic,
}
```

统一流程：

```text
mark native_show_requested = false
mark native_interactive = false
invalidate native request id
remove outside-click monitor
hide native window
state = Warm or Cold
schedule idle release
```

---

# 16. Main UI and Popup Interaction

Main UI 与 Popup 生命周期必须相互独立。

## Scenario A

```text
Popup Active
Main UI Closed
```

允许。

---

## Scenario B

```text
Popup Warm
User opens Settings
```

行为：

```text
same lvos-ui process
 ↓
create MainUiHost
 ↓
PopupHost remains Warm
```

如果 Popup timeout 到期：

```text
drop PopupHost
MainUiHost remains alive
```

---

## Scenario C

```text
Main UI Open
New Lookup
```

行为：

```text
ensure PopupHost
show Popup
```

不得创建第二个 GUI process。

---

## Scenario D

```text
Main UI closes
Popup Warm
```

行为：

```text
drop MainUiHost
keep lvos-ui alive until popup timeout
```

---

## Scenario E

```text
Main UI closes
Popup Cold
PermissionHost None
```

行为：

```text
exit lvos-ui immediately
```

---

# 17. GUI Process Exit Policy

`lvos-ui` 必须由 `UiProcessCoordinator` 统一决定是否退出。

建议：

```rust
fn has_live_ui(&self) -> bool {
    self.main.is_some()
        || self.popup.is_some()
        || self.permission.is_some()
}
```

当：

```text
main == None
popup == None
permission == None
```

且：

- 无正在处理的 UI IPC request；
- 无必须完成的 UI-local operation；

则：

```text
UI → RequestIdleExit(request_id, generation)
 ↓
Agent atomically marks generation Stopping
 ↓
Agent → IdleExitApproved(request_id, generation)
 ↓
UI rechecks that it is still idle
 ↓
quit Slint event loop
exit lvos-ui
```

UI 不得在 handshake Snapshot 应用后、首个显示命令到达前申请退出。若 `RequestIdleExit` 与新的显示命令交错，已排队的显示命令必须先应用；UI 随后发送 `CancelIdleExit`，Agent 只有在 request ID 与 generation 同时匹配时才允许 `Stopping → Ready`。若没有新活动，Agent 保持 `Stopping`，新查询等待旧进程真正退出后再启动下一 generation。

---

# 18. Agent-side UI Process Management

Agent 应拥有：

```rust
UiProcessClient
```

职责：

- 判断 `lvos-ui` 是否存在；
- 如果不存在则 spawn；
- 建立 IPC；
- 等待 UI ready；
- 发送 UI command；
- UI crash 后允许下次请求重新启动；
- 避免重复启动多个 UI process。

---

# 19. UI Process Single Instance

同一 Agent session 中最多只能存在一个：

```text
lvos-ui
```

Agent 发送 GUI request 时：

```text
if ui process alive:
    reuse connection
else:
    spawn ui
```

UI process 自身也应有 secondary instance protection，避免异常情况下出现两个 GUI host。

实现可由 Agent session 的 authenticated handshake 提供保护：只有处于 `Starting(generation)` 的唯一连接能被接受；重复、旧 generation 或手工启动且没有 Agent launch capability 的 UI 必须拒绝并退出。

---

# 20. IPC Boundary

建议新增或明确：

```text
crates/ipc
```

IPC transport 必须保持本地、用户级、安全。

Windows 可采用：

```text
Named Pipe
```

macOS 可采用：

```text
Unix Domain Socket
```

也可以采用一个跨平台 local IPC abstraction。

协议必须包含：

- 固定协议版本，版本不匹配时 fail closed；
- 每个 Agent session 的随机 session ID 与高熵 authentication capability；
- 每个 envelope 单调递增的 sequence 与唯一 message ID；
- UI request ID，以及带 `response_to`、operation name、success / failure、用户反馈的异步响应；
- `Hello → HandshakeAccepted → Snapshot → Ready` 启动握手；
- UI 必须完整应用 Snapshot 后才能发送 Ready；
- Snapshot revision 与 Patch 的 `base_revision → revision` 顺序约束；断档时 UI 请求新 Snapshot；
- Agent 或 UI 断线检测与有界 shutdown；
- Windows Named Pipe 禁止远程 client；macOS Unix socket 位于用户数据目录并设置用户级 `0600` 权限。

所有 Settings callback 只提交异步 IPC request，不得在 Slint callback 内阻塞等待 Agent。GUI 在 request pending 时禁用相关交互，收到关联响应后显示结果；失败时用最新 Agent snapshot / patch 恢复 authoritative value。

“GUI 正准备退出，新查询同时到达”必须由显式状态机处理：

```text
Stopped → Starting(generation N) → Ready(generation N)
Ready(generation N) → Stopping(generation N) → Stopped
```

Agent 在 `Stopping` 时不得仅凭 OS process alive 判断可发送，也不得并行启动 N+1。必须等待 N 真正退出，再由 Agent-owned 单调 generation 启动 N+1。

---

# 21. IPC Commands

至少定义以下 Agent → UI 消息。

```rust
enum AgentToUi {
    ShowLookup(LookupUiState),
    HideLookup,
    OpenMainWindow(MainUiSnapshot),
    UpdateMainWindow(MainUiPatch),
    ShowPermission(PermissionUiState),
    UpdatePreference(UiPreferencePatch),
    Shutdown,
}
```

UI → Agent：

```rust
enum UiToAgent {
    Ready,
    LookupDismissed,
    FavoriteToggle { key: String },
    HistorySearch { term: String },
    FavoritesSearch { term: String },
    ClearHistory,
    SaveProviderSettings(...),
    SaveNetworkSettings(...),
    Login(...),
    Logout,
    ManualSync,
    TestConnection,
    RevokeDevice(...),
    RegenerateDeviceIdentity,
    ExportData,
    ImportData,
    CheckUpdate,
    UpdateGlobalHotkey(...),
    UpdateStartAtLogin(...),
    UpdateLaunchMinimized(...),
    UpdatePopupIdleTimeout { seconds: u32 },
}
```

具体类型可以按现有 callback contract 拆分。

---

# 22. Authoritative State Ownership

业务 authoritative state 必须归 Agent。

UI 只保存：

- 当前显示状态
- focus state
- scroll position
- transient form editing state
- animation state
- Popup warm lifecycle state

不得依赖一个长期存在的 Window 来保存业务真值。

---

# 23. Credentials

Provider API Key、Access Token、Refresh Token 等敏感数据应尽可能由 Agent 持有。

UI 仅在用户明确编辑时接收必要的临时输入。

不得因为 MainWindow 销毁而丢失持久配置。

不得为了方便 UI 恢复而长期将完整 secrets 保存在 GUI process 内。

---

# 24. Quadrant-Kit Requirement

所有 UI surface 继续使用：

```text
Quadrant-Kit
```

包括：

- Popup
- MainWindow
- Permission UI

必须共享：

- Theme
- Typography
- Spacing
- Radius
- Icons
- Motion
- Surface
- Button
- InfoBar
- ScrollView
- Navigation

生命周期分离不得通过复制组件代码实现。

---

# 25. Suggested Source Layout

建议逐步整理为：

```text
apps/
├── agent/
│   └── src/
│       ├── main.rs
│       ├── ui_process.rs
│       └── ...
│
└── desktop/
    ├── src/
    │   ├── main.rs
    │   ├── coordinator.rs
    │   ├── popup_host.rs
    │   ├── main_ui_host.rs
    │   ├── permission_host.rs
    │   └── ipc.rs
    │
    └── ui/
        ├── app.slint
        ├── windows/
        │   ├── main_window.slint
        │   ├── quick_lookup_popup.slint
        │   └── permission_window.slint
        └── pages/
```

如果暂时不重命名现有 `apps/desktop`，也可以先保留目录名。

关键是职责边界，而不是目录名字。

---

# 26. `UiController` Refactor

当前 `UiController` 不应继续承担：

```text
所有 Window 的永久 ownership
```

建议废弃或改造成：

```rust
pub struct UiProcessCoordinator {
    popup: Option<PopupHost>,
    main: Option<MainUiHost>,
    permission: Option<PermissionHost>,
}
```

`initialize_desktop_backend()` 只允许在 `lvos-ui` 中执行。

Agent 不得调用。

---

# 27. ConfirmationBroker

`ConfirmationBroker` 的生命周期必须与：

```text
MainUiHost
```

绑定。

当 MainWindow 销毁：

```text
ConfirmationBroker
```

同步销毁。

Main UI close 前必须：

```text
invalidate pending confirmation
```

不得让 pending confirmation 阻止 Window drop。

---

# 28. Permission UI

Permission Window 也必须 Lazy Construct。

初始：

```text
PermissionHost = None
```

需要显示时：

```text
PermissionHost::new()
```

权限操作完成并关闭后：

```text
drop PermissionHost
```

不得在普通后台启动路径中 eager 创建。

---

# 29. Existing Conditional Pages

当前 `MainWindow` 内部使用条件实例化：

```text
HistoryPage
FavoritesPage
SettingsPage
```

这种页面级 lazy pattern 可以保留。

本次重点是进一步解决：

```text
Window level
Process level
```

生命周期。

---

# 30. Preferences Model

建议将 Popup timeout 纳入 Local Preferences。

示意：

```rust
pub struct UiPreferences {
    pub popup_idle_timeout_secs: u32,
}
```

默认：

```rust
impl Default for UiPreferences {
    fn default() -> Self {
        Self {
            popup_idle_timeout_secs: 30,
        }
    }
}
```

---

# 31. Preference Validation

底层建议接受：

```text
0..=300 seconds
```

超出范围时：

```text
clamp or reject
```

推荐明确 reject，避免隐藏配置错误。

UI 第一版只暴露标准预设：

```text
0
15
30
60
120
```

因此正常 UI 不会产生非法值。

---

# 32. Preference Persistence

修改后必须：

1. 保存到现有本地 preference store；
2. Agent 成为 authoritative owner；
3. 将新值推送给当前 `lvos-ui`；
4. 当前 Warm Popup 立即按新规则重启 timer；
5. 下一次 UI process 启动时从 Agent snapshot 获得该值。

---

# 33. Startup Behavior

正常 Agent startup：

```text
start lvos-agent
 ↓
load preferences
 ↓
initialize tray
 ↓
initialize hotkey
 ↓
initialize database
 ↓
initialize provider / sync
 ↓
listen IPC
```

不得：

```text
spawn lvos-ui
```

除非有明确 GUI 需求。

这里的“正常启动”指后台启动路径，且当时没有权限提示、恢复失败或其他必须由用户处理的 GUI 请求。命令行显式打开、托盘打开、第二实例激活和 `launch-minimized=false` 都属于明确 GUI 需求。

---

# 34. Launch Minimized Semantics

现有 `launch-minimized` 需要重新定义。

首次启动或本地配置缺失、损坏时，默认值必须为：

```text
launch-minimized = true
```

因此首次正常后台启动只创建 Agent。用户之后显式关闭该选项，才在后续启动时自动打开主窗口。权限请求等必须由用户处理的流程可以覆盖该选项并启动 UI。

在新架构下：

### Enabled

```text
start Agent only
do not spawn UI
```

### Disabled

如果产品仍希望启动时显示主窗口：

```text
Agent startup
 ↓
spawn lvos-ui
 ↓
OpenMainWindow
```

如果未来决定 LVOS 默认永远后台启动，则可以进一步废弃该选项，但不属于本 Spec。

---

# 35. Lookup Flow

完整 Lookup flow：

```text
Global Hotkey
 ↓
Agent captures selection
 ↓
Agent validates source
 ↓
Agent starts lookup
 ↓
Agent obtains loading state
 ↓
ensure lvos-ui process
 ↓
send BeginLookup(display_session_id, Loading)
 ↓
UI ensure PopupHost
 ↓
Popup Active
 ↓
Agent obtains Ready/Error
 ↓
send UpdateLookup(display_session_id, Ready/Error)
 ↓
UI updates existing Popup
```

`BeginLookup` 是唯一允许创建 UI process、创建 PopupHost 和显示 Popup 的查询消息。`UpdateLookup` 必须同时匹配 query ID 与 display session ID，并且只能更新现有 Active Popup；不得创建、重新显示或重启 UI。用户关闭 Popup 或 UI process 后才到达的旧结果只完成 Agent 侧业务，不得把界面自动弹回。

---

# 36. Cached Lookup

如果 Agent 本地 cache 命中：

```text
Hotkey
 ↓
Agent cache hit
 ↓
ensure UI
 ↓
BeginLookup(display_session_id, Ready)
```

不需要额外 loading round-trip。

---

# 37. Popup Dismiss Flow

```text
User dismiss
 ↓
UI hides Popup
 ↓
UI state = Warm
 ↓
UI starts retention timer
 ↓
UI sends LookupDismissed if business layer needs it
```

Agent 不需要因为 Popup hidden 而销毁业务服务。

---

# 38. Warm Reuse Flow

```text
Popup Warm
 ↓
new Lookup Request within 30s
 ↓
cancel old timer
 ↓
reuse same QuickLookupPopup
 ↓
apply new loading/ready state
 ↓
show
 ↓
Active
```

不得：

```text
drop
new
show
```

---

# 39. Idle Destruction Flow

默认：

```text
Popup hide
 ↓
30 seconds
 ↓
no new lookup
 ↓
drop PopupHost
 ↓
evaluate process exit
```

如果：

```text
MainUiHost = None
PermissionHost = None
```

则：

```text
quit event loop
exit lvos-ui
```

---

# 40. Main Window Close Flow

```text
Close MainWindow
 ↓
cancel confirmation
 ↓
drop MainUiHost
 ↓
evaluate process exit
```

如果 Popup Warm：

```text
keep lvos-ui
```

如果 Popup Cold：

```text
exit lvos-ui
```

---

# 41. UI Crash Recovery

如果 `lvos-ui` 异常退出：

- Agent 必须继续运行；
- Hotkey / Tray / Sync / Database 不受影响；
- 下一次 GUI request 自动重新 spawn UI；
- 不需要弹出永久错误；
- 应记录一次 structured log。

---

# 42. Agent Exit

当 Agent 正常退出：

```text
send Shutdown to UI
 ↓
UI dismiss all surfaces
 ↓
quit event loop
 ↓
exit
```

如果 IPC 已失效，UI 应检测 Agent disconnect，并自行退出。

---

# 43. Update / Restart

Update / Restart 流程不得假设 GUI 永远存在。

更新逻辑应由 Agent coordination。

如果需要重启：

```text
shutdown UI
 ↓
shutdown Agent
 ↓
restart
```

---

# 44. Logging

必须记录以下 lifecycle 事件。

建议 event names：

```text
ui_process_spawned
ui_process_ready
ui_process_exited

popup_created
popup_shown
popup_hidden
popup_warm_started
popup_warm_reused
popup_idle_timeout
popup_destroyed

main_ui_created
main_ui_shown
main_ui_destroyed

permission_ui_created
permission_ui_destroyed
```

Popup timeout 日志需包含：

```text
configured_timeout_secs
generation
reason
```

不得记录：

- API key
- access token
- refresh token
- private lookup content

除非现有 redaction policy 已允许并严格脱敏。

---

# 45. Performance Requirements

## 45.1 Agent-only Idle

在：

```text
no Main UI
no Popup
no Permission UI
```

时：

- `lvos-ui` 进程必须不存在；
- Agent 不得持有 Slint Window；
- Agent 不得初始化 femtovg；
- Agent 的 native event loop 不得创建窗口或加载 Slint / Quadrant-Kit / femtovg。

### Memory Target

Windows 11 reference build：

```text
Agent-only steady-state Private Bytes target: <= 20 MB
```

此值作为初始工程目标，不建议在第一次重构 commit 中直接设为 release blocker。

完成架构拆分后应重新建立真实 baseline，再决定最终 hard budget。

---

## 45.2 Warm Popup

允许 Popup Warm 状态短时间保持 GUI memory。

这是有意设计，不视为 leak。

但：

```text
idle timeout + 5s
```

后必须满足：

```text
PopupHost == None
```

如果没有其他 UI：

```text
lvos-ui process == not running
```

---

## 45.3 Memory Stability

应增加长期 lifecycle test：

```text
500 lookup popup cycles
```

至少覆盖：

```text
Cold → Active → Warm → Active
Active → Warm → Cold
Main UI open/close
Popup during Main UI
Theme / scale
```

不得观察到随 cycle 数持续线性增长的 Private Bytes。

---

# 46. Latency Requirements

建议初始目标：

### Warm Popup

已有 PopupHost：

```text
IPC request → visible update
p95 <= 50 ms
```

不包含远程 Provider 请求耗时。

### Cold Popup

需要启动 `lvos-ui`：

```text
Agent spawn → Popup visible
p95 target <= 250 ms
```

该值作为优化目标，需在目标硬件 benchmark 后确认。

---

# 47. Timer Accuracy

Popup timeout 应满足：

```text
configured timeout ± 1 second
```

不要求硬实时。

系统 suspend / resume 后不得出现：

- 旧 timer 销毁新 Popup；
- 多个 timer 同时 destroy；
- stale generation callback。

---

# 48. Test Plan

## 48.1 Unit Tests

### Popup state transitions

测试：

```text
Cold → Active
Active → Warm
Warm → Active
Warm → Cold
Active + timeout callback must not destroy
stale timer ignored
```

### Preference

测试：

```text
default = 30
0 accepted
15 accepted
30 accepted
60 accepted
120 accepted
invalid value rejected
```

---

## 48.2 Integration Tests

测试：

```text
Agent starts with no UI
First lookup spawns UI
Popup dismiss enters Warm
Second lookup within 30s reuses Popup
Timeout destroys Popup
No other UI causes UI process exit
Settings opens UI
Settings closes and UI exits
Settings closes while Popup Warm and UI stays alive
Popup timeout while Settings open destroys Popup only
```

---

## 48.3 Native Window Tests

Windows：

- no-activate Popup behavior
- outside click
- Escape
- native close
- topmost placement
- focus preservation

macOS：

- no-activate Popup behavior
- outside click monitor
- permission activation
- focus behavior
- 从打包后的 `LVOS.app` 启动，确认 Accessibility 列表与系统审计中的责任进程是 Agent 主可执行文件；
- 授权后在其他应用选中文本并触发热键，确认实际发送复制动作和读取选区的仍是 Agent，而不是 `lvos-ui`；
- 退出 / 重启 Agent 后重复验证授权恢复、Tray 与 Hotkey 都在 macOS 主线程事件循环上工作。

上述 macOS 权限与进程身份必须在 macOS 15 arm64 实机完成，交叉编译、代码签名检查和模拟测试不能替代该验收。

---

## 48.4 Process Tests

测试：

```text
UI crash does not kill Agent
Agent exits → UI exits
duplicate UI spawn prevented
IPC reconnect after UI restart
```

---

# 49. Acceptance Criteria

本功能只有在以下条件全部满足后才算完成。

## AC1

以后台方式启动 LVOS，`launch-minimized=true`，且没有权限提示等 GUI 需求时：

```text
only lvos-agent exists
```

---

## AC2

Agent 不初始化任何 Slint Window。

---

## AC3

第一次 Lookup 才创建 Popup。

---

## AC4

Popup dismiss 后默认进入 30 秒 Warm 状态。

---

## AC5

30 秒内再次 Lookup 复用同一个 PopupHost。

---

## AC6

Popup idle timeout 到期后：

```text
PopupHost == None
```

---

## AC7

如果没有 Main / Permission Host：

```text
lvos-ui exits
```

---

## AC8

MainWindow 关闭后：

```text
MainUiHost == None
```

不得只是永久 hide。

---

## AC9

Settings 中可以修改 Popup retention。

---

## AC10

修改后的 retention 在重启后仍持久化。

---

## AC11

Warm 状态修改 retention 时立即应用新规则。

---

## AC12

Popup、MainWindow、Permission UI 均继续使用 Quadrant-Kit。

---

## AC13

连续 500 次 Popup lifecycle 不出现明显线性内存增长。

---

# 50. Migration Plan

建议按以下阶段实施。

---

## Phase 1 — Host Lifecycle Refactor

先不改进程。

完成：

```text
UiController
 ↓
UiProcessCoordinator

PopupHost = Option
MainUiHost = Option
PermissionHost = Option
```

删除 eager Window creation。

建立 Popup state machine。

实现 30 秒 retention。

目的：

- 验证 UI lifecycle；
- 减少一次性重构风险；
- 保留现有业务调用。

---

## Phase 2 — Preference

加入：

```text
popup_idle_timeout_secs
```

完成：

- preference model
- persistence
- Settings UI
- runtime update
- tests

---

## Phase 3 — Agent / GUI Process Split

将长期业务 runtime 移入：

```text
lvos-agent
```

将 Slint runtime 留在：

```text
lvos-ui
```

建立 IPC。

同步修改发布入口：Windows 便携包必须包含 Agent 入口 `LVOS.exe` 与同目录 `lvos-ui.exe`；macOS Bundle 的 `CFBundleExecutable` 必须指向 Agent，`Contents/MacOS/lvos-ui` 作为同 Bundle helper 随包发布。启动项、Tray、Hotkey、权限申请与选区捕获均由 Agent 身份执行。

---

## Phase 4 — GUI Process Auto Exit

完成：

```text
no live hosts
 ↓
quit Slint
 ↓
exit process
```

Native lifecycle diagnostic 必须覆盖：空闲退出、退出许可前排队的新查询取消退出、旧进程完全退出后 generation 递增重建，以及迟到结果不能重启 UI。可用 `python scripts/check_ui_process_lifecycle.py` 在目标桌面系统执行。

---

## Phase 5 — Performance Validation

运行：

- cold-start benchmark
- warm lookup benchmark
- 500-cycle memory test
- Agent-only memory test

建立新的 release baseline。

实现后的 Windows 发布基线由以下命令生成：

```powershell
powershell -NoProfile -File scripts/measure-phase5-performance.ps1
```

该诊断必须等待 Popup 实际渲染回执后计时，并把聚合结果与原始进程样本写入
`target/phase5-performance/`。0.1.7 基线记录在 `PERFORMANCE_BASELINE.md`；冷启动目标仍为优化
目标，Warm latency、500-cycle 无持续线性增长、Agent-only 无 GUI process 和 timeout 后退出为
发布检查项。

---

# 51. Future Option — Dedicated Popup Process

本次不实现，但架构必须预留。

如果未来发现：

```text
lvos-ui with only PopupHost
```

仍然存在无法接受的资源成本，允许演进为：

```text
lvos-agent
lvos-popup
lvos-desktop
```

由于本 Spec 已要求：

```text
PopupHost
MainUiHost
```

业务和生命周期独立，因此未来拆第三进程应主要是：

- binary boundary
- IPC routing
- process ownership

而不需要重新设计 Popup 本身。

---

# 52. Final Architectural Rule

LVOS 的生命周期原则最终固定为：

```text
Agent
    Permanent

Main UI
    Lazy Create
    Close → Destroy

Permission UI
    Lazy Create
    Close → Destroy

Quick Lookup Popup
    Lazy Create
    Active
    Dismiss → Warm
    Default 30s
    New Lookup → Reuse + Reset Timer
    Timeout → Destroy

GUI Process
    On Demand
    No Live UI Host → Exit
```

一句话概括：

> **后台只常驻 Agent；Main UI 用完即销毁；Popup 按需创建、短时热保留、默认 30 秒后销毁；所有 GUI 继续统一使用 Quadrant-Kit。**
