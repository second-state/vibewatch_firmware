# UI 重构方案

## 背景

现在 `src/ui.rs` 和 `src/remote.rs` 的职责边界不够清楚。

`src/ui.rs` 里同时包含：

- 底层 framebuffer 和绘图辅助函数
- 字体、颜色、布局常量
- 列表、菜单、弹窗、状态页等可复用 UI
- 主菜单、设置菜单、boot 菜单、主题选择等具体界面
- terminal 文本渲染、缓存、diff、节流
- ASR 编辑器、session overlay、loading modal

`src/remote.rs` 里也包含了大量 UI 逻辑：

- 直接处理触摸坐标
- 判断点击、滑动、长按
- 判断 session list 选中了哪一行
- 控制 boot menu / theme picker / screen menu 的交互流程
- 在 active session 页面里处理退格区域和右上角菜单区域

这导致后续新增 UI 页面时，经常需要同时改 `ui.rs` 和 `remote.rs`，而且业务逻辑和触摸细节混在一起。

## 重构目标

这次重构的核心目标是把 UI 分成两层：

1. **通用组件层**
2. **由通用组件组合出来的界面层**

最终希望达到的边界是：

- 通用组件负责可复用的绘制和交互能力。
- 具体界面负责组合组件、保存界面状态、处理触摸流程，并返回语义化 action。
- `remote` 不再处理坐标、矩形、长按计时、列表命中这些 UI 细节。
- `remote` 只处理 UI 返回的 action，比如进入 session、刷新列表、关屏、发送按键。
- UI 层不直接操作 MQTT，也不直接 publish 消息。

## 数据、事件和渲染

这个设计更接近前端里的单向数据流。系统可以分成三块：

1. **数据 State**
2. **事件 Event**
3. **渲染 Render**

核心流程是：

```text
mqtt-event ┐
            ├─> update(state) + side effects(send mqtt / power / audio)
touch-event ┘

state changed -> render(current screen, components) -> lcd-flush
```

也就是说：

- `mqtt-event` 和 `touch-event` 都是事件源。
- 事件处理函数可以更新数据，也可以触发副作用，比如 send MQTT。
- 渲染不直接处理 MQTT，也不直接处理硬件策略。
- 渲染只根据当前数据和当前页面状态，调用对应组件画出来。
- 数据更新后，系统根据 dirty 标记或当前 screen 决定渲染哪些组件。

### State：数据

State 是 UI 和业务共享的当前事实。

可以先有一个总状态：

```rust
struct AppState {
    route: Route,
    sessions: SessionListState,
    active_session: ActiveSessionState,
    boot_menu: BootMenuState,
    settings: SettingsState,
    asr: AsrState,
    power: PowerState,
    dirty: DirtyFlags,
}
```

其中 `route` 表示当前显示哪个界面：

```rust
enum Route {
    MainMenu,
    Settings,
    SessionPicker,
    ActiveSession,
    BootMenu,
    ThemePicker,
    Ota,
}
```

每个页面可以有自己的 state：

```rust
struct SessionListState {
    title: String,
    items: Vec<SessionPickerItem>,
    scroll_offset: usize,
    loading: bool,
}

struct ActiveSessionState {
    terminal: TerminalViewState,
    jpeg_screen: JpegScreenState,
    loading: bool,
    backspace_pressed: bool,
    menu_pressed: bool,
}
```

这里要注意：`terminal_parser`、`terminal_renderer`、`terminal_last_render_us`、`jpeg_screen` 这类字段，本质上也是组件 state，不应该长期挂在顶层 `UI` 上。

### Event：事件

事件分两类：外部事件和内部事件。

外部事件：

```rust
enum AppEvent {
    Touch(TouchGesture),
    Mqtt(MqttEvent),
    Timer(TimerEvent),
}
```

`TouchGesture` 来自 touch driver：

- click
- long press
- swipe

`MqttEvent` 来自 MQTT client：

- session presence
- screen_text frame
- JPEG screen frame
- LWT offline
- OTA 相关消息

内部事件可以用来表达 UI 语义：

```rust
enum UiEvent {
    SessionClicked(String),
    SessionRefreshRequested,
    BackspacePressed,
    BackspaceHeld,
    ScreenMenuRequested,
    ThemeSelected(usize),
}
```

事件处理可以分两步：

```text
raw event -> current screen handle_event -> ui event
ui event / mqtt event -> update app state + effects
```

也可以先简单一点，把这两步放在一个 reducer/handler 里，后续再拆。

### Effects：副作用

事件处理除了更新 state，还可能产生副作用。

```rust
enum Effect {
    MqttPublish(MqttCommand),
    MqttSubscribe(String),
    MqttUnsubscribe(String),
    SetBacklight(BacklightMode),
    PlayAudio(AudioCue),
    PowerOff,
    Reboot,
}
```

例如点击 session：

```text
Touch click
  -> SessionPicker 根据坐标得到 SessionClicked(prefix)
  -> update state:
       route = ActiveSession
       active_session.loading = true
  -> effects:
       subscribe screen_text
       send text-mode sync
  -> render:
       ActiveSession loading
```

这样就不会变成 UI 直接 publish MQTT，也不会变成 remote 直接判断坐标。

### Render：渲染

Render 只看 state。

```text
AppState
  -> 当前 Route
    -> 对应 Screen render
      -> 组合 Components render
        -> UI framebuffer
          -> lcd-flush
```

例如：

```rust
match state.route {
    Route::SessionPicker => {
        session_picker_screen.render(ui, &state.sessions).await?;
    }
    Route::ActiveSession => {
        active_session_screen.render(ui, &state.active_session).await?;
    }
    Route::BootMenu => {
        boot_menu_screen.render(ui, &state.boot_menu).await?;
    }
}
```

渲染层不应该：

- subscribe MQTT
- publish MQTT
- 改 power policy
- 判断业务流程

渲染层可以：

- 根据 state 画 loading
- 根据 state 画 list
- 根据 state 画 terminal
- 根据 state 画 overlay
- 调用统一的 lcd flush

### lcd-flush 的位置

`lcd-flush` 是 Render 的最后一步，属于 `UI core`。

所有组件和 screen 都不直接操作 LCD driver，而是通过统一的 `UI` / `UI core`：

```text
Component render
  -> UI framebuffer
    -> async flush / rect flush
      -> lcd driver
```

这样 LCD callback、timeout、retry、flush 统计都能集中处理。

### remote 的位置

`remote` 在这个结构里更像 event loop / effect runner，而不是 UI 控制器。

它负责：

- 接收 MQTT event
- 接收 UI/touch event
- 调用 reducer 更新 `AppState`
- 执行 `Effect`
- 在 state dirty 后触发 render

它不应该负责：

- 手写坐标命中
- 手写列表滚动
- 手写长按计时
- 管 terminal dirty rect
- 直接画 modal / overlay
- 直接调用 LCD driver

关机、重启、熄屏、亮屏都属于 `Effect`，不应该由 UI 组件直接执行。

例如长按进入 boot menu 后点击关机：

```text
Touch click
  -> BootMenuScreen 根据坐标得到 UiEvent::PowerOffRequested
  -> update state:
       boot_menu.confirming = true 或 route 保持 BootMenu
  -> effects:
       PowerOff
  -> effect runner:
       调用 power/shutdown API
```

如果只是熄屏，则产生：

```rust
Effect::SetBacklight(BacklightMode::Off)
```

如果是彻底关机，则产生：

```rust
Effect::PowerOff
```

## 第一层：通用组件

通用组件是以后所有界面都可以复用的基础能力。它们不应该知道 MQTT、session、ASR 这些业务对象。

建议模块结构：

```text
src/ui/
  mod.rs
  core/
    framebuffer.rs
    style.rs
    layout.rs
    flush.rs
  components/
    list.rs
    menu.rs
    button_region.rs
    modal.rs
    overlay.rs
    status.rs
    terminal_view.rs
    jpeg_screen_view.rs
    asr_editor.rs
  screens/
    main_menu.rs
    settings_menu.rs
    session_picker.rs
    active_session.rs
    boot_menu.rs
    theme_picker.rs
    ota.rs
```

也可以先不一次性建完全部文件，迁移到哪个组件再建哪个文件。

### core

`core` 放最底层、不带业务含义的东西：

- framebuffer owner
- RGB565 颜色转换
- rect flush / full flush
- 屏幕尺寸常量
- 字体样式
- 通用布局计算
- 共享颜色

`UI` 仍然是 framebuffer 的主要 owner。组件和界面可以拿 `&mut UI` 绘制，但不要各自持有 framebuffer。

### components/list.rs

列表组件负责：

- 画标题
- 画列表 item
- 处理滚动 offset
- 根据触摸位置命中 item
- 支持上下滑动翻页
- 支持列表为空的状态

它只关心通用数据：

```rust
struct ListItem {
    label: String,
    sub_label: Option<String>,
    selected: bool,
    disabled: bool,
}
```

不应该直接知道 session prefix、MQTT topic、OTA URL。

### components/menu.rs

菜单组件负责：

- 画一组菜单项
- 当前项高亮
- 点击选择
- 可选的返回项
- 可选的禁用态

主菜单、设置菜单、boot system 菜单都应该使用这个组件。

### components/button_region.rs

这个组件用来封装屏幕上的固定点击区域。

例如：

- active session 左上角退格区域
- active session 右上角菜单区域
- 录音界面的按住说话区域

它负责：

- 判断点击是否落在区域内
- 按住时是否重复触发
- pressed 状态下画边框或图标

业务层只拿到 action，不直接判断坐标。

### components/modal.rs

弹窗组件负责：

- loading 弹窗
- 确认弹窗
- 简单提示弹窗
- 等待释放后才允许点击按钮的弹窗状态

boot 菜单之前遇到过“长按进入菜单后，release 直接触发菜单按钮”的问题，这类状态应该收进 modal 或 screen 内部，而不是散在 `remote.rs`。

### components/terminal_view.rs

terminal 组件负责：

- full frame / delta frame 解析
- vt100 cache
- dirty rect
- terminal theme
- append 节流
- async flush
- redraw cached frame

现在挂在 `UI` 上的这些字段应该整体移进 `TerminalView`：

```rust
struct TerminalView {
    parser: Option<vt100::Parser>,
    renderer: Option<embedded_graphics_terminal::TerminalRenderer>,
    last_render_us: i64,
}
```

这样 `UI` 不需要知道 terminal 的内部缓存，只提供绘制目标和 flush 能力。

这部分逻辑比较敏感，迁移时应该先只移动代码，不顺手改行为。

### components/jpeg_screen_view.rs

JPEG screen 组件负责：

- 保存最近一次 JPEG 屏幕缓存
- 渲染 JPEG frame
- redraw cached JPEG screen
- 和 terminal view 一样使用 `UI` 提供的 framebuffer / flush 能力

现在挂在 `UI` 上的这个字段应该移进 `JpegScreenView`：

```rust
struct JpegScreenView {
    screen: Option<crate::new_jpg::JpegBufferu16>,
}
```

它和 `TerminalView` 应该是两个独立组件。active session 页面可以根据收到的数据类型决定调用 terminal view 还是 JPEG screen view。

### components/asr_editor.rs

ASR editor 组件负责：

- 文字编辑区域渲染
- 录音状态边框
- connecting / listening 状态
- 按钮区域绘制

它可以先作为绘制组件存在，后续再升级成一个完整 screen。

## 第二层：界面

界面是由通用组件组合出来的 stateful object。每个界面应该同时包含：

- 当前界面状态
- 初次渲染
- 局部刷新
- 触摸事件处理
- 最终返回给业务层的 action

建议形态：

```rust
struct SessionPickerScreen {
    model: SessionPickerModel,
    scroll_offset: usize,
    loading: bool,
}

impl SessionPickerScreen {
    async fn render(&mut self, ui: &mut UI) -> anyhow::Result<()>;

    async fn run(
        &mut self,
        ui: &mut UI,
        touch: &mut TouchInput,
    ) -> anyhow::Result<SessionPickerAction>;
}
```

`run` 内部可以循环等待触摸事件，直到产生一个 action。

### MainMenuScreen

组合菜单组件，返回：

```rust
enum MainMenuAction {
    OpenSessionList,
    OpenSettings,
    OpenOta,
    ScreenOff,
}
```

### SettingsMenuScreen

组合菜单组件，返回：

```rust
enum SettingsMenuAction {
    OpenWifiList,
    OpenPowerInfo,
    OpenThemePicker,
    Back,
}
```

### SessionPickerScreen

组合列表组件、loading modal、右滑刷新手势。

返回：

```rust
enum SessionPickerAction {
    SelectSession(String),
    Refresh,
    OpenBootMenu,
    ScreenOff,
    Back,
}
```

这里的 `String` 可以先用 session prefix。后续如果需要更多信息，再改成结构体。

### ActiveSessionScreen

组合 terminal view、左上角退格区域、右上角菜单区域、loading modal。

这个 screen 也应该持有 active session 相关的视图状态：

```rust
struct ActiveSessionScreen {
    terminal: TerminalView,
    jpeg_screen: JpegScreenView,
    backspace_region: ButtonRegion,
    menu_region: ButtonRegion,
    loading: ModalState,
}
```

这样 `terminal_parser`、`terminal_renderer`、`terminal_last_render_us`、`jpeg_screen` 都不会继续堆在顶层 `UI` 里。

返回：

```rust
enum ActiveSessionAction {
    SendBackspace,
    OpenScreenMenu,
    SwipeUp,
    SwipeDown,
    WakeScreen,
}
```

屏幕文字数据仍然由 `remote` 从 MQTT 收到后交给 terminal view 渲染。ActiveSessionScreen 不直接订阅 MQTT。

### BootMenuScreen

组合菜单组件和 modal 状态。

返回：

```rust
enum BootMenuAction {
    ScreenOff,
    Reboot,
    OpenSystemMenu,
    Back,
}
```

进入 boot menu 后，先等待当前手指 release，再允许按钮触发，这个规则应该封装在 screen 内部。

### ThemePickerScreen

组合列表组件，支持上下滑动。

返回：

```rust
enum ThemePickerAction {
    SelectTheme(usize),
    Back,
}
```

主题是否持久化由上层决定。当前需求是不需要储存。

## TouchInput：通用输入基础

触摸输入建议作为 UI 层共用基础设施，而不是每个界面自己解析 `TouchEvent`。

已经有一个方向：

```rust
struct TouchInput {
    rx: tokio::sync::mpsc::Receiver<TouchEvent>,
}

enum TouchGesture {
    Click { start, end },
    LongPress { start, end, duration },
    Swipe { start, end, direction, dx, dy },
}
```

规则：

- press 是一次交互的起点。
- release 是点击和滑动的结束。
- 长按超过阈值后可以直接返回，不需要等 release。
- 长按过程中如果移动超过滑动阈值，则按滑动处理。
- 长按优先级最低。
- 当前滑动阈值统一使用 40px。

后续所有 screen 都应该使用 `TouchInput`，不要各自重复写坐标判断。

## remote 的目标形态

`remote` 最终应该变成 action dispatcher。

例如 session list：

```rust
let action = session_picker.run(&mut ui, &mut touch).await?;

match action {
    SessionPickerAction::SelectSession(prefix) => {
        server.subscribe_session_screen(&prefix).await?;
        send_active_sync(server, false).await?;
    }
    SessionPickerAction::Refresh => {
        refresh_sessions().await?;
    }
    SessionPickerAction::OpenBootMenu => {
        let action = boot_menu.run(&mut ui, &mut touch).await?;
        handle_boot_action(action).await?;
    }
    SessionPickerAction::ScreenOff => {
        power.screen_off().await?;
    }
    SessionPickerAction::Back => {}
}
```

`remote` 不应该再直接判断：

- x/y 是否落在某个按钮里
- dy 是否超过阈值
- 长按是否超过 1 秒
- 列表第几行被点击
- overlay 是否应该显示

## Model 和 Action

界面输入用 model，界面输出用 action。

以 session picker 为例：

```rust
struct SessionPickerModel {
    title: String,
    items: Vec<SessionPickerItem>,
}

struct SessionPickerItem {
    prefix: String,
    label: String,
    online: bool,
    working: bool,
}
```

UI 可以读取 model 来画界面，但不直接修改 MQTT 状态。

`remote` 负责：

- 从 MQTT state 构造 model
- 调用 screen
- 根据 action 修改业务状态
- 发送 MQTT sync / key / scroll 消息
- 控制 power / backlight / audio

## 迁移步骤

### 第一步：固定输入抽象

先保留现有 `src/ui.rs`，只引入 `TouchInput`。

目标：

- 不大规模移动 UI 文件
- 先让新 screen 有统一输入
- 让后面的页面迁移不会继续复制触摸判断

检查：

- `cargo build`
- 真机确认点击、滑动、长按语义符合预期

### 第二步：抽通用 List 和 Menu 组件

先从风险最低的列表和菜单开始。

目标：

- 新增通用 list/menu 组件
- 主菜单和设置菜单优先迁移
- 不碰 terminal 渲染
- 不碰 async flush 逻辑

这一步可以先不把整个 `src/ui.rs` 改成目录。如果一次性移动文件风险高，可以先在 `src/ui_components.rs` 或 `src/ui/components/` 里放新组件，再逐步迁移。

### 第三步：迁移 MainMenuScreen 和 SettingsMenuScreen

这两个页面业务依赖少，适合作为 screen 模式的验证。

目标：

- 页面自己 render
- 页面自己处理 touch
- 页面返回 action
- `remote` 只 match action

### 第四步：迁移 SessionPickerScreen

这是最重要的一步。

目标：

- session list 的点击、上下滑动、右滑刷新、长按 boot menu 都移动到 screen 内
- `remote` 不再关心 session list 的坐标
- 点击 session 后的 loading modal 由 screen 或 screen + remote 协作处理
- 先订阅 screen topic，再发送 sync 的业务顺序仍然由 `remote` 保证

真机重点测试：

- 点击 session 是否稳定进入
- 第一帧 screen_text 是否不会丢
- loading 是否能被 screen_text 刷新覆盖
- 右滑刷新是否正常
- 长按 boot menu 是否不会误触按钮

### 第五步：迁移 BootMenuScreen 和 ThemePickerScreen

目标：

- boot 菜单的“等待 release 后再显示/处理按钮”封装到 screen
- theme picker 使用 list 组件，支持上下滑动
- `remote` 只处理返回 action

### 第六步：迁移 ActiveSessionScreen

目标：

- 左上角退格区域变成 `ButtonRegion`
- 右上角菜单区域变成 `ButtonRegion`
- session 页面里的点击、长按、滑动统一走 `TouchInput`
- terminal view 仍然只负责渲染数据，不直接碰 MQTT

这一步风险比菜单高，建议单独 commit。

### 第七步：整理 terminal / ASR / OTA

最后再处理复杂或低频页面：

- terminal view
- ASR editor screen
- OTA screen
- status pages

terminal 迁移时只做结构移动，不改缓存、diff、timeout、flush 行为。

## Commit 建议

建议每一步一个 commit，方便真机发现问题后回退：

1. `touch: add semantic gesture input`
2. `ui: add reusable list and menu components`
3. `ui: move main and settings menus to screens`
4. `ui: move session picker to screen`
5. `ui: move boot and theme picker to screens`
6. `ui: move active session touch handling to screen`
7. `ui: organize terminal and asr components`

## 风险点

- terminal cache 和 dirty rect 逻辑容易引入显示问题，晚一点迁移。
- async flush 目前和 LCD callback / timeout 相关，重构时不要顺手改。
- `UI` 的 framebuffer 所有权要保持单一，不要让组件各自持有屏幕 buffer。
- UI screen 不直接操作 MQTT。
- remote 不直接处理触摸坐标。
- 触摸阈值统一从一个常量读取，不要在不同 screen 里写多个 40px。
- 先小范围迁移并真机验证，再继续扩大范围。

## 这次重构的验收标准

完成后，新增一个页面时应该大致只需要：

1. 定义一个 screen struct。
2. 组合已有通用组件。
3. 定义一个 action enum。
4. 在 `remote` 里 match action。

不应该再需要在 `remote` 里手写一堆坐标判断和触摸状态机。
