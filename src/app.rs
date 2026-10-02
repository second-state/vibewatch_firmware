#![allow(dead_code)]

use crate::{mqtt::MqttEvent, protocol, touch::TouchGesture, ui::UI};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Clock,
    MainMenu,
    Settings,
    SessionPicker,
    ActiveSession,
    AsrEditor,
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
    pub reconnecting: bool,
    pub reconnect_dots: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub scroll_preview_y_offset: i32,
    pub scroll_preview_redraw: bool,
    pub backspace_hold_sent: bool,
    pub last_backspace_sent_at: Option<tokio::time::Instant>,
    pub pending_text_frame: Option<Vec<u8>>,
    pub pending_screen_chunk: Option<protocol::ScreenImageChunk>,
    pub controls_visible: bool,
    pub action_index: usize,
}

impl ActiveSessionState {
    pub fn action(&self) -> crate::watch_ui::AgentTuiAction {
        crate::watch_ui::AgentTuiAction::from_index(self.action_index)
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingAction {
    Ota,
    SyncTime,
    Ble,
    Reboot,
    PowerOff,
}

#[derive(Debug, Default)]
pub struct SettingsState {
    pub selected_index: Option<usize>,
    pub exit_action: Option<SettingAction>,
}

#[derive(Debug, Default)]
pub struct AsrState {
    pub text: String,
    pub cursor: usize,
    pub hint: &'static str,
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
    pub main_menu_hits: &'a crate::watch_ui::MainMenuHitRegions,
    pub session_list_hits: &'a crate::watch_ui::SessionListHitRegions,
    pub active_session_hits: &'a crate::watch_ui::ActiveSessionHitRegions,
    pub voice_input_hits: &'a crate::watch_ui::VoiceInputHitRegions,
    pub settings_hits: &'a crate::watch_ui::SettingsListHitRegions,
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
    pub main_menu_hits: crate::watch_ui::MainMenuHitRegions,
    pub session_list_hits: crate::watch_ui::SessionListHitRegions,
    pub active_session_hits: crate::watch_ui::ActiveSessionHitRegions,
    pub voice_input_hits: crate::watch_ui::VoiceInputHitRegions,
    pub settings_hits: crate::watch_ui::SettingsListHitRegions,
    pub next_clock_tick: tokio::time::Instant,
    pub next_title_refresh: tokio::time::Instant,
    pub next_mqtt_reconnect_refresh: tokio::time::Instant,
}

pub struct SessionSyncResult {
    pub render: bool,
    pub play_prompt: bool,
    pub session_activity: bool,
}

impl AppRenderState {
    pub fn new() -> Self {
        Self {
            main_menu_hits: crate::watch_ui::MainMenuHitRegions::default(),
            session_list_hits: crate::watch_ui::SessionListHitRegions::default(),
            active_session_hits: crate::watch_ui::ActiveSessionHitRegions::default(),
            voice_input_hits: crate::watch_ui::VoiceInputHitRegions::default(),
            settings_hits: crate::watch_ui::SettingsListHitRegions::default(),
            next_clock_tick: tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            next_title_refresh: tokio::time::Instant::now() + crate::ui::MENU_TITLE_REFRESH_DELAY,
            next_mqtt_reconnect_refresh: tokio::time::Instant::now()
                + std::time::Duration::from_secs(1),
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

    /// Sets the active route and logs heap usage on every page transition.
    fn enter_route(&mut self, route: Route) {
        if self.route == route {
            return;
        }
        self.route = route;
        crate::util::log_heap_usage(&format!("route -> {route:?}"));
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
        let next_items: Vec<SessionPickerItem> = labels
            .into_iter()
            .map(|(prefix, label, active, working)| SessionPickerItem {
                prefix,
                label,
                active,
                working,
            })
            .collect();
        let structure_changed = self.sessions.items.len() != next_items.len()
            || self
                .sessions
                .items
                .iter()
                .zip(next_items.iter())
                .any(|(old, new)| old.prefix != new.prefix);
        let working_changed = self
            .sessions
            .items
            .iter()
            .zip(next_items.iter())
            .any(|(old, new)| old.prefix == new.prefix && old.working != new.working);
        self.sessions.title = title;
        self.sessions.items = next_items;
        self.sessions.scroll_offset =
            clamp_scroll_offset(self.sessions.scroll_offset, self.sessions.items.len(), 1);
        SessionSyncResult {
            render: (structure_changed || working_changed) && self.route == Route::SessionPicker,
            play_prompt,
            session_activity: working_changed,
        }
    }

    pub fn set_session_title(&mut self, title: String) -> bool {
        if self.sessions.reconnecting {
            return false;
        }
        if self.sessions.title == title {
            return false;
        }
        self.sessions.title = title;
        self.route == Route::SessionPicker
    }

    pub fn wake_screen(&self) -> bool {
        matches!(
            self.route,
            Route::Clock
                | Route::MainMenu
                | Route::SessionPicker
                | Route::ActiveSession
                | Route::AsrEditor
        )
    }

    pub fn request_render_for_current_route(&self) -> bool {
        matches!(
            self.route,
            Route::Clock
                | Route::MainMenu
                | Route::SessionPicker
                | Route::ActiveSession
                | Route::AsrEditor
        )
    }

    pub fn is_mqtt_reconnecting(&self) -> bool {
        self.route == Route::SessionPicker && self.sessions.reconnecting
    }

    pub fn tick_mqtt_reconnecting(&mut self) -> bool {
        if !self.is_mqtt_reconnecting() {
            return false;
        }
        self.sessions.reconnect_dots = (self.sessions.reconnect_dots + 1) % 3;
        true
    }

    pub fn enter_mqtt_reconnecting(&mut self) -> bool {
        let changed = !self.sessions.reconnecting
            || self.route != Route::SessionPicker
            || !self.sessions.items.is_empty();
        self.enter_route(Route::SessionPicker);
        self.sessions.title.clear();
        self.sessions.items.clear();
        self.sessions.scroll_offset = 0;
        self.sessions.reconnecting = true;
        self.sessions.reconnect_dots = 0;
        self.active_session.loading = false;
        changed
    }

    pub fn exit_mqtt_reconnecting(&mut self) -> bool {
        if !self.sessions.reconnecting {
            return false;
        }
        self.enter_route(Route::SessionPicker);
        self.sessions.reconnecting = false;
        self.sessions.reconnect_dots = 0;
        self.active_session.loading = false;
        true
    }

    pub fn return_to_session_picker(&mut self) {
        self.enter_route(Route::SessionPicker);
        self.active_session.loading = false;
        self.active_session.backspace_overlay = false;
        self.active_session.menu_overlay = false;
        self.active_session.clear_overlay = false;
        self.active_session.scroll_preview_y_offset = 0;
        self.active_session.scroll_preview_redraw = false;
        self.active_session.backspace_hold_sent = false;
        self.active_session.last_backspace_sent_at = None;
        self.active_session.pending_text_frame = None;
        self.active_session.pending_screen_chunk = None;
        self.active_session.controls_visible = false;
        self.active_session.action_index = 0;
    }

    pub fn open_asr_editor(&mut self) {
        self.enter_route(Route::AsrEditor);
        self.asr.text.clear();
        self.asr.cursor = 0;
        self.asr.hint = "Hold Record";
    }

    pub fn return_from_asr_editor(&mut self) {
        self.enter_route(Route::ActiveSession);
        self.asr.hint = "Hold Record";
    }

    pub fn set_asr_connecting(&mut self) {
        self.asr.hint = "Connecting...";
    }

    pub fn set_asr_listening(&mut self) {
        self.asr.hint = "Listening...";
    }

    pub fn apply_asr_result(&mut self, result: anyhow::Result<Option<String>>) {
        match result {
            Ok(Some(text)) => {
                let text = text.trim();
                log::info!("Local ASR result: {text}");
                self.asr_insert_str(&format!("{text} "));
                self.asr.hint = "Hold Record";
            }
            Ok(None) => {
                self.asr.hint = "(empty)";
            }
            Err(e) => {
                log::error!("ASR failed: {e:?}");
                self.asr.hint = "ASR error";
            }
        }
    }

    pub fn asr_display_text(&self) -> String {
        let mut out = String::with_capacity(self.asr.text.len() + 1);
        for (i, ch) in self.asr.text.chars().enumerate() {
            if i == self.asr.cursor {
                out.push('|');
            }
            out.push(ch);
        }
        if self.asr.cursor >= self.asr_char_len() {
            out.push('|');
        }
        out
    }

    fn asr_insert_str(&mut self, s: &str) {
        let byte_pos = self.asr_cursor_byte_pos();
        self.asr.text.insert_str(byte_pos, s);
        self.asr.cursor += s.chars().count();
    }

    fn asr_backspace(&mut self) {
        if self.asr.cursor == 0 {
            return;
        }
        let byte_pos = self
            .asr
            .text
            .char_indices()
            .nth(self.asr.cursor - 1)
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.asr.text.remove(byte_pos);
        self.asr.cursor -= 1;
    }

    fn asr_move_left(&mut self) {
        self.asr.cursor = self.asr.cursor.saturating_sub(1);
    }

    fn asr_move_right(&mut self) {
        self.asr.cursor = self.asr.cursor.saturating_add(1).min(self.asr_char_len());
    }

    fn asr_take_trimmed(&mut self) -> String {
        self.asr.text.truncate(self.asr.text.trim_end().len());
        let text = self.asr.text.trim_start().to_string();
        self.asr.text.clear();
        self.asr.cursor = 0;
        text
    }

    fn asr_char_len(&self) -> usize {
        self.asr.text.chars().count()
    }

    fn asr_cursor_byte_pos(&self) -> usize {
        self.asr
            .text
            .char_indices()
            .nth(self.asr.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.asr.text.len())
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
            Route::Clock => {
                gui.show_clock().await?;
                render_state.next_clock_tick =
                    tokio::time::Instant::now() + std::time::Duration::from_secs(1);
            }
            Route::MainMenu => {
                render_state.main_menu_hits = gui.display_main_menu().await?;
                render_state.next_title_refresh =
                    tokio::time::Instant::now() + crate::ui::MENU_TITLE_REFRESH_DELAY;
            }
            Route::SessionPicker => {
                self.render_session_picker(gui, render_state).await?;
                render_state.next_title_refresh =
                    tokio::time::Instant::now() + crate::ui::MENU_TITLE_REFRESH_DELAY;
            }
            Route::ActiveSession => {
                self.render_active_session(gui, render_state).await?;
            }
            Route::AsrEditor => {
                gui.show_asr_editor(&self.asr_display_text(), self.asr.hint)
                    .await?;
            }
            Route::Settings => {
                render_state.settings_hits = gui.display_settings_list().await?;
            }
            Route::BootMenu | Route::ThemePicker | Route::Ota => {}
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
            Route::Clock => self.handle_clock_touch(gesture),
            Route::MainMenu => self.handle_main_menu_touch(gesture, context),
            Route::SessionPicker => self.handle_session_picker_touch(gesture, context),
            Route::ActiveSession => self.handle_active_session_touch(gesture, context),
            Route::AsrEditor => self.handle_asr_editor_touch(gesture, context),
            Route::Settings => self.handle_settings_touch(gesture, context),
            Route::BootMenu | Route::ThemePicker | Route::Ota => AppEventResult::none(),
        }
    }

    fn handle_clock_touch(&mut self, gesture: TouchGesture) -> AppEventResult {
        if matches!(gesture, TouchGesture::Click { .. }) {
            self.enter_route(Route::MainMenu);
            AppEventResult::render()
        } else {
            AppEventResult::none()
        }
    }

    fn handle_main_menu_touch(
        &mut self,
        gesture: TouchGesture,
        context: &AppEventContext<'_>,
    ) -> AppEventResult {
        match gesture {
            TouchGesture::Click { start, end } => match context.main_menu_hits.hit_pair(start, end)
            {
                Some(crate::watch_ui::MainMenuHit::Back) => {
                    self.enter_route(Route::Clock);
                    AppEventResult::render()
                }
                Some(crate::watch_ui::MainMenuHit::Top) => {
                    self.enter_route(Route::SessionPicker);
                    AppEventResult::render()
                }
                Some(crate::watch_ui::MainMenuHit::Bottom) => {
                    self.enter_route(Route::Settings);
                    self.settings.exit_action = None;
                    AppEventResult::render()
                }
                None => AppEventResult::none(),
            },
            TouchGesture::Swipe {
                direction: crate::touch::SwipeDirection::Right,
                ..
            } => {
                self.enter_route(Route::Clock);
                AppEventResult::render()
            }
            _ => AppEventResult::none(),
        }
    }

    fn handle_session_picker_touch(
        &mut self,
        gesture: TouchGesture,
        context: &AppEventContext<'_>,
    ) -> AppEventResult {
        match gesture {
            TouchGesture::Press { .. } => AppEventResult::none(),
            TouchGesture::Click { start, end } => {
                if context.session_list_hits.back_hit_pair(start, end) {
                    self.enter_route(Route::MainMenu);
                    return AppEventResult::none();
                }
                let press_index = context.session_list_hits.hit_index(start);
                let release_index = context.session_list_hits.hit_index(end);
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
                self.enter_route(Route::ActiveSession);
                self.active_session.loading = true;
                self.active_session.controls_visible = false;
                self.active_session.action_index = 0;

                AppEventResult {
                    render: true,
                    effects: vec![
                        Effect::SelectSession(prefix),
                        Effect::MqttPublish(MqttCommand::SendSync { close: false }),
                    ],
                }
            }
            TouchGesture::LongPress { .. }
            | TouchGesture::SwipePreview { .. }
            | TouchGesture::SwipeCancel { .. } => AppEventResult::none(),
            TouchGesture::Swipe {
                start,
                end,
                direction,
                dx,
                dy,
            } => match direction {
                crate::touch::SwipeDirection::Right => {
                    log::info!("new UI session list right swipe detected, going back to main menu");
                    self.enter_route(Route::MainMenu);
                    AppEventResult::none()
                }
                crate::touch::SwipeDirection::Up | crate::touch::SwipeDirection::Down => {
                    let Some(delta) = list_scroll_delta(start, end) else {
                        return AppEventResult::none();
                    };
                    let visible_count = context.session_list_hits.visible_count().max(1);
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
            },
        }
    }

    fn handle_settings_touch(
        &mut self,
        gesture: TouchGesture,
        context: &AppEventContext<'_>,
    ) -> AppEventResult {
        match gesture {
            TouchGesture::Press { .. } => AppEventResult::none(),
            TouchGesture::Click { start, end } => {
                if context.settings_hits.back_hit_pair(start, end) {
                    self.enter_route(Route::MainMenu);
                    return AppEventResult::none();
                }
                let press_index = context.settings_hits.hit_index(start);
                let release_index = context.settings_hits.hit_index(end);
                if press_index.is_none() || press_index != release_index {
                    return AppEventResult::none();
                }

                let action = match press_index.unwrap() {
                    0 => SettingAction::Ota,
                    1 => SettingAction::SyncTime,
                    2 => SettingAction::Ble,
                    3 => SettingAction::Reboot,
                    4 => SettingAction::PowerOff,
                    _ => return AppEventResult::none(),
                };
                log::info!("new UI settings action selected: {action:?}");
                self.settings.exit_action = Some(action);
                AppEventResult::none()
            }
            TouchGesture::LongPress { .. }
            | TouchGesture::SwipePreview { .. }
            | TouchGesture::SwipeCancel { .. } => AppEventResult::none(),
            TouchGesture::Swipe {
                direction: crate::touch::SwipeDirection::Right,
                ..
            } => {
                self.enter_route(Route::MainMenu);
                AppEventResult::none()
            }
            TouchGesture::Swipe { .. } => AppEventResult::none(),
        }
    }

    fn handle_active_session_touch(
        &mut self,
        gesture: TouchGesture,
        context: &AppEventContext<'_>,
    ) -> AppEventResult {
        match gesture {
            TouchGesture::Press { .. } if self.active_session.controls_visible => {
                AppEventResult::none()
            }
            TouchGesture::Press { .. } => AppEventResult::none(),
            TouchGesture::Click { start, end } => {
                if !self.active_session.controls_visible
                    && crate::watch_ui::active_session_controls_trigger_hit_pair(start, end)
                {
                    log::info!("new UI active session controls opened");
                    self.active_session.action_index = 0;
                    self.active_session.controls_visible = true;
                    return AppEventResult::render();
                }
                let had_overlay =
                    self.active_session.backspace_overlay || self.active_session.menu_overlay;
                match context.active_session_hits.hit_pair(start, end) {
                    Some(crate::watch_ui::ActiveSessionHit::Back) => {
                        log::info!("new UI active session back click");
                        self.enter_route(Route::SessionPicker);
                        self.active_session.backspace_overlay = false;
                        self.active_session.menu_overlay = false;
                        self.active_session.loading = false;
                        AppEventResult {
                            render: true,
                            effects: vec![
                                Effect::MqttPublish(MqttCommand::SendSync { close: true }),
                                Effect::ClearActiveSession,
                            ],
                        }
                    }
                    Some(crate::watch_ui::ActiveSessionHit::RunAction) => {
                        let action = self.active_session.action();
                        log::info!("new UI agent run action click: {action:?}");
                        self.active_session.backspace_overlay = false;
                        self.active_session.menu_overlay = false;
                        self.active_session.clear_overlay = had_overlay;
                        match action {
                            crate::watch_ui::AgentTuiAction::Speak => {
                                self.active_session.controls_visible = false;
                                self.open_asr_editor();
                                AppEventResult::render()
                            }
                            crate::watch_ui::AgentTuiAction::Accept => {
                                AppEventResult::effect(Effect::MqttPublish(MqttCommand::SendKey {
                                    key: "\r".to_string(),
                                }))
                            }
                            crate::watch_ui::AgentTuiAction::Next => {
                                AppEventResult::effect(Effect::MqttPublish(MqttCommand::SendKey {
                                    key: "\x1b[B".to_string(),
                                }))
                            }
                            crate::watch_ui::AgentTuiAction::Yolo => {
                                AppEventResult::effect(Effect::MqttPublish(MqttCommand::SendKey {
                                    key: "\x1b[Z".to_string(),
                                }))
                            }
                            crate::watch_ui::AgentTuiAction::Del => {
                                AppEventResult::effect(Effect::MqttPublish(MqttCommand::SendKey {
                                    key: "\x7f".to_string(),
                                }))
                            }
                            crate::watch_ui::AgentTuiAction::Esc => {
                                AppEventResult::effect(Effect::MqttPublish(MqttCommand::SendKey {
                                    key: "\x1b".to_string(),
                                }))
                            }
                        }
                    }
                    Some(crate::watch_ui::ActiveSessionHit::PrevAction) => {
                        let count = crate::watch_ui::AgentTuiAction::ALL.len();
                        self.active_session.action_index =
                            (self.active_session.action_index + count - 1) % count;
                        AppEventResult::render()
                    }
                    Some(crate::watch_ui::ActiveSessionHit::NextAction) => {
                        self.active_session.action_index = (self.active_session.action_index + 1)
                            % crate::watch_ui::AgentTuiAction::ALL.len();
                        AppEventResult::render()
                    }
                    None => {
                        if self.active_session.controls_visible {
                            log::info!("new UI active session controls dismissed");
                            self.active_session.controls_visible = false;
                            self.active_session.clear_overlay = true;
                            return AppEventResult::render();
                        }
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
            }
            TouchGesture::LongPress { start, end, .. } => {
                if context.active_session_hits.hit_pair(start, end)
                    == Some(crate::watch_ui::ActiveSessionHit::RunAction)
                    && self.active_session.action() == crate::watch_ui::AgentTuiAction::Speak
                {
                    self.active_session.controls_visible = false;
                    self.open_asr_editor();
                    AppEventResult::render()
                } else {
                    AppEventResult::none()
                }
            }
            TouchGesture::SwipePreview {
                start,
                current,
                direction,
                dy,
                ..
            } => {
                let had_overlay = self.active_session.backspace_overlay
                    || self.active_session.menu_overlay
                    || self.active_session.controls_visible;
                self.active_session.backspace_overlay = false;
                self.active_session.menu_overlay = false;
                self.active_session.controls_visible = false;
                self.active_session.clear_overlay = had_overlay;
                self.active_session.backspace_hold_sent = false;
                self.active_session.last_backspace_sent_at = None;

                let next_offset = context
                    .active_session_hits
                    .swipe_preview_offset(start, current, direction, dy);
                if next_offset == self.active_session.scroll_preview_y_offset && !had_overlay {
                    return AppEventResult::none();
                }
                self.active_session.scroll_preview_y_offset = next_offset;
                self.active_session.scroll_preview_redraw = true;
                AppEventResult::render()
            }
            TouchGesture::SwipeCancel { .. } => {
                let had_overlay = self.active_session.backspace_overlay
                    || self.active_session.menu_overlay
                    || self.active_session.controls_visible;
                let had_preview = self.active_session.scroll_preview_y_offset != 0;
                self.active_session.backspace_overlay = false;
                self.active_session.menu_overlay = false;
                self.active_session.controls_visible = false;
                self.active_session.clear_overlay = had_overlay;
                self.active_session.backspace_hold_sent = false;
                self.active_session.last_backspace_sent_at = None;
                self.active_session.scroll_preview_y_offset = 0;
                self.active_session.scroll_preview_redraw = had_preview;
                if had_overlay || had_preview {
                    AppEventResult::render()
                } else {
                    AppEventResult::none()
                }
            }
            TouchGesture::Swipe { start, end, .. } => {
                let had_overlay = self.active_session.backspace_overlay
                    || self.active_session.menu_overlay
                    || self.active_session.controls_visible;
                let had_preview = self.active_session.scroll_preview_y_offset != 0;
                self.active_session.scroll_preview_y_offset = 0;
                self.active_session.scroll_preview_redraw = had_preview;
                self.active_session.backspace_overlay = false;
                self.active_session.menu_overlay = false;
                self.active_session.controls_visible = false;
                self.active_session.clear_overlay = had_overlay;
                self.active_session.backspace_hold_sent = false;
                self.active_session.last_backspace_sent_at = None;
                match context.active_session_hits.swipe(start, end) {
                    Some(crate::watch_ui::ActiveSessionSwipe::Back) => {
                        log::info!("new UI right swipe detected, returning to session list");
                        self.enter_route(Route::SessionPicker);
                        self.active_session.loading = false;
                        AppEventResult {
                            render: true,
                            effects: vec![
                                Effect::MqttPublish(MqttCommand::SendSync { close: true }),
                                Effect::ClearActiveSession,
                            ],
                        }
                    }
                    Some(crate::watch_ui::ActiveSessionSwipe::ScrollUp) => {
                        let msg = MqttCommand::SendScrollUp { rows: 15 };
                        if had_overlay || had_preview {
                            AppEventResult::render_with_effect(Effect::MqttPublish(msg))
                        } else {
                            AppEventResult::effect(Effect::MqttPublish(msg))
                        }
                    }
                    Some(crate::watch_ui::ActiveSessionSwipe::ScrollDown) => {
                        let msg = MqttCommand::SendScrollDown { rows: 15 };
                        if had_overlay || had_preview {
                            AppEventResult::render_with_effect(Effect::MqttPublish(msg))
                        } else {
                            AppEventResult::effect(Effect::MqttPublish(msg))
                        }
                    }
                    None => {
                        if had_overlay {
                            AppEventResult::render()
                        } else {
                            AppEventResult::none()
                        }
                    }
                }
            }
        }
    }

    fn handle_asr_editor_touch(
        &mut self,
        gesture: TouchGesture,
        context: &AppEventContext<'_>,
    ) -> AppEventResult {
        match gesture {
            TouchGesture::Press { point } => match context.voice_input_hits.hit(point) {
                Some(crate::watch_ui::VoiceInputHit::Left) => {
                    self.asr_move_left();
                    AppEventResult::render()
                }
                Some(crate::watch_ui::VoiceInputHit::Right) => {
                    self.asr_move_right();
                    AppEventResult::render()
                }
                Some(crate::watch_ui::VoiceInputHit::Delete) => {
                    self.asr_backspace();
                    AppEventResult::render()
                }
                Some(crate::watch_ui::VoiceInputHit::Record) => {
                    self.set_asr_connecting();
                    AppEventResult::render_with_effect(Effect::StartAsrRecording)
                }
                _ => AppEventResult::none(),
            },
            TouchGesture::Click { start, end } => {
                let hit = context.voice_input_hits.hit(start);
                if hit != context.voice_input_hits.hit(end) {
                    return AppEventResult::none();
                }
                match hit {
                    Some(crate::watch_ui::VoiceInputHit::Back) => {
                        self.return_from_asr_editor();
                        AppEventResult::render_with_effect(Effect::CancelAsrEditor)
                    }
                    Some(crate::watch_ui::VoiceInputHit::Submit) => {
                        let text = self.asr_take_trimmed();
                        self.return_from_asr_editor();
                        AppEventResult::render_with_effect(Effect::SubmitAsrEditor(text))
                    }
                    _ => AppEventResult::none(),
                }
            }
            TouchGesture::LongPress { start, end, .. } => {
                let hit = context.voice_input_hits.hit(start);
                if hit != context.voice_input_hits.hit(end) {
                    return AppEventResult::none();
                }
                match hit {
                    Some(crate::watch_ui::VoiceInputHit::Left) => {
                        self.asr_move_left();
                        AppEventResult::render()
                    }
                    Some(crate::watch_ui::VoiceInputHit::Right) => {
                        self.asr_move_right();
                        AppEventResult::render()
                    }
                    Some(crate::watch_ui::VoiceInputHit::Delete) => {
                        self.asr_backspace();
                        AppEventResult::render()
                    }
                    _ => AppEventResult::none(),
                }
            }
            TouchGesture::Swipe { start, end, .. } => {
                if context.voice_input_hits.hit(start).is_some() {
                    return AppEventResult::none();
                }
                let dx = end.x as i32 - start.x as i32;
                let dy = end.y as i32 - start.y as i32;
                if dx >= crate::touch::DEFAULT_SWIPE_THRESHOLD_PX
                    && dy.abs() <= crate::touch::DEFAULT_SWIPE_THRESHOLD_PX
                {
                    self.return_from_asr_editor();
                    AppEventResult::render_with_effect(Effect::CancelAsrEditor)
                } else if dx.abs() <= crate::touch::DEFAULT_SWIPE_THRESHOLD_PX
                    && dy <= -crate::touch::DEFAULT_SWIPE_THRESHOLD_PX
                {
                    let text = self.asr_take_trimmed();
                    self.return_from_asr_editor();
                    AppEventResult::render_with_effect(Effect::SubmitAsrEditor(text))
                } else {
                    AppEventResult::none()
                }
            }
            TouchGesture::SwipePreview { .. } | TouchGesture::SwipeCancel { .. } => {
                AppEventResult::none()
            }
        }
    }

    fn handle_mqtt_event(&mut self, event: MqttEvent) -> AppEventResult {
        match event {
            MqttEvent::ActiveScreen(chunk) => {
                if self.sessions.reconnecting {
                    return AppEventResult::none();
                }
                if self.route == Route::AsrEditor {
                    return AppEventResult::none();
                }
                self.enter_route(Route::ActiveSession);
                self.active_session.loading = false;
                self.active_session.pending_screen_chunk = Some(chunk);
                AppEventResult::render()
            }
            MqttEvent::ActiveText(frame) => {
                if self.sessions.reconnecting {
                    return AppEventResult::none();
                }
                if self.route == Route::AsrEditor {
                    return AppEventResult::none();
                }
                self.enter_route(Route::ActiveSession);
                self.active_session.loading = false;
                self.active_session.pending_text_frame = Some(frame);
                AppEventResult::render()
            }
            MqttEvent::Connected => {
                log::info!("new UI MQTT connected; returning to session list");
                let render = self.exit_mqtt_reconnecting();
                AppEventResult {
                    render,
                    effects: vec![Effect::SetBacklight(BacklightState::Normal)],
                }
            }
            MqttEvent::Disconnected => {
                log::warn!("new UI MQTT disconnected; showing reconnecting state");
                AppEventResult {
                    render: self.enter_mqtt_reconnecting(),
                    effects: Vec::new(),
                }
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
                    self.enter_route(Route::SessionPicker);
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
                self.enter_route(Route::ActiveSession);
                self.active_session.loading = true;
                self.active_session.controls_visible = false;
                self.active_session.action_index = 0;
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
        gui.cancel_pending_terminal_append();
        if self.sessions.reconnecting {
            let dots = ".".repeat(self.sessions.reconnect_dots + 1);
            gui.show_status(format!("Reconnect MQTT{dots}"), "").await?;
            render_state.session_list_hits.clear();
            return Ok(());
        }
        if self.sessions.items.is_empty() {
            gui.show_status("no session", "").await?;
            render_state.session_list_hits.clear();
            return Ok(());
        }

        let visible_hint = render_state.session_list_hits.visible_count().max(1);
        self.sessions.scroll_offset = clamp_scroll_offset(
            self.sessions.scroll_offset,
            self.sessions.items.len(),
            visible_hint,
        );

        let visible_items = &self.sessions.items[self.sessions.scroll_offset..];
        render_state.session_list_hits = gui
            .display_session_list(&self.sessions.title, visible_items)
            .await?;
        Ok(())
    }

    async fn render_active_session(
        &mut self,
        gui: &mut UI,
        render_state: &mut AppRenderState,
    ) -> anyhow::Result<()> {
        if let Some(chunk) = self.active_session.pending_screen_chunk.take() {
            render_screen_chunk(gui, chunk).await?;
        }
        if let Some(frame) = self.active_session.pending_text_frame.take() {
            gui.set_terminal_render_y_offset(
                crate::ui::DEFAULT_TERMINAL_RENDER_Y_OFFSET
                    + self.active_session.scroll_preview_y_offset,
            );
            // log::info!("new UI screen text frame: {}B", frame.len());
            match screen_text_frame_kind(&frame) {
                Some(ScreenTextFrameKind::Full) => {
                    if self.active_session.controls_visible {
                        gui.prepare_terminal_text_frame(&frame).await?;
                    } else {
                        gui.show_terminal_text_frame(&frame).await?;
                    }
                }
                Some(ScreenTextFrameKind::Append) => {
                    if self.active_session.controls_visible {
                        gui.prepare_terminal_text_frame(&frame).await?;
                    } else {
                        gui.buffer_terminal_text_frame(&frame).await?;
                    }
                }
                None => {
                    if self.active_session.controls_visible {
                        gui.prepare_terminal_text_frame(&frame).await?;
                    } else {
                        gui.show_terminal_text_frame(&frame).await?;
                    }
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
        if self.active_session.scroll_preview_redraw {
            gui.set_terminal_render_y_offset(
                crate::ui::DEFAULT_TERMINAL_RENDER_Y_OFFSET
                    + self.active_session.scroll_preview_y_offset,
            );
            redraw_active_cached_screen(gui).await?;
            self.active_session.scroll_preview_redraw = false;
        }
        if self.active_session.backspace_overlay {
            gui.show_session_backspace_overlay().await?;
        }
        if self.active_session.menu_overlay {
            gui.show_session_menu_overlay().await?;
        }
        if self.active_session.controls_visible {
            render_state.active_session_hits = gui
                .show_active_session_controls(self.active_session.action())
                .await?;
        } else {
            render_state.active_session_hits = crate::watch_ui::ActiveSessionHitRegions::default();
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
    StartAsrRecording,
    SubmitAsrEditor(String),
    CancelAsrEditor,
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
