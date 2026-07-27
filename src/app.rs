#![allow(dead_code)]

use embedded_graphics::primitives::Rectangle;

use crate::{mqtt::MqttEvent, protocol, touch::TouchGesture, ui::UI};

const SESSION_PICKER_BOOT_LONG_PRESS_COUNT: u8 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    MainMenu,
    Settings,
    SessionPicker,
    ActiveSession,
    BootMenu,
    ThemePicker,
    Ota,
}

impl Default for Route {
    fn default() -> Self {
        Self::MainMenu
    }
}

#[derive(Debug, Default)]
pub struct AppState {
    pub route: Route,
    pub sessions: SessionListState,
    pub active_session: ActiveSessionState,
    pub boot_menu: BootMenuState,
    pub settings: SettingsState,
    pub asr: AsrState,
    pub power: PowerState,
}

#[derive(Debug, Default)]
pub struct SessionListState {
    pub title: String,
    pub items: Vec<SessionPickerItem>,
    pub scroll_offset: usize,
    pub loading: bool,
    pub boot_long_press_count: u8,
}

#[derive(Debug, Clone)]
pub struct SessionPickerItem {
    pub prefix: String,
    pub label: String,
    pub active: bool,
    pub working: bool,
}

#[derive(Debug, Default)]
pub struct ActiveSessionState {
    pub terminal: TerminalViewState,
    pub jpeg_screen: JpegScreenState,
    pub loading: bool,
    pub backspace_overlay: bool,
    pub menu_overlay: bool,
    pub clear_overlay: bool,
    pub backspace_hold_sent: bool,
    pub last_backspace_sent_at: Option<tokio::time::Instant>,
    pub pending_text_frame: Option<Vec<u8>>,
    pub pending_screen_chunk: Option<protocol::ScreenImageChunk>,
}

#[derive(Default)]
pub struct TerminalViewState {
    pub parser: Option<vt100::Parser>,
    pub renderer: Option<embedded_graphics_terminal::TerminalRenderer>,
    pub last_render_us: i64,
}

impl std::fmt::Debug for TerminalViewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalViewState")
            .field("has_parser", &self.parser.is_some())
            .field("has_renderer", &self.renderer.is_some())
            .field("last_render_us", &self.last_render_us)
            .finish()
    }
}

#[derive(Default)]
pub struct JpegScreenState {
    pub screen: Option<crate::new_jpg::JpegBufferu16>,
}

impl std::fmt::Debug for JpegScreenState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JpegScreenState")
            .field("has_screen", &self.screen.is_some())
            .finish()
    }
}

#[derive(Debug, Default)]
pub struct BootMenuState {
    pub waiting_release: bool,
    pub selected_index: Option<usize>,
}

#[derive(Debug, Default)]
pub struct SettingsState {
    pub selected_index: Option<usize>,
}

#[derive(Debug, Default)]
pub struct AsrState {
    pub connecting: bool,
    pub listening: bool,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct PowerState {
    pub backlight: BacklightState,
    pub charging: Option<bool>,
    pub battery_percent: Option<u8>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BacklightState {
    #[default]
    Normal,
    Off,
}

pub enum AppEvent {
    Touch(TouchGesture),
    Mqtt(MqttEvent),
    Timer(TimerEvent),
    Ui(UiEvent),
}

pub struct AppEventContext<'a> {
    pub session_item_rects: &'a [Rectangle],
}

pub struct AppEventResult {
    pub render: bool,
    pub effects: Vec<Effect>,
}

impl AppEventResult {
    pub fn none() -> Self {
        Self {
            render: false,
            effects: Vec::new(),
        }
    }

    pub fn render() -> Self {
        Self {
            render: true,
            effects: Vec::new(),
        }
    }

    pub fn effect(effect: Effect) -> Self {
        Self {
            render: false,
            effects: vec![effect],
        }
    }

    pub fn render_with_effect(effect: Effect) -> Self {
        Self {
            render: true,
            effects: vec![effect],
        }
    }
}

pub struct AppRenderState {
    pub session_item_rects: Vec<Rectangle>,
    pub next_title_refresh: tokio::time::Instant,
}

pub struct SessionSyncResult {
    pub render: bool,
    pub play_prompt: bool,
}

impl AppRenderState {
    pub fn new() -> Self {
        Self {
            session_item_rects: Vec::new(),
            next_title_refresh: tokio::time::Instant::now() + crate::ui::MENU_TITLE_REFRESH_DELAY,
        }
    }
}

impl AppState {
    pub fn session_picker() -> Self {
        Self {
            route: Route::SessionPicker,
            ..Default::default()
        }
    }

    pub fn sync_sessions(
        &mut self,
        title: String,
        labels: Vec<(String, String, bool, bool)>,
    ) -> SessionSyncResult {
        let play_prompt = self.route == Route::SessionPicker
            && labels.iter().any(|(prefix, _, _, working)| {
                !*working
                    && self
                        .sessions
                        .items
                        .iter()
                        .any(|item| item.prefix == *prefix && item.working)
            });
        self.sessions.title = title;
        self.sessions.items = labels
            .into_iter()
            .map(|(prefix, label, active, working)| SessionPickerItem {
                prefix,
                label,
                active,
                working,
            })
            .collect();
        self.sessions.scroll_offset =
            clamp_scroll_offset(self.sessions.scroll_offset, self.sessions.items.len(), 1);
        SessionSyncResult {
            render: self.route == Route::SessionPicker,
            play_prompt,
        }
    }

    pub fn set_session_title(&mut self, title: String) -> bool {
        if self.sessions.title == title {
            return false;
        }
        self.sessions.title = title;
        self.route == Route::SessionPicker
    }

    pub fn wake_screen(&self) -> bool {
        matches!(self.route, Route::SessionPicker | Route::ActiveSession)
    }

    pub fn request_render_for_current_route(&self) -> bool {
        matches!(self.route, Route::SessionPicker | Route::ActiveSession)
    }

    pub fn handle_event(
        &mut self,
        event: AppEvent,
        context: &AppEventContext<'_>,
    ) -> AppEventResult {
        match event {
            AppEvent::Touch(gesture) => self.handle_touch_event(gesture, context),
            AppEvent::Mqtt(event) => self.handle_mqtt_event(event),
            AppEvent::Timer(event) => self.handle_timer_event(event),
            AppEvent::Ui(event) => self.handle_ui_event(event),
        }
    }

    pub async fn render(
        &mut self,
        gui: &mut UI,
        render_state: &mut AppRenderState,
    ) -> anyhow::Result<()> {
        match self.route {
            Route::SessionPicker => {
                self.render_session_picker(gui, render_state).await?;
                render_state.next_title_refresh =
                    tokio::time::Instant::now() + crate::ui::MENU_TITLE_REFRESH_DELAY;
            }
            Route::ActiveSession => {
                self.render_active_session(gui).await?;
            }
            Route::MainMenu
            | Route::Settings
            | Route::BootMenu
            | Route::ThemePicker
            | Route::Ota => {}
        }

        Ok(())
    }

    pub fn all_sessions_idle(&self) -> bool {
        self.sessions.items.iter().all(|item| !item.working)
    }

    fn handle_touch_event(
        &mut self,
        gesture: TouchGesture,
        context: &AppEventContext<'_>,
    ) -> AppEventResult {
        match self.route {
            Route::SessionPicker => self.handle_session_picker_touch(gesture, context),
            Route::ActiveSession => self.handle_active_session_touch(gesture),
            Route::MainMenu
            | Route::Settings
            | Route::BootMenu
            | Route::ThemePicker
            | Route::Ota => AppEventResult::none(),
        }
    }

    fn handle_session_picker_touch(
        &mut self,
        gesture: TouchGesture,
        context: &AppEventContext<'_>,
    ) -> AppEventResult {
        match gesture {
            TouchGesture::Press { .. } => {
                self.sessions.boot_long_press_count = 0;
                AppEventResult::none()
            }
            TouchGesture::Click { start, end } => {
                self.sessions.boot_long_press_count = 0;
                let press_index = crate::ui::list_touch_index(start, context.session_item_rects);
                let release_index = crate::ui::list_touch_index(end, context.session_item_rects);
                if press_index.is_none() || press_index != release_index {
                    return AppEventResult::none();
                }

                let visible_index = press_index.unwrap();
                let index = self.sessions.scroll_offset + visible_index;
                let Some(item) = self.sessions.items.get(index) else {
                    return AppEventResult::none();
                };

                let prefix = item.prefix.clone();
                log::info!("new UI session selected: {prefix}");
                self.route = Route::ActiveSession;
                self.active_session.loading = true;

                AppEventResult {
                    render: true,
                    effects: vec![
                        Effect::SelectSession(prefix),
                        Effect::MqttPublish(MqttCommand::SendSync { close: false }),
                    ],
                }
            }
            TouchGesture::LongPress { .. } => {
                self.sessions.boot_long_press_count =
                    self.sessions.boot_long_press_count.saturating_add(1);
                if self.sessions.boot_long_press_count >= SESSION_PICKER_BOOT_LONG_PRESS_COUNT {
                    self.sessions.boot_long_press_count = 0;
                    AppEventResult::effect(Effect::OpenBootMenu)
                } else {
                    AppEventResult::none()
                }
            }
            TouchGesture::Swipe {
                start,
                end,
                direction,
                dx,
                dy,
            } => {
                self.sessions.boot_long_press_count = 0;
                match direction {
                    crate::touch::SwipeDirection::Right => {
                        log::info!("new UI session list right swipe detected, refreshing");
                        AppEventResult::render()
                    }
                    crate::touch::SwipeDirection::Up | crate::touch::SwipeDirection::Down => {
                        let Some(delta) = list_scroll_delta(start, end) else {
                            return AppEventResult::none();
                        };
                        let visible_count = context.session_item_rects.len().max(1);
                        let next_offset = apply_scroll_delta(
                            self.sessions.scroll_offset,
                            self.sessions.items.len(),
                            visible_count,
                            delta,
                        );
                        if next_offset == self.sessions.scroll_offset {
                            return AppEventResult::none();
                        }
                        self.sessions.scroll_offset = next_offset;
                        log::info!(
                            "new UI session list scroll offset={} dx={} dy={}",
                            self.sessions.scroll_offset,
                            dx,
                            dy
                        );
                        AppEventResult::render()
                    }
                    crate::touch::SwipeDirection::Left => AppEventResult::none(),
                }
            }
        }
    }

    fn handle_active_session_touch(&mut self, gesture: TouchGesture) -> AppEventResult {
        match gesture {
            TouchGesture::Press { point } => {
                if is_screen_backspace_point(point) {
                    log::info!("new UI backspace press");
                    self.active_session.backspace_overlay = true;
                    self.active_session.menu_overlay = false;
                    self.active_session.backspace_hold_sent = false;
                    self.active_session.last_backspace_sent_at = None;
                    AppEventResult::render()
                } else if is_screen_menu_point(point) {
                    log::info!("new UI screen menu press");
                    self.active_session.menu_overlay = true;
                    self.active_session.backspace_overlay = false;
                    AppEventResult::render()
                } else {
                    AppEventResult::none()
                }
            }
            TouchGesture::Click { start, end } => {
                let had_overlay =
                    self.active_session.backspace_overlay || self.active_session.menu_overlay;
                if is_screen_backspace_point(start) && is_screen_backspace_point(end) {
                    log::info!("new UI backspace click");
                    self.active_session.backspace_overlay = false;
                    self.active_session.menu_overlay = false;
                    self.active_session.clear_overlay = had_overlay;
                    let send_click = !self.active_session.backspace_hold_sent;
                    self.active_session.backspace_hold_sent = false;
                    self.active_session.last_backspace_sent_at = None;
                    if send_click {
                        AppEventResult::render_with_effect(Effect::MqttPublish(
                            MqttCommand::SendKey {
                                key: "\x7f".to_string(),
                            },
                        ))
                    } else {
                        AppEventResult::render()
                    }
                } else if is_screen_menu_point(start) && is_screen_menu_point(end) {
                    log::info!("new UI screen menu click");
                    self.active_session.menu_overlay = false;
                    self.active_session.backspace_overlay = false;
                    self.active_session.clear_overlay = had_overlay;
                    AppEventResult::render_with_effect(Effect::OpenScreenMenu)
                } else if is_asr_touch(start) && is_asr_touch(end) {
                    self.active_session.backspace_overlay = false;
                    self.active_session.menu_overlay = false;
                    self.active_session.clear_overlay = had_overlay;
                    AppEventResult::effect(Effect::OpenAsrEditor)
                } else {
                    self.active_session.backspace_overlay = false;
                    self.active_session.menu_overlay = false;
                    self.active_session.clear_overlay = had_overlay;
                    if had_overlay {
                        AppEventResult::render()
                    } else {
                        AppEventResult::none()
                    }
                }
            }
            TouchGesture::LongPress { start, end, .. } => {
                if is_screen_backspace_point(start) && is_screen_backspace_point(end) {
                    const BACKSPACE_REPEAT_DELAY: std::time::Duration =
                        std::time::Duration::from_millis(500);
                    let now = tokio::time::Instant::now();
                    if self
                        .active_session
                        .last_backspace_sent_at
                        .is_some_and(|sent_at| now.duration_since(sent_at) < BACKSPACE_REPEAT_DELAY)
                    {
                        return AppEventResult::none();
                    }
                    log::info!("new UI backspace long press");
                    self.active_session.backspace_overlay = true;
                    self.active_session.backspace_hold_sent = true;
                    self.active_session.last_backspace_sent_at = Some(now);
                    AppEventResult::effect(Effect::MqttPublish(MqttCommand::SendKey {
                        key: "\x7f".to_string(),
                    }))
                } else {
                    AppEventResult::none()
                }
            }
            TouchGesture::Swipe { start, end, .. } => {
                let had_overlay =
                    self.active_session.backspace_overlay || self.active_session.menu_overlay;
                self.active_session.backspace_overlay = false;
                self.active_session.menu_overlay = false;
                self.active_session.clear_overlay = had_overlay;
                self.active_session.backspace_hold_sent = false;
                self.active_session.last_backspace_sent_at = None;
                if is_back_swipe(start, end) {
                    log::info!("new UI right swipe detected, returning to session list");
                    self.route = Route::SessionPicker;
                    self.active_session.loading = false;
                    AppEventResult {
                        render: true,
                        effects: vec![
                            Effect::MqttPublish(MqttCommand::SendSync { close: true }),
                            Effect::ClearActiveSession,
                        ],
                    }
                } else if let Some(msg) = scroll_swipe_message(start, end) {
                    if had_overlay {
                        AppEventResult::render_with_effect(Effect::MqttPublish(msg))
                    } else {
                        AppEventResult::effect(Effect::MqttPublish(msg))
                    }
                } else {
                    if had_overlay {
                        AppEventResult::render()
                    } else {
                        AppEventResult::none()
                    }
                }
            }
        }
    }

    fn handle_mqtt_event(&mut self, event: MqttEvent) -> AppEventResult {
        match event {
            MqttEvent::ActiveScreen(chunk) => {
                self.route = Route::ActiveSession;
                self.active_session.loading = false;
                self.active_session.pending_screen_chunk = Some(chunk);
                AppEventResult::render()
            }
            MqttEvent::ActiveText(frame) => {
                self.route = Route::ActiveSession;
                self.active_session.loading = false;
                self.active_session.pending_text_frame = Some(frame);
                AppEventResult::render()
            }
            MqttEvent::Presence {
                prefix,
                online,
                list_changed,
                was_active,
            } => {
                log::info!(
                    "new UI presence: {prefix} online={online} list_changed={list_changed} was_active={was_active}"
                );
                if !online && was_active {
                    log::warn!("new UI active session offline; returning to session list");
                    self.route = Route::SessionPicker;
                    self.active_session.loading = false;
                    return AppEventResult::render_with_effect(Effect::SetBacklight(
                        BacklightState::Normal,
                    ));
                }
                if list_changed {
                    return AppEventResult::render_with_effect(Effect::SetBacklight(
                        BacklightState::Normal,
                    ));
                }
                AppEventResult::none()
            }
        }
    }

    fn handle_timer_event(&mut self, event: TimerEvent) -> AppEventResult {
        match event {
            TimerEvent::IdleShutdownCheck => {
                AppEventResult::effect(Effect::SetBacklight(BacklightState::Off))
            }
            TimerEvent::BackspaceRepeat | TimerEvent::RenderThrottleExpired => {
                AppEventResult::none()
            }
        }
    }

    fn handle_ui_event(&mut self, event: UiEvent) -> AppEventResult {
        match event {
            UiEvent::SessionRefreshRequested => AppEventResult::render(),
            UiEvent::ScreenOffRequested => {
                AppEventResult::effect(Effect::SetBacklight(BacklightState::Off))
            }
            UiEvent::PowerOffRequested => AppEventResult::effect(Effect::PowerOff),
            UiEvent::RebootRequested => AppEventResult::effect(Effect::Reboot),
            UiEvent::SessionClicked(prefix) => {
                self.route = Route::ActiveSession;
                self.active_session.loading = true;
                AppEventResult {
                    render: true,
                    effects: vec![
                        Effect::SelectSession(prefix),
                        Effect::MqttPublish(MqttCommand::SendSync { close: false }),
                    ],
                }
            }
            UiEvent::BackspacePressed | UiEvent::BackspaceHeld => {
                AppEventResult::effect(Effect::MqttPublish(MqttCommand::SendKey {
                    key: "\x7f".to_string(),
                }))
            }
            UiEvent::ScreenMenuRequested => AppEventResult::effect(Effect::OpenScreenMenu),
            UiEvent::ThemeSelected(index) => AppEventResult::effect(Effect::SelectTheme(index)),
        }
    }

    async fn render_session_picker(
        &mut self,
        gui: &mut UI,
        render_state: &mut AppRenderState,
    ) -> anyhow::Result<()> {
        if self.sessions.items.is_empty() {
            gui.show_status("no session", "").await?;
            render_state.session_item_rects.clear();
            return Ok(());
        }

        let visible_hint = render_state.session_item_rects.len().max(1);
        self.sessions.scroll_offset = clamp_scroll_offset(
            self.sessions.scroll_offset,
            self.sessions.items.len(),
            visible_hint,
        );

        let items: Vec<(String, bool)> = self
            .sessions
            .items
            .iter()
            .skip(self.sessions.scroll_offset)
            .map(|item| (item.label.clone(), item.working))
            .collect();
        render_state.session_item_rects =
            gui.display_menu_list(&self.sessions.title, &items).await?;
        Ok(())
    }

    async fn render_active_session(&mut self, gui: &mut UI) -> anyhow::Result<()> {
        if let Some(chunk) = self.active_session.pending_screen_chunk.take() {
            render_screen_chunk(gui, chunk).await?;
        }
        if let Some(frame) = self.active_session.pending_text_frame.take() {
            // log::info!("new UI screen text frame: {}B", frame.len());
            match screen_text_frame_kind(&frame) {
                Some(ScreenTextFrameKind::Full) => {
                    gui.show_terminal_text_frame(&frame).await?;
                }
                Some(ScreenTextFrameKind::Append) => {
                    gui.buffer_terminal_text_frame(&frame).await?;
                }
                None => {
                    gui.show_terminal_text_frame(&frame).await?;
                }
            }
        }
        if self.active_session.loading {
            gui.show_loading_modal().await?;
        }
        if self.active_session.clear_overlay {
            redraw_active_cached_screen(gui).await?;
            self.active_session.clear_overlay = false;
        }
        if self.active_session.backspace_overlay {
            gui.show_session_backspace_overlay().await?;
        }
        if self.active_session.menu_overlay {
            gui.show_session_menu_overlay().await?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenTextFrameKind {
    Full,
    Append,
}

fn screen_text_frame_kind(frame: &[u8]) -> Option<ScreenTextFrameKind> {
    match frame.first().copied() {
        Some(0x00) => Some(ScreenTextFrameKind::Full),
        Some(0x01) => Some(ScreenTextFrameKind::Append),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimerEvent {
    IdleShutdownCheck,
    BackspaceRepeat,
    RenderThrottleExpired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEvent {
    SessionClicked(String),
    SessionRefreshRequested,
    BackspacePressed,
    BackspaceHeld,
    ScreenMenuRequested,
    ThemeSelected(usize),
    ScreenOffRequested,
    PowerOffRequested,
    RebootRequested,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    MqttPublish(MqttCommand),
    MqttSubscribe(String),
    MqttUnsubscribe(String),
    SetBacklight(BacklightState),
    PlayAudio(AudioCue),
    SelectSession(String),
    ClearActiveSession,
    OpenBootMenu,
    OpenScreenMenu,
    OpenAsrEditor,
    SelectTheme(usize),
    PowerOff,
    Reboot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MqttCommand {
    SendSync { close: bool },
    SendKey { key: String },
    SendScrollUp { rows: u16 },
    SendScrollDown { rows: u16 },
    Publish { topic: String, payload: Vec<u8> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCue {
    Prompt,
}

async fn render_screen_chunk(
    gui: &mut UI,
    chunk: protocol::ScreenImageChunk,
) -> anyhow::Result<()> {
    if !matches!(chunk.format, protocol::ImageFormat::Jpeg) {
        log::warn!("Unsupported screen format {:?}, only JPEG", chunk.format);
        return Ok(());
    }
    let data = &chunk.data;
    let jpeg: &[u8] = if data.len() >= 4 {
        &data[..data.len() - 4]
    } else {
        data.as_slice()
    };
    log::info!("new UI screen frame: {}B jpeg", jpeg.len());
    match crate::new_jpg::esp_jpeg_decode_one_picture(jpeg) {
        Ok(display) => gui.show_jpeg_screen(display).await?,
        Err(e) => log::error!("decode JPEG failed: {e:?}"),
    }
    Ok(())
}

async fn redraw_active_cached_screen(gui: &mut UI) -> anyhow::Result<()> {
    if !gui.redraw_cached_terminal_text().await? && !gui.redraw_cached_jpeg_screen().await? {
        log::warn!("no cached active screen to redraw after overlay");
    }
    Ok(())
}

fn clamp_scroll_offset(offset: usize, total_items: usize, visible_count: usize) -> usize {
    offset.min(total_items.saturating_sub(visible_count))
}

fn apply_scroll_delta(
    offset: usize,
    total_items: usize,
    visible_count: usize,
    delta: isize,
) -> usize {
    let max_offset = total_items.saturating_sub(visible_count);
    if delta > 0 {
        offset.saturating_add(delta as usize).min(max_offset)
    } else {
        offset.saturating_sub((-delta) as usize)
    }
}

fn list_scroll_delta(start: crate::lcd::TouchPoint, end: crate::lcd::TouchPoint) -> Option<isize> {
    let dx = (end.x as i32 - start.x as i32).abs();
    let dy = end.y as i32 - start.y as i32;
    if dx > crate::touch::DEFAULT_SWIPE_THRESHOLD_PX
        || dy.abs() < crate::touch::DEFAULT_SWIPE_THRESHOLD_PX
    {
        return None;
    }

    if dy < 0 {
        Some(-1)
    } else {
        Some(1)
    }
}

fn is_back_swipe(start: crate::lcd::TouchPoint, end: crate::lcd::TouchPoint) -> bool {
    let dx = end.x as i32 - start.x as i32;
    let dy = (end.y as i32 - start.y as i32).abs();
    dx >= crate::touch::DEFAULT_SWIPE_THRESHOLD_PX && dy <= crate::touch::DEFAULT_SWIPE_THRESHOLD_PX
}

fn is_screen_menu_point(touch: crate::lcd::TouchPoint) -> bool {
    touch.y < 80 && touch.x >= crate::lcd::LCD_WIDTH * 2 / 3
}

fn is_screen_backspace_point(touch: crate::lcd::TouchPoint) -> bool {
    touch.y < 80 && touch.x < crate::lcd::LCD_WIDTH / 3
}

fn is_asr_touch(touch: crate::lcd::TouchPoint) -> bool {
    touch.y > crate::lcd::LCD_HEIGHT.saturating_sub(80)
}

fn scroll_swipe_message(
    start: crate::lcd::TouchPoint,
    end: crate::lcd::TouchPoint,
) -> Option<MqttCommand> {
    let dx = (end.x as i32 - start.x as i32).abs();
    let dy = end.y as i32 - start.y as i32;
    if dx > crate::touch::DEFAULT_SWIPE_THRESHOLD_PX
        || dy.abs() < crate::touch::DEFAULT_SWIPE_THRESHOLD_PX
    {
        return None;
    }

    let rows = 15;
    if dy < 0 {
        Some(MqttCommand::SendScrollDown { rows })
    } else {
        Some(MqttCommand::SendScrollUp { rows })
    }
}
