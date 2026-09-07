//! Remote 模式:MQTT 连 vibetty broker。启动先进入 session list(触摸选会话),
//! 选定后订阅该会话的整屏 JPEG、解码刷到 LCD。
//!
//! picker 支持触摸选择会话:点列表行后设置 active session 并发送 sync。

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use embedded_graphics::{prelude::*, primitives::Rectangle};

use crate::{
    app, audio, boot::BootButton, lcd, mqtt::MqttEvent, mqtt::MqttServer, protocol, touch, ui::UI,
};

const BACKLIGHT_NORMAL: u8 = 50;
const SESSION_LIST_IDLE_OFF_DELAY: std::time::Duration = std::time::Duration::from_secs(30);
const TOUCH_SWIPE_THRESHOLD_PX: i32 = 40;
const SESSION_LIST_OFF_SHUTDOWN_PROMPT_DELAY: std::time::Duration =
    std::time::Duration::from_secs(20 * 60);
const IDLE_SHUTDOWN_COUNTDOWN_SECS: u64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BacklightMode {
    Normal,
    Off,
}

impl BacklightMode {
    fn set(&mut self, mode: Self) -> anyhow::Result<()> {
        if *self == mode {
            return Ok(());
        }
        if mode == Self::Normal {
            crate::power::hold_light_sleep_lock()?;
            if let Err(e) = crate::audio::init() {
                log::warn!("Failed to reopen audio after screen on: {e:?}");
            }
        }
        crate::lcd::set_display_on(mode == Self::Normal)?;
        if mode == Self::Normal {
            crate::lcd::set_backlight(BACKLIGHT_NORMAL)?;
        }
        if mode == Self::Off {
            if let Err(e) = crate::audio::close() {
                log::warn!("Failed to close audio before light sleep: {e:?}");
            }
            crate::power::release_light_sleep_lock()?;
        }
        *self = mode;
        Ok(())
    }
}

pub async fn run(
    uri: String,
    client_id: String,
    gui: &mut UI,
    touch: &mut touch::TouchInput,
    boot_button: &mut BootButton,
    asr_tx: std::sync::mpsc::Sender<audio::AsrRequest>,
    asr_config: Option<&audio::AsrConfig>,
    audio_prompt: Option<&audio::PromptPlayer>,
    mut audio_prompt_enabled: bool,
    nvs: &esp_idf_svc::nvs::EspDefaultNvs,
) -> anyhow::Result<crate::ui::SettingMenuSelection> {
    log::info!("Connecting to MQTT broker {uri} as {client_id} with new UI loop");
    let mut server = match MqttServer::new(&uri, &client_id).await {
        Ok(s) => s,
        Err(e) => {
            log::error!("MQTT connect failed: {e:?}");
            let _ = gui.show_status("MQTT failed", format!("{e:?}")).await;
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            return Err(e);
        }
    };
    log::info!("MQTT connected, entering new UI session list");

    let mut state = app::AppState::session_picker();
    let mut render_requested = true;

    let mut backlight = BacklightMode::Normal;
    let mut render_state = app::AppRenderState::new();
    let mut last_session_list_change = tokio::time::Instant::now();
    let mut session_list_off_since = None;

    loop {
        if let Err(e) = server.flush_pending().await {
            recover_mqtt_error_(
                "subscribe screen",
                e,
                &mut state,
                &mut server,
                gui,
                &mut render_state,
            )
            .await?;
            render_requested = false;
            session_list_off_since = None;
            last_session_list_change = tokio::time::Instant::now();
            continue;
        }
        if server.is_connected() {
            if state.exit_mqtt_reconnecting() {
                execute_simple_effect_(
                    app::Effect::SetBacklight(app::BacklightState::Normal),
                    &mut server,
                    &mut backlight,
                )
                .await?;
                render_requested = true;
                session_list_off_since = None;
                last_session_list_change = tokio::time::Instant::now();
            }
        } else if state.enter_mqtt_reconnecting() {
            render_requested = true;
            session_list_off_since = None;
            last_session_list_change = tokio::time::Instant::now();
        }
        if render_requested {
            state.render(gui, &mut render_state).await?;
            render_requested = false;
        }

        let session_shutdown_at = session_list_off_since
            .map(|instant| instant + SESSION_LIST_OFF_SHUTDOWN_PROMPT_DELAY)
            .unwrap_or_else(|| tokio::time::Instant::now() + std::time::Duration::from_secs(3600));
        let terminal_append_render_at = if state.route == app::Route::ActiveSession {
            gui.terminal_append_render_deadline()
        } else {
            None
        };

        tokio::select! {
            // 固定窗口合并 terminal append，到点渲染一次。
            _ = async {
                match terminal_append_render_at {
                    Some(when) => tokio::time::sleep_until(when).await,
                    None => std::future::pending::<()>().await,
                }
            }, if terminal_append_render_at.is_some() => {
                if let Err(e) = gui.render_pending_terminal_append().await {
                    log::warn!("render pending terminal append failed: {e:?}");
                }
            }
            // MQTT 断线重连提示的点号动画。
            _ = tokio::time::sleep_until(render_state.next_mqtt_reconnect_refresh), if state.is_mqtt_reconnecting() && backlight != BacklightMode::Off => {
                render_state.next_mqtt_reconnect_refresh =
                    tokio::time::Instant::now() + std::time::Duration::from_secs(1);
                render_requested = state.tick_mqtt_reconnecting();
            }
            // 时钟页面每秒刷新一次时间。
            _ = tokio::time::sleep_until(render_state.next_clock_tick), if state.route == app::Route::Clock && backlight != BacklightMode::Off => {
                render_requested = true;
            }
            // 定时刷新 session list 标题里的电量。
            _ = tokio::time::sleep_until(render_state.next_title_refresh), if state.route == app::Route::SessionPicker && backlight != BacklightMode::Off => {
                render_state.next_title_refresh =
                    tokio::time::Instant::now() + crate::ui::MENU_TITLE_REFRESH_DELAY;
                render_requested = state.set_session_title(session_picker_title());
            }
            // home/main/session list 长时间无交互时自动熄屏。
            _ = tokio::time::sleep_until(last_session_list_change + SESSION_LIST_IDLE_OFF_DELAY), if matches!(state.route, app::Route::Clock | app::Route::MainMenu | app::Route::SessionPicker) && backlight != BacklightMode::Off => {
                log::info!("Home/menu/session list idle for 30s, turning screen off");
                execute_simple_effect_(
                    app::Effect::SetBacklight(app::BacklightState::Off),
                    &mut server,
                    &mut backlight,
                )
                .await?;
                session_list_off_since = Some(tokio::time::Instant::now());
            }
            // session list 熄屏一段时间后提示自动关机。
            _ = tokio::time::sleep_until(session_shutdown_at), if state.route == app::Route::SessionPicker && backlight == BacklightMode::Off && session_list_off_since.is_some() && state.all_sessions_idle() => {
                log::info!("Session list screen off for 20min with no working sessions; prompting shutdown");
                execute_simple_effect_(
                    app::Effect::SetBacklight(app::BacklightState::Normal),
                    &mut server,
                    &mut backlight,
                )
                .await?;
                session_list_off_since = None;
                last_session_list_change = tokio::time::Instant::now();
                if show_idle_shutdown_prompt(&mut server, gui, touch).await? {
                    render_requested = state.request_render_for_current_route();
                } else {
                    log::warn!("Idle shutdown countdown expired, shutting down");
                    execute_simple_effect_(app::Effect::PowerOff, &mut server, &mut backlight).await?;
                }
            }
            // 物理 BOOT 键在 home/main/session list 中用于熄屏。
            _ = crate::boot::wait_boot_press(boot_button), if matches!(state.route, app::Route::Clock | app::Route::MainMenu | app::Route::SessionPicker) => {
                log::info!("BOOT button pressed from new UI idle route, turning screen off");
                execute_simple_effect_(
                    app::Effect::SetBacklight(app::BacklightState::Off),
                    &mut server,
                    &mut backlight,
                )
                .await?;
                session_list_off_since = Some(tokio::time::Instant::now());
            }
            // 触摸手势进入 AppState，由 state 决定渲染和 effect。
            gesture = touch.next_gesture() => {
                let Some(gesture) = gesture else {
                    return Err(anyhow::anyhow!("touch event source closed"));
                };
                if backlight == BacklightMode::Off {
                    log::info!("Touch while screen is off, restoring backlight");
                    execute_simple_effect_(
                        app::Effect::SetBacklight(app::BacklightState::Normal),
                        &mut server,
                        &mut backlight,
                    )
                    .await?;
                    session_list_off_since = None;
                    last_session_list_change = tokio::time::Instant::now();
                    render_requested = state.wake_screen();
                    continue;
                }

                let result = state.handle_event(
                    app::AppEvent::Touch(gesture),
                    &app::AppEventContext {
                        main_menu_hits: &render_state.main_menu_hits,
                        session_list_hits: &render_state.session_list_hits,
                        active_session_hits: &render_state.active_session_hits,
                        voice_input_hits: &render_state.voice_input_hits,
                    },
                );
                let render_after_effect = handle_app_event_result_(
                    result,
                    &mut state,
                    &mut server,
                    gui,
                    &mut render_state,
                    touch,
                    &mut backlight,
                    &asr_tx,
                    asr_config,
                    audio_prompt,
                    &mut audio_prompt_enabled,
                    nvs,
                )
                .await?;
                if render_after_effect {
                    render_requested = true;
                }
                if state.route == app::Route::Settings {
                    touch.cancel_active_gesture();
                    return crate::ui::setting_menu(gui, touch).await;
                }
                if matches!(state.route, app::Route::Clock | app::Route::MainMenu | app::Route::SessionPicker) {
                    last_session_list_change = tokio::time::Instant::now();
                    session_list_off_since = None;
                }
            }
            // MQTT 事件更新 AppState，screen frame 也从这里进入渲染。
            ev = server.recv() => {
                let Some(ev) = ev else {
                    return Err(anyhow::anyhow!("MQTT event source closed"));
                };
                if matches!(ev, MqttEvent::ActiveScreen(_) | MqttEvent::ActiveText(_))
                    && !server.has_active_session()
                {
                    log::warn!("Ignoring stale screen frame after active session cleared");
                    continue;
                }
                let session_sync = if matches!(&ev, MqttEvent::Presence { .. }) {
                    Some(sync_sessions_(&mut state, &server))
                } else {
                    None
                };
                if let Some(sync) = session_sync.as_ref() {
                    render_requested |= sync.render;
                }
                let was_session_picker = state.route == app::Route::SessionPicker;
                let was_backlight_off = backlight == BacklightMode::Off;
                let result = state.handle_event(
                    app::AppEvent::Mqtt(ev),
                    &app::AppEventContext {
                        main_menu_hits: &render_state.main_menu_hits,
                        session_list_hits: &render_state.session_list_hits,
                        active_session_hits: &render_state.active_session_hits,
                        voice_input_hits: &render_state.voice_input_hits,
                    },
                );
                let should_render = result.render;
                let render_after_effect = handle_app_event_result_(
                    result,
                    &mut state,
                    &mut server,
                    gui,
                    &mut render_state,
                    touch,
                    &mut backlight,
                    &asr_tx,
                    asr_config,
                    audio_prompt,
                    &mut audio_prompt_enabled,
                    nvs,
                )
                .await?;
                if render_after_effect {
                    render_requested = true;
                }
                if let Some(sync) = session_sync {
                    if sync.session_activity {
                        last_session_list_change = tokio::time::Instant::now();
                        session_list_off_since = None;
                    }
                    if sync.play_prompt && audio_prompt_enabled {
                        if let Some(prompt) = audio_prompt {
                            prompt.play_async();
                        }
                    }
                }
                if should_render {
                    render_requested = true;
                }
                if state.route == app::Route::SessionPicker
                    && (!was_session_picker
                        || (was_backlight_off && backlight != BacklightMode::Off))
                {
                    last_session_list_change = tokio::time::Instant::now();
                    session_list_off_since = None;
                }
            }
        }
    }
}

async fn handle_app_event_result_(
    result: app::AppEventResult,
    state: &mut app::AppState,
    server: &mut MqttServer,
    gui: &mut UI,
    render_state: &mut app::AppRenderState,
    touch: &mut touch::TouchInput,
    backlight: &mut BacklightMode,
    asr_tx: &std::sync::mpsc::Sender<audio::AsrRequest>,
    asr_config: Option<&audio::AsrConfig>,
    audio_prompt: Option<&audio::PromptPlayer>,
    audio_prompt_enabled: &mut bool,
    nvs: &esp_idf_svc::nvs::EspDefaultNvs,
) -> anyhow::Result<bool> {
    let mut render_again = false;
    if result.render {
        state.render(gui, render_state).await?;
    }

    for effect in result.effects {
        if execute_app_effect_(
            effect,
            state,
            server,
            gui,
            render_state,
            touch,
            backlight,
            asr_tx,
            asr_config,
            audio_prompt,
            audio_prompt_enabled,
            nvs,
        )
        .await?
        {
            render_again = true;
        }
    }

    Ok(render_again)
}

#[allow(clippy::too_many_arguments)]
async fn execute_app_effect_(
    effect: app::Effect,
    state: &mut app::AppState,
    server: &mut MqttServer,
    gui: &mut UI,
    render_state: &mut app::AppRenderState,
    touch: &mut touch::TouchInput,
    backlight: &mut BacklightMode,
    asr_tx: &std::sync::mpsc::Sender<audio::AsrRequest>,
    asr_config: Option<&audio::AsrConfig>,
    audio_prompt: Option<&audio::PromptPlayer>,
    audio_prompt_enabled: &mut bool,
    nvs: &esp_idf_svc::nvs::EspDefaultNvs,
) -> anyhow::Result<bool> {
    let mut render_after_effect = false;
    match effect {
        app::Effect::SelectSession(prefix) => {
            server.set_active(&prefix);
            if let Err(e) = server.flush_pending().await {
                recover_mqtt_error_("subscribe screen", e, state, server, gui, render_state)
                    .await?;
            }
        }
        app::Effect::ClearActiveSession => {
            gui.cancel_pending_terminal_append();
            server.clear_active();
            if let Err(e) = server.flush_pending().await {
                recover_mqtt_error_("unsubscribe screen", e, state, server, gui, render_state)
                    .await?;
            }
        }
        app::Effect::OpenBootMenu => {
            log::info!("new UI opening BOOT menu");
            touch.cancel_active_gesture();
            match show_boot_menu(server, gui, touch, *audio_prompt_enabled).await? {
                BootMenuAction::Restart => {
                    execute_simple_effect_(app::Effect::Reboot, server, backlight).await?
                }
                BootMenuAction::PowerOff => {
                    execute_simple_effect_(app::Effect::PowerOff, server, backlight).await?
                }
                BootMenuAction::ScreenOff => {
                    render_after_effect = true;
                    execute_simple_effect_(
                        app::Effect::SetBacklight(app::BacklightState::Off),
                        server,
                        backlight,
                    )
                    .await?;
                }
                BootMenuAction::ToggleSound => {
                    *audio_prompt_enabled = !*audio_prompt_enabled;
                    if let Err(e) = audio::save_prompt_enabled(nvs, *audio_prompt_enabled) {
                        log::error!("Failed to save sound setting: {e:?}");
                    }
                    if *audio_prompt_enabled {
                        if let Some(prompt) = audio_prompt {
                            prompt.play_async();
                        }
                    }
                    render_after_effect = true;
                }
                BootMenuAction::Theme => {
                    if let Some(theme) = select_terminal_theme(server, gui, touch).await? {
                        log::info!("Terminal theme selected: {theme}");
                    }
                    render_after_effect = true;
                }
                BootMenuAction::Back => {
                    render_after_effect = true;
                }
            }
        }
        app::Effect::OpenScreenMenu => {
            touch.cancel_active_gesture();
            show_screen_action_menu(server, gui, touch).await?;
        }
        app::Effect::StartAsrRecording => {
            state.set_asr_connecting();
            state.render(gui, render_state).await?;
            let display_text = state.asr_display_text();
            let result = record_asr_once(
                server,
                gui,
                state,
                render_state,
                touch,
                asr_tx,
                asr_config.cloned(),
                &display_text,
            )
            .await;
            state.apply_asr_result(result);
            render_after_effect = true;
        }
        app::Effect::SubmitAsrEditor(text) => {
            if !text.is_empty() {
                if let Err(e) = server.send(protocol::ClientMessage::Input(text)).await {
                    log::warn!("Ignoring ASR editor send after active session disappeared: {e:?}");
                    return Ok(true);
                }
            }
            redraw_active_cached_screen(server, gui).await?;
        }
        app::Effect::CancelAsrEditor => {
            redraw_active_cached_screen(server, gui).await?;
        }
        app::Effect::SelectTheme(index) => {
            let label = gui.set_terminal_theme(index);
            log::info!("Terminal theme selected by effect: {label}");
        }
        app::Effect::MqttPublish(command) => {
            if let Err(e) = execute_mqtt_command_(command, server).await {
                recover_mqtt_error_("send data", e, state, server, gui, render_state).await?;
            }
        }
        other => execute_simple_effect_(other, server, backlight).await?,
    }

    Ok(render_after_effect)
}

async fn recover_mqtt_error_(
    context: &str,
    error: anyhow::Error,
    state: &mut app::AppState,
    server: &mut MqttServer,
    gui: &mut UI,
    render_state: &mut app::AppRenderState,
) -> anyhow::Result<()> {
    log::warn!("Recovering from MQTT {context} error: {error:?}");
    gui.show_status("MQTT error", format!("{context}\n{error:?}"))
        .await
        .ok();
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    gui.cancel_pending_terminal_append();
    server.clear_active();
    if let Err(e) = server.flush_pending().await {
        log::warn!("Ignoring MQTT cleanup error after {context} failure: {e:?}");
    }

    if server.is_connected() {
        state.exit_mqtt_reconnecting();
        state.return_to_session_picker();
        let _ = sync_sessions_(state, server);
    } else {
        state.enter_mqtt_reconnecting();
    }
    state.render(gui, render_state).await?;
    Ok(())
}

fn sync_sessions_(state: &mut app::AppState, server: &MqttServer) -> app::SessionSyncResult {
    state.sync_sessions(session_picker_title(), server.session_labels())
}

async fn execute_simple_effect_(
    effect: app::Effect,
    server: &mut MqttServer,
    backlight: &mut BacklightMode,
) -> anyhow::Result<()> {
    match effect {
        app::Effect::MqttPublish(command) => execute_mqtt_command_(command, server).await?,
        app::Effect::MqttSubscribe(topic) => {
            log::warn!("new UI explicit subscribe effect not implemented yet: {topic}");
        }
        app::Effect::MqttUnsubscribe(topic) => {
            log::warn!("new UI explicit unsubscribe effect not implemented yet: {topic}");
        }
        app::Effect::SetBacklight(mode) => {
            let mode = match mode {
                app::BacklightState::Normal => BacklightMode::Normal,
                app::BacklightState::Off => BacklightMode::Off,
            };
            backlight.set(mode)?;
        }
        app::Effect::PlayAudio(app::AudioCue::Prompt) => {}
        app::Effect::SelectSession(prefix) => {
            server.set_active(&prefix);
            server.flush_pending().await?;
        }
        app::Effect::ClearActiveSession => {
            // Returning to the picker must not flush buffered terminal output over the list.
            server.clear_active();
            server.flush_pending().await?;
        }
        app::Effect::OpenBootMenu
        | app::Effect::OpenScreenMenu
        | app::Effect::StartAsrRecording
        | app::Effect::SubmitAsrEditor(_)
        | app::Effect::CancelAsrEditor
        | app::Effect::SelectTheme(_) => {
            log::warn!("new UI effect requires UI context and was ignored: {effect:?}");
        }
        app::Effect::PowerOff => {
            crate::power::shutdown();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
        app::Effect::Reboot => esp_idf_svc::hal::reset::restart(),
    }
    Ok(())
}

async fn execute_mqtt_command_(
    command: app::MqttCommand,
    server: &mut MqttServer,
) -> anyhow::Result<()> {
    match command {
        app::MqttCommand::SendSync { close } => {
            send_active_sync(server, close).await?;
        }
        app::MqttCommand::SendKey { key } => {
            server
                .send(protocol::ClientMessage::pty_input_str(&key))
                .await?;
        }
        app::MqttCommand::SendScrollUp { rows } => {
            server
                .send(protocol::ClientMessage::ScrollUp { rows })
                .await?;
        }
        app::MqttCommand::SendScrollDown { rows } => {
            server
                .send(protocol::ClientMessage::ScrollDown { rows })
                .await?;
        }
        app::MqttCommand::Publish { topic, .. } => {
            log::warn!("new UI raw publish effect not implemented yet: {topic}");
        }
    }
    Ok(())
}

async fn send_active_sync(server: &mut MqttServer, close: bool) -> anyhow::Result<()> {
    if !server.has_active_session() {
        log::warn!("Skipping active sync: no active session");
        return Ok(());
    }

    let msg = if server.active_uses_text_screen() {
        let (cols, rows) = crate::ui::terminal_text_cells();
        log::info!("Sending text-mode sync: cols={cols} rows={rows} close={close}");
        protocol::ClientMessage::sync_cells(cols, rows, close)
    } else {
        protocol::ClientMessage::sync_with_close(close)
    };
    server.send(msg).await
}

enum ScreenAction {
    Esc,
    Next,
    Yolo,
    Enter,
}

enum BootMenuAction {
    Restart,
    PowerOff,
    ScreenOff,
    ToggleSound,
    Theme,
    Back,
}

async fn show_boot_menu(
    server: &mut MqttServer,
    gui: &mut UI,
    touch: &mut touch::TouchInput,
    audio_prompt_enabled: bool,
) -> anyhow::Result<BootMenuAction> {
    let sound_label = if audio_prompt_enabled {
        "Sound Off"
    } else {
        "Sound On"
    };
    let theme_label = format!("Theme: {}", crate::ui::terminal_theme_label());
    gui.display_list("System", &[]).await?;
    touch.wait_release().await;
    loop {
        let items = vec![
            boot_menu_item(0, &power_menu_label(), crate::ui::UiColor::CSS_DARK_ORANGE),
            boot_menu_item(1, sound_label, crate::ui::UiColor::CSS_GREEN),
            boot_menu_item(2, &theme_label, crate::ui::UiColor::CSS_STEEL_BLUE),
            boot_menu_item(3, "Back", crate::ui::UiColor::CSS_BLACK),
        ];
        let Some(index) = select_remote_list_item(server, gui, touch, "System", &items).await?
        else {
            return Ok(BootMenuAction::Back);
        };
        match index {
            0 => match show_power_menu(server, gui, touch).await? {
                BootMenuAction::Back => continue,
                action => return Ok(action),
            },
            1 => return Ok(BootMenuAction::ToggleSound),
            2 => return Ok(BootMenuAction::Theme),
            3 => return Ok(BootMenuAction::Back),
            _ => unreachable!(),
        }
    }
}

fn boot_menu_item(index: usize, text: &str, bg: crate::ui::UiColor) -> crate::ui::ListItem {
    crate::ui::ListItem::new(
        crate::ui::menu_item_rect(index).expect("boot menu item must fit"),
        text,
        Some(bg),
        Some(crate::ui::TEXT_LIGHT),
    )
}

fn power_menu_label() -> String {
    match crate::power::battery_percent() {
        Some(percent) => format!("Power: Battery {percent}%"),
        None => "Power: Battery --".to_string(),
    }
}

async fn show_power_menu(
    server: &mut MqttServer,
    gui: &mut UI,
    touch: &mut touch::TouchInput,
) -> anyhow::Result<BootMenuAction> {
    let items = vec![
        boot_menu_item(0, "Reboot", crate::ui::UiColor::CSS_DARK_ORANGE),
        boot_menu_item(1, "Power Off", crate::ui::UiColor::CSS_RED),
        boot_menu_item(2, "Screen Off", crate::ui::UiColor::CSS_GRAY),
        boot_menu_item(3, "Back", crate::ui::UiColor::CSS_BLACK),
    ];
    let Some(index) = select_remote_list_item(server, gui, touch, "Power", &items).await? else {
        return Ok(BootMenuAction::Back);
    };
    Ok(match index {
        0 => BootMenuAction::Restart,
        1 => BootMenuAction::PowerOff,
        2 => BootMenuAction::ScreenOff,
        3 => BootMenuAction::Back,
        _ => unreachable!(),
    })
}

async fn select_terminal_theme(
    server: &mut MqttServer,
    gui: &mut UI,
    touch: &mut touch::TouchInput,
) -> anyhow::Result<Option<&'static str>> {
    let mut item_rects = Vec::new();
    let mut scroll_offset = 0usize;
    render_terminal_theme_picker(gui, &mut item_rects, scroll_offset).await?;

    loop {
        tokio::select! {
            gesture = touch.next_gesture() => {
                match gesture {
                    Some(touch::TouchGesture::Swipe { start, end, direction, .. }) => {
                        if direction == touch::SwipeDirection::Right {
                            return Ok(None);
                        }
                        if let Some(delta) = list_scroll_delta(start, end) {
                            let visible_count = item_rects.len().max(1);
                            let total_items = crate::ui::terminal_theme_count() + 1;
                            let max_offset = total_items.saturating_sub(visible_count);
                            let next_offset = if delta > 0 {
                                scroll_offset.saturating_add(delta as usize).min(max_offset)
                            } else {
                                scroll_offset.saturating_sub((-delta) as usize)
                            };
                            if next_offset != scroll_offset {
                                scroll_offset = next_offset;
                                render_terminal_theme_picker(gui, &mut item_rects, scroll_offset).await?;
                            }
                        }
                    }
                    Some(touch::TouchGesture::Click { start, end }) => {
                        let press_index = crate::ui::list_touch_index(start, &item_rects);
                        let release_index = crate::ui::list_touch_index(end, &item_rects);
                        if press_index.is_some() && press_index == release_index {
                            let index = scroll_offset + press_index.unwrap();
                            if index >= crate::ui::terminal_theme_count() {
                                return Ok(None);
                            }
                            let label = gui.set_terminal_theme(index);
                            log::info!("Terminal theme switched to {label}");
                            return Ok(Some(label));
                        }
                    }
                    Some(_) => {}
                    None => return Ok(None),
                }
            }
            ev = server.recv() => {
                match ev {
                    Some(MqttEvent::Connected) | Some(MqttEvent::Disconnected) => {}
                    Some(MqttEvent::Presence { .. }) => {}
                    Some(MqttEvent::ActiveScreen(_)) | Some(MqttEvent::ActiveText(_)) => {}
                    None => return Ok(None),
                }
            }
        }
    }
}

async fn render_terminal_theme_picker(
    gui: &mut UI,
    item_rects: &mut Vec<embedded_graphics::primitives::Rectangle>,
    scroll_offset: usize,
) -> anyhow::Result<()> {
    let current = crate::ui::current_terminal_theme_index();
    let theme_count = crate::ui::terminal_theme_count();
    let items: Vec<(String, bool)> = (0..theme_count)
        .map(|index| {
            (
                crate::ui::terminal_theme_label_at(index).to_string(),
                index == current,
            )
        })
        .chain(std::iter::once(("Back".to_string(), false)))
        .skip(scroll_offset)
        .collect();
    *item_rects = gui.display_menu_list("Theme", &items).await?;
    Ok(())
}

async fn show_screen_action_menu(
    server: &mut MqttServer,
    gui: &mut UI,
    touch: &mut touch::TouchInput,
) -> anyhow::Result<()> {
    let items = vec![
        ("Esc".to_string(), true),
        ("Next".to_string(), true),
        ("Yolo".to_string(), true),
        ("Enter".to_string(), true),
    ];
    let Some(index) = select_screen_menu_item(server, gui, touch, "Menu", &items).await? else {
        redraw_active_cached_screen(server, gui).await?;
        return Ok(());
    };
    let action = match index {
        0 => ScreenAction::Esc,
        1 => ScreenAction::Next,
        2 => ScreenAction::Yolo,
        3 => ScreenAction::Enter,
        _ => return Ok(()),
    };
    let send_result = match action {
        ScreenAction::Esc => {
            server
                .send(protocol::ClientMessage::pty_input_str("\x1b"))
                .await
        }
        ScreenAction::Next => {
            server
                .send(protocol::ClientMessage::pty_input_str("\x1b[B"))
                .await
        }
        ScreenAction::Yolo => {
            server
                .send(protocol::ClientMessage::pty_input_str("\x1b[Z"))
                .await
        }
        ScreenAction::Enter => {
            server
                .send(protocol::ClientMessage::pty_input_str("\r"))
                .await
        }
    };
    if let Err(e) = send_result {
        log::warn!("Ignoring screen menu action after active session disappeared: {e:?}");
        return Ok(());
    }
    if server.active_uses_text_screen() {
        redraw_active_cached_screen(server, gui).await?;
    } else if let Err(e) = send_active_sync(server, false).await {
        log::warn!("Ignoring screen menu sync after active session disappeared: {e:?}");
    }
    Ok(())
}

async fn redraw_active_cached_screen(server: &MqttServer, gui: &mut UI) -> anyhow::Result<()> {
    if server.active_uses_text_screen() {
        if !gui.redraw_cached_terminal_text().await? {
            log::warn!("no cached terminal text screen to redraw");
        }
    } else if !gui.redraw_cached_jpeg_screen().await? {
        log::warn!("no cached JPEG screen to redraw");
    }
    Ok(())
}

async fn select_screen_menu_item(
    server: &mut MqttServer,
    gui: &mut UI,
    touch: &mut touch::TouchInput,
    title: &str,
    items: &[(String, bool)],
) -> anyhow::Result<Option<usize>> {
    let item_rects = gui.display_menu_list(title, items).await?;
    loop {
        tokio::select! {
            gesture = touch.next_gesture() => {
                match gesture {
                    Some(touch::TouchGesture::Click { start, end }) => {
                        let press_index = crate::ui::list_touch_index(start, &item_rects);
                        let release_index = crate::ui::list_touch_index(end, &item_rects);
                        if press_index.is_some() && press_index == release_index {
                            return Ok(press_index);
                        }
                        if press_index.is_none() && release_index.is_none() {
                            return Ok(None);
                        }
                    }
                    Some(touch::TouchGesture::Swipe { direction: touch::SwipeDirection::Right, .. }) => return Ok(None),
                    Some(_) => {}
                    None => return Err(anyhow::anyhow!("touch event source closed during screen menu")),
                }
            }
            ev = server.recv() => {
                match ev {
                    Some(MqttEvent::Connected) | Some(MqttEvent::Disconnected) => {}
                    Some(MqttEvent::Presence { .. }) => {}
                    Some(MqttEvent::ActiveScreen(_)) | Some(MqttEvent::ActiveText(_)) => {}
                    None => return Err(anyhow::anyhow!("MQTT event source closed during screen menu")),
                }
            }
        }
    }
}

async fn select_remote_list_item(
    server: &mut MqttServer,
    gui: &mut UI,
    touch: &mut touch::TouchInput,
    title: &str,
    items: &[crate::ui::ListItem],
) -> anyhow::Result<Option<usize>> {
    let item_rects = gui.display_list(title, items).await?;
    loop {
        tokio::select! {
            gesture = touch.next_gesture() => {
                match gesture {
                    Some(touch::TouchGesture::Swipe { direction: touch::SwipeDirection::Right, .. }) => {
                        log::info!("{title}: right swipe detected, returning");
                        return Ok(None);
                    }
                    Some(touch::TouchGesture::Click { start, end }) => {
                        let press_index = crate::ui::list_touch_index(start, &item_rects);
                        let release_index = crate::ui::list_touch_index(end, &item_rects);
                        if press_index.is_some() && press_index == release_index {
                            return Ok(press_index);
                        }
                    }
                    Some(_) => {}
                    None => return Err(anyhow::anyhow!("touch event source closed during custom menu")),
                }
            }
            ev = server.recv() => {
                match ev {
                    Some(MqttEvent::Connected) | Some(MqttEvent::Disconnected) => {}
                    Some(MqttEvent::Presence { .. }) => {}
                    Some(MqttEvent::ActiveScreen(_)) | Some(MqttEvent::ActiveText(_)) => {}
                    None => return Err(anyhow::anyhow!("MQTT event source closed during custom menu")),
                }
            }
        }
    }
}

async fn record_asr_once(
    server: &mut MqttServer,
    gui: &mut UI,
    state: &mut app::AppState,
    render_state: &mut app::AppRenderState,
    touch: &mut touch::TouchInput,
    asr_tx: &std::sync::mpsc::Sender<audio::AsrRequest>,
    asr_config: Option<audio::AsrConfig>,
    _display_text: &str,
) -> anyhow::Result<Option<String>> {
    let Some(config) = asr_config else {
        touch.wait_release().await;
        return Err(anyhow::anyhow!("ASR not configured"));
    };

    let (listening_tx, listening_rx) = tokio::sync::oneshot::channel();
    let (respond, result) = tokio::sync::oneshot::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let req = audio::AsrRequest {
        config,
        cancel: cancel.clone(),
        listening: listening_tx,
        respond,
    };

    state.set_asr_connecting();
    state.render(gui, render_state).await?;

    if asr_tx.send(req).is_err() {
        touch.wait_release().await;
        return Err(anyhow::anyhow!("ASR unavailable"));
    }

    let mut listening = std::pin::pin!(listening_rx);
    let mut result = std::pin::pin!(result);
    let mut is_listening = false;
    let asr_result = loop {
        tokio::select! {
            response = &mut listening, if !is_listening => {
                is_listening = true;
                if response.is_ok() {
                    state.set_asr_listening();
                    state.render(gui, render_state).await?;
                }
            }
            response = &mut result => {
                break response.unwrap_or_else(|_| Err(anyhow::anyhow!("ASR worker dropped request")));
            }
            gesture = touch.next_gesture() => {
                match gesture {
                    Some(touch::TouchGesture::Press { point }) if is_asr_record_touch(point) => {}
                    Some(touch::TouchGesture::LongPress { start, end, .. })
                        if is_asr_record_touch(start) && is_asr_record_touch(end) => {}
                    Some(_) | None => cancel.store(true, Ordering::Relaxed),
                }
            }
            ev = server.recv() => {
                if ev.is_none() {
                    break Err(anyhow::anyhow!("MQTT event source closed during ASR"));
                }
            }
        }
    };

    match asr_result {
        Ok(text) if !text.trim().is_empty() => Ok(Some(text.trim().to_string())),
        Ok(_) => Ok(None),
        Err(e) => Err(e),
    }
}

fn is_asr_record_touch(touch: lcd::TouchPoint) -> bool {
    crate::watch_ui::VoiceInputHitRegions::default().hit(touch)
        == Some(crate::watch_ui::VoiceInputHit::Record)
}

async fn show_idle_shutdown_prompt(
    server: &mut MqttServer,
    gui: &mut UI,
    touch: &mut touch::TouchInput,
) -> anyhow::Result<bool> {
    let cancel_rect = Rectangle::new(
        Point::new(40, lcd::LCD_HEIGHT as i32 - 132),
        Size::new((lcd::LCD_WIDTH - 80) as u32, 82),
    );
    let items = vec![crate::ui::ListItem::new(
        cancel_rect,
        "Cancel",
        Some(crate::ui::UiColor::CSS_DARK_ORANGE),
        Some(crate::ui::TEXT_LIGHT),
    )];

    let mut remaining = IDLE_SHUTDOWN_COUNTDOWN_SECS;
    let mut title = idle_shutdown_title(remaining);
    let item_rects = gui.display_list(&title, &items).await?;
    let mut next_tick = tokio::time::Instant::now() + std::time::Duration::from_secs(1);

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(next_tick) => {
                if remaining == 0 {
                    return Ok(false);
                }
                remaining -= 1;
                if remaining == 0 {
                    return Ok(false);
                }
                next_tick += std::time::Duration::from_secs(1);
                title = idle_shutdown_title(remaining);
                gui.refresh_list_title(&title).await?;
            }
            gesture = touch.next_gesture() => {
                match gesture {
                    Some(touch::TouchGesture::Click { start, end }) => {
                        let press_index = crate::ui::list_touch_index(start, &item_rects);
                        let release_index = crate::ui::list_touch_index(end, &item_rects);
                        if press_index.is_some() && press_index == release_index {
                            log::info!("Idle shutdown cancelled");
                            return Ok(true);
                        }
                    }
                    Some(_) => {}
                    None => return Ok(true),
                }
            }
            ev = server.recv() => {
                match ev {
                    Some(MqttEvent::Connected) | Some(MqttEvent::Disconnected) => return Ok(true),
                    Some(MqttEvent::Presence { list_changed, .. }) => {
                        if list_changed
                            && !server
                                .session_labels()
                                .iter()
                                .all(|(_, _, _, is_working)| !*is_working)
                        {
                            log::info!("Idle shutdown cancelled by working session");
                            return Ok(true);
                        }
                    }
                    Some(MqttEvent::ActiveScreen(_)) | Some(MqttEvent::ActiveText(_)) => {}
                    None => return Ok(true),
                }
            }
        }
    }
}

fn idle_shutdown_title(remaining: u64) -> String {
    format!("Save battery: off in {remaining}s")
}

fn session_picker_title() -> String {
    match crate::power::battery_percent() {
        Some(percent) => format!("Session: Battery {percent}%"),
        None => "Session: Battery --".to_string(),
    }
}

fn list_scroll_delta(start: lcd::TouchPoint, end: lcd::TouchPoint) -> Option<isize> {
    let dx = (end.x as i32 - start.x as i32).abs();
    let dy = end.y as i32 - start.y as i32;
    if dx > TOUCH_SWIPE_THRESHOLD_PX || dy.abs() < TOUCH_SWIPE_THRESHOLD_PX {
        return None;
    }

    if dy < 0 {
        Some(-1)
    } else {
        Some(1)
    }
}
