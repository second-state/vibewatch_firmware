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
    app, audio, boot::BootButton, lcd, mqtt::MqttEvent, mqtt::MqttServer, new_jpg, protocol, touch,
    ui::UI,
};

const BACKLIGHT_NORMAL: u8 = 50;
const SESSION_LIST_IDLE_OFF_DELAY: std::time::Duration = std::time::Duration::from_secs(30);
const SESSION_LIST_LONG_PRESS_MENU_DELAY: std::time::Duration = std::time::Duration::from_secs(1);
const TOUCH_SWIPE_THRESHOLD_PX: i32 = 40;
const SESSION_LIST_OFF_SHUTDOWN_PROMPT_DELAY: std::time::Duration =
    std::time::Duration::from_secs(20 * 60);
const IDLE_SHUTDOWN_COUNTDOWN_SECS: u64 = 15;
const SCREEN_BACKSPACE_REPEAT_DELAY: std::time::Duration = std::time::Duration::from_millis(500);
const SCREEN_SCROLL_SWIPE_ROWS: u16 = 15;

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
        let level = match mode {
            Self::Normal => BACKLIGHT_NORMAL,
            Self::Off => 0,
        };
        crate::lcd::set_backlight(level)?;
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
    mut touch_rx: tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
    mut boot_button: BootButton,
    asr_tx: std::sync::mpsc::Sender<audio::AsrRequest>,
    asr_config: Option<&audio::AsrConfig>,
    audio_prompt: Option<&audio::PromptPlayer>,
    mut audio_prompt_enabled: bool,
    nvs: &esp_idf_svc::nvs::EspDefaultNvs,
) -> anyhow::Result<()> {
    log::info!("Connecting to MQTT broker {uri} as {client_id}");
    let mut server = match MqttServer::new(&uri, &client_id).await {
        Ok(s) => s,
        Err(e) => {
            log::error!("MQTT connect failed: {e:?}");
            let _ = gui.show_status("MQTT failed", format!("{e:?}")).await;
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            return Err(e);
        }
    };
    log::info!("MQTT connected, entering session list");

    let mut backlight = BacklightMode::Normal;

    // session list:触摸选择会话。
    open_session_picker(
        &mut server,
        gui,
        &mut touch_rx,
        &mut boot_button,
        &mut backlight,
        audio_prompt,
        &mut audio_prompt_enabled,
        nvs,
    )
    .await?;

    let mut swipe_start = None;
    let mut backspace_touch_active = false;
    let mut backspace_repeat_sent = false;
    let mut next_backspace_at = None;
    let mut screen_menu_touch_active = false;
    // 选定会话后:主循环,解码并刷屏
    loop {
        // 把活跃会话落实为 `{prefix}/screen` 订阅(不可被取消)。
        server.flush_pending().await?;

        tokio::select! {
            _ = async {
                match next_backspace_at {
                    Some(when) => tokio::time::sleep_until(when).await,
                    None => std::future::pending::<()>().await,
                }
            }, if backspace_touch_active => {
                if let Err(e) = send_backspace_key(&mut server).await {
                    log::warn!("Ignoring backspace repeat after active session disappeared: {e:?}");
                    backspace_touch_active = false;
                    backspace_repeat_sent = false;
                    next_backspace_at = None;
                    screen_menu_touch_active = false;
                    swipe_start = None;
                    open_session_picker(
                        &mut server,
                        gui,
                        &mut touch_rx,
                        &mut boot_button,
                        &mut backlight,
                        audio_prompt,
                        &mut audio_prompt_enabled,
                        nvs,
                    )
                    .await?;
                    continue;
                }
                backspace_repeat_sent = true;
                next_backspace_at = Some(tokio::time::Instant::now() + SCREEN_BACKSPACE_REPEAT_DELAY);
            }
            event = touch_rx.recv() => {
                match event {
                    Some(lcd::TouchEvent::Press(touch)) => {
                        if backspace_touch_active || screen_menu_touch_active {
                            continue;
                        }
                        if is_screen_backspace_point(touch) {
                            log::info!("Screen backspace touch started");
                            backspace_touch_active = true;
                            backspace_repeat_sent = false;
                            next_backspace_at = Some(tokio::time::Instant::now() + SCREEN_BACKSPACE_REPEAT_DELAY);
                            swipe_start = None;
                            gui.show_session_backspace_overlay().await?;
                        } else if is_screen_menu_point(touch) {
                            log::info!("Screen menu touch started");
                            screen_menu_touch_active = true;
                            swipe_start = None;
                            gui.show_session_menu_overlay().await?;
                        } else if swipe_start.is_none() {
                            swipe_start = Some(touch);
                        }
                    }
                    Some(lcd::TouchEvent::Release(touch)) => {
                        if backspace_touch_active {
                            log::info!("Screen backspace touch released");
                            backspace_touch_active = false;
                            next_backspace_at = None;
                            if !backspace_repeat_sent {
                                if let Err(e) = send_backspace_key(&mut server).await {
                                    log::warn!("Ignoring backspace release after active session disappeared: {e:?}");
                                }
                            }
                            redraw_active_cached_screen(&server, gui).await?;
                            continue;
                        }
                        if screen_menu_touch_active {
                            log::info!("Screen menu touch released");
                            screen_menu_touch_active = false;
                            redraw_active_cached_screen(&server, gui).await?;
                            if is_screen_menu_point(touch) {
                                show_screen_action_menu(&mut server, gui, &mut touch_rx).await?;
                            }
                            continue;
                        }
                        if let Some(start) = swipe_start.take() {
                            let dx = touch.x as i32 - start.x as i32;
                            let dy = touch.y as i32 - start.y as i32;
                            log::info!("Swipe candidate: dx={} dy={}", dx, dy);
                            if is_asr_touch(start) && is_asr_touch(touch) {
                                run_touch_asr(&mut server, gui, &mut touch_rx, &asr_tx, asr_config).await?;
                            } else if is_screen_menu_touch(start, touch) {
                                show_screen_action_menu(&mut server, gui, &mut touch_rx).await?;
                            } else if is_back_swipe(start, touch) {
                                log::info!("Right swipe detected, returning to session list");
                                if let Err(e) =
                                    send_active_sync(&mut server, true).await
                                {
                                    log::warn!("Failed to close active screen stream: {e:?}");
                                }
                                server.clear_active();
                                server.flush_pending().await?;
                                open_session_picker(
                                    &mut server,
                                    gui,
                                    &mut touch_rx,
                                    &mut boot_button,
                                    &mut backlight,
                                    audio_prompt,
                                    &mut audio_prompt_enabled,
                                    nvs,
                                )
                                .await?;
                            } else if let Some(msg) = scroll_swipe_message(start, touch) {
                                if server.active_uses_text_screen() {
                                    match try_local_text_scroll(gui, &msg).await {
                                        Ok(true) => continue,
                                        Ok(false) => {}
                                        Err(e) => log::warn!("Local text scroll failed: {e:?}"),
                                    }
                                }
                                log::info!("Vertical swipe detected, sending {msg:?}");
                                if let Err(e) = server.send(msg).await {
                                    log::warn!("Ignoring vertical swipe send after active session disappeared: {e:?}");
                                    open_session_picker(
                                        &mut server,
                                        gui,
                                        &mut touch_rx,
                                        &mut boot_button,
                                        &mut backlight,
                                        audio_prompt,
                                        &mut audio_prompt_enabled,
                                        nvs,
                                    )
                                    .await?;
                                    continue;
                                }
                            }
                        }
                    }
                    None => {
                        log::warn!("Touch event source closed, exiting remote loop");
                        break;
                    }
                }
                continue;
            }
            ev = server.recv() => {
                let Some(ev) = ev else {
                    log::warn!("MQTT event source closed, exiting remote loop");
                    break;
                };
                let active_session_went_offline = matches!(
                    &ev,
                    MqttEvent::Presence {
                        online: false,
                        was_active: true,
                        ..
                    }
                );
                handle_mqtt_event(ev, gui, &mut backlight).await?;
                if active_session_went_offline {
                    log::warn!("Returning to session list after active session offline");
                    swipe_start = None;
                    backspace_touch_active = false;
                    backspace_repeat_sent = false;
                    next_backspace_at = None;
                    screen_menu_touch_active = false;
                    server.flush_pending().await?;
                    open_session_picker(
                        &mut server,
                        gui,
                        &mut touch_rx,
                        &mut boot_button,
                        &mut backlight,
                        audio_prompt,
                        &mut audio_prompt_enabled,
                        nvs,
                    )
                    .await?;
                    continue;
                }
                if backspace_touch_active {
                    gui.show_session_backspace_overlay().await?;
                } else if screen_menu_touch_active {
                    gui.show_session_menu_overlay().await?;
                }
            }
        }
    }

    Ok(())
}

pub async fn run_(
    uri: String,
    client_id: String,
    gui: &mut UI,
    touch_rx: tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
    mut boot_button: BootButton,
    asr_tx: std::sync::mpsc::Sender<audio::AsrRequest>,
    asr_config: Option<&audio::AsrConfig>,
    audio_prompt: Option<&audio::PromptPlayer>,
    mut audio_prompt_enabled: bool,
    nvs: &esp_idf_svc::nvs::EspDefaultNvs,
) -> anyhow::Result<()> {
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

    let mut touch = touch::TouchInput::new(touch_rx);
    let mut backlight = BacklightMode::Normal;
    let mut render_state = app::AppRenderState::new();
    let mut last_session_list_change = tokio::time::Instant::now();
    let mut session_list_off_since = None;

    loop {
        server.flush_pending().await?;
        if render_requested {
            state.render(gui, &mut render_state).await?;
            render_requested = false;
        }

        let session_shutdown_at = session_list_off_since
            .map(|instant| instant + SESSION_LIST_OFF_SHUTDOWN_PROMPT_DELAY)
            .unwrap_or_else(|| tokio::time::Instant::now() + std::time::Duration::from_secs(3600));
        let terminal_append_render_at = gui.terminal_append_render_deadline();

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
            // 定时刷新 session list 标题里的电量。
            _ = tokio::time::sleep_until(render_state.next_title_refresh), if state.route == app::Route::SessionPicker && backlight != BacklightMode::Off => {
                render_requested = state.set_session_title(session_picker_title());
            }
            // session list 长时间无变化时自动熄屏。
            _ = tokio::time::sleep_until(last_session_list_change + SESSION_LIST_IDLE_OFF_DELAY), if state.route == app::Route::SessionPicker && backlight != BacklightMode::Off => {
                log::info!("Session list unchanged for 30s, turning screen off");
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
                if show_idle_shutdown_prompt(&mut server, gui, touch.receiver_mut()).await? {
                    render_requested = state.request_render_for_current_route();
                } else {
                    log::warn!("Idle shutdown countdown expired, shutting down");
                    execute_simple_effect_(app::Effect::PowerOff, &mut server, &mut backlight).await?;
                }
            }
            // 物理 BOOT 键在 session list 中用于熄屏。
            _ = crate::boot::wait_boot_press(&mut boot_button), if state.route == app::Route::SessionPicker => {
                log::info!("BOOT button pressed from new UI session list, turning screen off");
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
                    log::warn!("Touch event source closed, exiting new UI loop");
                    break;
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
                        session_item_rects: &render_state.session_item_rects,
                    },
                );
                let should_render = result.render;
                let render_after_effect = handle_app_event_result_(
                    result,
                    &mut state,
                    &mut server,
                    gui,
                    &mut render_state,
                    &mut touch,
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
                if should_render {
                    last_session_list_change = tokio::time::Instant::now();
                }
            }
            // MQTT 事件更新 AppState，screen frame 也从这里进入渲染。
            ev = server.recv() => {
                let Some(ev) = ev else {
                    log::warn!("MQTT event source closed, exiting new UI loop");
                    break;
                };
                if matches!(ev, MqttEvent::ActiveScreen(_) | MqttEvent::ActiveText(_))
                    && !server.has_active_session()
                {
                    log::warn!("Ignoring stale screen frame after active session cleared");
                    continue;
                }
                let session_sync = if matches!(&ev, MqttEvent::Presence { list_changed: true, .. }) {
                    Some(sync_sessions_(&mut state, &server))
                } else {
                    None
                };
                if let Some(sync) = session_sync.as_ref() {
                    render_requested |= sync.render;
                }
                let result = state.handle_event(
                    app::AppEvent::Mqtt(ev),
                    &app::AppEventContext {
                        session_item_rects: &render_state.session_item_rects,
                    },
                );
                let should_render = result.render;
                let render_after_effect = handle_app_event_result_(
                    result,
                    &mut state,
                    &mut server,
                    gui,
                    &mut render_state,
                    &mut touch,
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
                    last_session_list_change = tokio::time::Instant::now();
                    session_list_off_since = None;
                    if sync.play_prompt && audio_prompt_enabled {
                        if let Some(prompt) = audio_prompt {
                            prompt.play_async();
                        }
                    }
                }
                if should_render {
                    render_requested = true;
                }
            }
        }
    }

    Ok(())
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
            server,
            gui,
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
    server: &mut MqttServer,
    gui: &mut UI,
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
            server.flush_pending().await?;
        }
        app::Effect::ClearActiveSession => {
            server.clear_active();
            server.flush_pending().await?;
        }
        app::Effect::OpenBootMenu => {
            log::info!("new UI opening BOOT menu");
            touch.cancel_active_gesture();
            match show_boot_menu(server, gui, touch.receiver_mut(), *audio_prompt_enabled).await? {
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
                    if let Some(theme) =
                        select_terminal_theme(server, gui, touch.receiver_mut()).await?
                    {
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
            show_screen_action_menu(server, gui, touch.receiver_mut()).await?;
        }
        app::Effect::OpenAsrEditor => {
            touch.cancel_active_gesture();
            run_touch_asr(server, gui, touch.receiver_mut(), asr_tx, asr_config).await?;
        }
        app::Effect::SelectTheme(index) => {
            let label = gui.set_terminal_theme(index);
            log::info!("Terminal theme selected by effect: {label}");
        }
        other => execute_simple_effect_(other, server, backlight).await?,
    }

    Ok(render_after_effect)
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
            server.clear_active();
            server.flush_pending().await?;
        }
        app::Effect::OpenBootMenu
        | app::Effect::OpenScreenMenu
        | app::Effect::OpenAsrEditor
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
            if let Err(e) = send_active_sync(server, close).await {
                log::warn!("Ignoring sync after active session disappeared: {e:?}");
            }
        }
        app::MqttCommand::SendKey { key } => {
            if let Err(e) = server
                .send(protocol::ClientMessage::pty_input_str(&key))
                .await
            {
                log::warn!("Ignoring key send after active session disappeared: {e:?}");
            }
        }
        app::MqttCommand::SendScrollUp { rows } => {
            if let Err(e) = server
                .send(protocol::ClientMessage::ScrollUp { rows })
                .await
            {
                log::warn!("Ignoring scroll-up after active session disappeared: {e:?}");
            }
        }
        app::MqttCommand::SendScrollDown { rows } => {
            if let Err(e) = server
                .send(protocol::ClientMessage::ScrollDown { rows })
                .await
            {
                log::warn!("Ignoring scroll-down after active session disappeared: {e:?}");
            }
        }
        app::MqttCommand::Publish { topic, .. } => {
            log::warn!("new UI raw publish effect not implemented yet: {topic}");
        }
    }
    Ok(())
}

async fn handle_mqtt_event(
    ev: MqttEvent,
    gui: &mut UI,
    backlight: &mut BacklightMode,
) -> anyhow::Result<()> {
    match ev {
        MqttEvent::ActiveScreen(chunk) => {
            if !matches!(chunk.format, protocol::ImageFormat::Jpeg) {
                log::warn!("Unsupported screen format {:?}, only JPEG", chunk.format);
                return Ok(());
            }
            // vibetty 在 JPEG 末尾附大端 u32 滚动标记(0=最底页);解码前剥掉尾部 4 字节。
            let data = &chunk.data;
            let jpeg: &[u8] = if data.len() >= 4 {
                &data[..data.len() - 4]
            } else {
                data.as_slice()
            };
            log::info!("Screen frame: {}B jpeg", jpeg.len());
            match new_jpg::esp_jpeg_decode_one_picture(jpeg) {
                Ok(display) => {
                    if let Err(e) = gui.show_jpeg_screen(display).await {
                        log::error!("flush screen failed: {e:?}");
                    }
                }
                Err(e) => log::error!("decode JPEG failed: {e:?}"),
            }
        }
        MqttEvent::ActiveText(frame) => {
            log::info!("Screen text frame: {}B", frame.len());
            if let Err(e) = gui.show_terminal_text_frame(&frame).await {
                log::error!("flush text screen failed: {e:?}");
            }
        }
        MqttEvent::Presence {
            prefix,
            online,
            list_changed,
            was_active,
        } => {
            log::info!(
                "Presence: {prefix} online={online} list_changed={list_changed} was_active={was_active}"
            );
            if !online && was_active {
                log::warn!(
                    "Active session offline; input sends will fail until a new session is selected"
                );
            }
            if list_changed {
                backlight.set(BacklightMode::Normal)?;
            }
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

async fn try_local_text_scroll(
    gui: &mut UI,
    msg: &protocol::ClientMessage,
) -> anyhow::Result<bool> {
    match msg {
        protocol::ClientMessage::ScrollUp { .. } => {
            gui.scroll_terminal_text(crate::ui::TerminalScroll::Up)
                .await
        }
        protocol::ClientMessage::ScrollDown { .. } => {
            gui.scroll_terminal_text(crate::ui::TerminalScroll::Down)
                .await
        }
        _ => Ok(false),
    }
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
    touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
    audio_prompt_enabled: bool,
) -> anyhow::Result<BootMenuAction> {
    let sound_label = if audio_prompt_enabled {
        "Sound Off"
    } else {
        "Sound On"
    };
    let theme_label = format!("Theme: {}", crate::ui::terminal_theme_label());
    gui.display_list("System", &[]).await?;
    wait_touch_release(touch_rx).await;
    loop {
        let items = vec![
            boot_menu_item(0, &power_menu_label(), crate::ui::UiColor::CSS_DARK_ORANGE),
            boot_menu_item(1, sound_label, crate::ui::UiColor::CSS_GREEN),
            boot_menu_item(2, &theme_label, crate::ui::UiColor::CSS_STEEL_BLUE),
            boot_menu_item(3, "Back", crate::ui::UiColor::CSS_BLACK),
        ];
        let Some(index) = select_remote_list_item(server, gui, touch_rx, "System", &items).await?
        else {
            return Ok(BootMenuAction::Back);
        };
        match index {
            0 => match show_power_menu(server, gui, touch_rx).await? {
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
    touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
) -> anyhow::Result<BootMenuAction> {
    let items = vec![
        boot_menu_item(0, "Reboot", crate::ui::UiColor::CSS_DARK_ORANGE),
        boot_menu_item(1, "Power Off", crate::ui::UiColor::CSS_RED),
        boot_menu_item(2, "Screen Off", crate::ui::UiColor::CSS_GRAY),
        boot_menu_item(3, "Back", crate::ui::UiColor::CSS_BLACK),
    ];
    let Some(index) = select_remote_list_item(server, gui, touch_rx, "Power", &items).await? else {
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
    touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
) -> anyhow::Result<Option<&'static str>> {
    let mut item_rects = Vec::new();
    let mut scroll_offset = 0usize;
    render_terminal_theme_picker(gui, &mut item_rects, scroll_offset).await?;

    let mut press_touch = None;
    loop {
        tokio::select! {
            event = touch_rx.recv() => {
                match event {
                    Some(lcd::TouchEvent::Press(touch)) => {
                        if press_touch.is_none() {
                            press_touch = Some(touch);
                        }
                    }
                    Some(lcd::TouchEvent::Release(touch)) => {
                        let Some(start) = press_touch.take() else {
                            continue;
                        };
                        if let Some(delta) = list_scroll_delta(start, touch) {
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
                            continue;
                        }

                        let press_index = crate::ui::list_touch_index(start, &item_rects);
                        let release_index = crate::ui::list_touch_index(touch, &item_rects);
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
                    None => return Ok(None),
                }
            }
            ev = server.recv() => {
                match ev {
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
    touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
) -> anyhow::Result<()> {
    let items = vec![
        ("Esc".to_string(), true),
        ("Next".to_string(), true),
        ("Yolo".to_string(), true),
        ("Enter".to_string(), true),
    ];
    let Some(index) = select_screen_menu_item(server, gui, touch_rx, "Menu", &items).await? else {
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

async fn send_backspace_key(server: &mut MqttServer) -> anyhow::Result<()> {
    server
        .send(protocol::ClientMessage::pty_input_str("\x7f"))
        .await
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
    touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
    title: &str,
    items: &[(String, bool)],
) -> anyhow::Result<Option<usize>> {
    let item_rects = gui.display_menu_list(title, items).await?;
    let mut press_index = None;
    loop {
        tokio::select! {
            event = touch_rx.recv() => {
                match event {
                    Some(lcd::TouchEvent::Press(touch)) => {
                        if press_index.is_none() {
                            press_index = crate::ui::list_touch_index(touch, &item_rects);
                        }
                    }
                    Some(lcd::TouchEvent::Release(touch)) => {
                        let release_index = crate::ui::list_touch_index(touch, &item_rects);
                        if press_index.is_some() && press_index == release_index {
                            return Ok(press_index);
                        }
                        if press_index.is_none() && release_index.is_none() {
                            return Ok(None);
                        }
                        press_index = None;
                    }
                    None => return Err(anyhow::anyhow!("touch event source closed during screen menu")),
                }
            }
            ev = server.recv() => {
                match ev {
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
    touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
    title: &str,
    items: &[crate::ui::ListItem],
) -> anyhow::Result<Option<usize>> {
    let item_rects = gui.display_list(title, items).await?;
    let mut press_touch = None;
    let mut press_index = None;
    loop {
        tokio::select! {
            event = touch_rx.recv() => {
                match event {
                    Some(lcd::TouchEvent::Press(touch)) => {
                        if press_touch.is_none() {
                            press_touch = Some(touch);
                        }
                        if press_index.is_none() {
                            press_index = crate::ui::list_touch_index(touch, &item_rects);
                        }
                    }
                    Some(lcd::TouchEvent::Release(touch)) => {
                        if let Some(start) = press_touch.take() {
                            if is_back_swipe(start, touch) {
                                log::info!("{title}: right swipe detected, returning");
                                return Ok(None);
                            }
                        }
                        let release_index = crate::ui::list_touch_index(touch, &item_rects);
                        if press_index.is_some() && press_index == release_index {
                            return Ok(press_index);
                        }
                        press_index = None;
                    }
                    None => return Err(anyhow::anyhow!("touch event source closed during custom menu")),
                }
            }
            ev = server.recv() => {
                match ev {
                    Some(MqttEvent::Presence { .. }) => {}
                    Some(MqttEvent::ActiveScreen(_)) | Some(MqttEvent::ActiveText(_)) => {}
                    None => return Err(anyhow::anyhow!("MQTT event source closed during custom menu")),
                }
            }
        }
    }
}

async fn run_touch_asr(
    server: &mut MqttServer,
    gui: &mut UI,
    touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
    asr_tx: &std::sync::mpsc::Sender<audio::AsrRequest>,
    asr_config: Option<&audio::AsrConfig>,
) -> anyhow::Result<()> {
    let mut editor = TouchAsrEditor::new();
    let mut hint = "Hold Record";
    let _ = gui.show_asr_editor(&editor.display_text(), hint).await;

    let mut press_touch = None;
    loop {
        tokio::select! {
            event = touch_rx.recv() => {
                match event {
                    Some(lcd::TouchEvent::Press(touch)) => {
                        if is_asr_touch(touch) {
                            hint = match record_asr_once(
                                server,
                                gui,
                                touch_rx,
                                asr_tx,
                                asr_config.cloned(),
                                &editor.display_text(),
                            )
                            .await
                            {
                                Ok(Some(text)) => {
                                    let text = text.trim();
                                    log::info!("Local ASR result: {text}");
                                    editor.insert_str(&format!("{text} "));
                                    "Hold Record"
                                }
                                Ok(None) => "(empty)",
                                Err(e) => {
                                    log::error!("ASR failed: {e:?}");
                                    "ASR error"
                                }
                            };
                            let _ = gui.show_asr_editor(&editor.display_text(), hint).await;
                        } else if let Some(action) = top_asr_action(touch) {
                            apply_top_asr_action(action, &mut editor);
                            let _ = gui.show_asr_editor(&editor.display_text(), hint).await;
                            wait_top_asr_action(
                                server,
                                gui,
                                touch_rx,
                                action,
                                &mut editor,
                                hint,
                            )
                            .await?;
                        } else if press_touch.is_none() {
                            press_touch = Some(touch);
                        }
                    }
                    Some(lcd::TouchEvent::Release(touch)) => {
                        let Some(start) = press_touch.take() else {
                            continue;
                        };
                        match asr_editor_swipe(start, touch) {
                            Some(AsrEditorSwipe::Send) => {
                                let text = editor.take_trimmed();
                                if !text.is_empty() {
                                    let text_mode = server.active_uses_text_screen();
                                    if let Err(e) =
                                        server.send(protocol::ClientMessage::Input(text)).await
                                    {
                                        log::warn!(
                                            "Ignoring ASR editor send after active session disappeared: {e:?}"
                                        );
                                        return Ok(());
                                    }
                                    if text_mode {
                                        if let Err(e) = gui.redraw_cached_terminal_text().await {
                                            log::warn!("redraw cached terminal text failed: {e:?}");
                                        }
                                        return Ok(());
                                    }
                                }
                                send_active_sync(server, false).await?;
                                return Ok(());
                            }
                            Some(AsrEditorSwipe::Cancel) => {
                                log::info!("ASR editor canceled");
                                send_active_sync(server, false).await?;
                                return Ok(());
                            }
                            None => {}
                        }
                    }
                    None => return Err(anyhow::anyhow!("touch event source closed during ASR editor")),
                }
            }
            ev = server.recv() => {
                if ev.is_none() {
                    return Err(anyhow::anyhow!("MQTT event source closed during ASR editor"));
                }
            }
        }
    }
}

async fn record_asr_once(
    server: &mut MqttServer,
    gui: &mut UI,
    touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
    asr_tx: &std::sync::mpsc::Sender<audio::AsrRequest>,
    asr_config: Option<audio::AsrConfig>,
    display_text: &str,
) -> anyhow::Result<Option<String>> {
    let Some(config) = asr_config else {
        wait_touch_release(touch_rx).await;
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

    let _ = gui.show_asr_editor(display_text, "Connecting...").await;

    if asr_tx.send(req).is_err() {
        wait_touch_release(touch_rx).await;
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
                    let _ = gui.show_asr_editor(display_text, "Listening...").await;
                }
            }
            response = &mut result => {
                break response.unwrap_or_else(|_| Err(anyhow::anyhow!("ASR worker dropped request")));
            }
            event = touch_rx.recv() => {
                match event {
                    Some(lcd::TouchEvent::Press(touch)) if is_asr_touch(touch) => {}
                    Some(lcd::TouchEvent::Release(_)) | Some(lcd::TouchEvent::Press(_)) | None => {
                        cancel.store(true, Ordering::Relaxed);
                    }
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

#[derive(Clone, Copy)]
enum TopAsrAction {
    Left,
    Delete,
    Right,
}

enum AsrEditorSwipe {
    Send,
    Cancel,
}

struct TouchAsrEditor {
    text: String,
    cursor: usize,
}

impl TouchAsrEditor {
    fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
        }
    }

    fn insert_str(&mut self, s: &str) {
        let byte_pos = self.cursor_byte_pos();
        self.text.insert_str(byte_pos, s);
        self.cursor += s.chars().count();
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let byte_pos = self
            .text
            .char_indices()
            .nth(self.cursor - 1)
            .map(|(i, _)| i)
            .unwrap_or(0);
        self.text.remove(byte_pos);
        self.cursor -= 1;
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        self.cursor = self.cursor.saturating_add(1).min(self.char_len());
    }

    fn display_text(&self) -> String {
        let mut out = String::with_capacity(self.text.len() + 1);
        for (i, ch) in self.text.chars().enumerate() {
            if i == self.cursor {
                out.push('|');
            }
            out.push(ch);
        }
        if self.cursor >= self.char_len() {
            out.push('|');
        }
        out
    }

    fn take_trimmed(mut self) -> String {
        self.text.truncate(self.text.trim_end().len());
        self.text.trim_start().to_string()
    }

    fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    fn cursor_byte_pos(&self) -> usize {
        self.text
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len())
    }
}

async fn wait_top_asr_action(
    server: &mut MqttServer,
    gui: &mut UI,
    touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
    action: TopAsrAction,
    editor: &mut TouchAsrEditor,
    hint: &str,
) -> anyhow::Result<()> {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(180));
    interval.tick().await;
    loop {
        tokio::select! {
            _ = interval.tick() => {
                apply_top_asr_action(action, editor);
                let _ = gui.show_asr_editor(&editor.display_text(), hint).await;
            }
            event = touch_rx.recv() => {
                match event {
                    Some(lcd::TouchEvent::Release(_)) | None => return Ok(()),
                    Some(lcd::TouchEvent::Press(_)) => {}
                }
            }
            ev = server.recv() => {
                if ev.is_none() {
                    return Err(anyhow::anyhow!("MQTT event source closed during ASR top action"));
                }
            }
        }
    }
}

fn apply_top_asr_action(action: TopAsrAction, editor: &mut TouchAsrEditor) {
    match action {
        TopAsrAction::Left => editor.move_left(),
        TopAsrAction::Delete => editor.backspace(),
        TopAsrAction::Right => editor.move_right(),
    }
}

fn top_asr_action(touch: lcd::TouchPoint) -> Option<TopAsrAction> {
    if touch.y >= 80 {
        return None;
    }
    let third = lcd::LCD_WIDTH / 3;
    if touch.x < third {
        Some(TopAsrAction::Left)
    } else if touch.x < third * 2 {
        Some(TopAsrAction::Delete)
    } else {
        Some(TopAsrAction::Right)
    }
}

fn asr_editor_swipe(start: lcd::TouchPoint, end: lcd::TouchPoint) -> Option<AsrEditorSwipe> {
    if is_asr_touch(start) || top_asr_action(start).is_some() {
        return None;
    }

    let dx = (end.x as i32 - start.x as i32).abs();
    let dy = end.y as i32 - start.y as i32;
    if dx > TOUCH_SWIPE_THRESHOLD_PX || dy.abs() < TOUCH_SWIPE_THRESHOLD_PX {
        return None;
    }
    if dy < 0 {
        Some(AsrEditorSwipe::Send)
    } else {
        Some(AsrEditorSwipe::Cancel)
    }
}

fn is_asr_touch(touch: lcd::TouchPoint) -> bool {
    touch.y > lcd::LCD_HEIGHT.saturating_sub(80)
}

fn is_back_swipe(start: lcd::TouchPoint, end: lcd::TouchPoint) -> bool {
    let dx = end.x as i32 - start.x as i32;
    let dy = (end.y as i32 - start.y as i32).abs();
    dx >= TOUCH_SWIPE_THRESHOLD_PX && dy <= TOUCH_SWIPE_THRESHOLD_PX
}

fn is_screen_menu_touch(start: lcd::TouchPoint, end: lcd::TouchPoint) -> bool {
    is_screen_menu_point(start) && is_screen_menu_point(end)
}

fn is_screen_menu_point(touch: lcd::TouchPoint) -> bool {
    touch.y < 80 && touch.x >= lcd::LCD_WIDTH * 2 / 3
}

fn is_screen_backspace_point(touch: lcd::TouchPoint) -> bool {
    touch.y < 80 && touch.x < lcd::LCD_WIDTH / 3
}

fn scroll_swipe_message(
    start: lcd::TouchPoint,
    end: lcd::TouchPoint,
) -> Option<protocol::ClientMessage> {
    let dx = (end.x as i32 - start.x as i32).abs();
    let dy = end.y as i32 - start.y as i32;
    if dx > TOUCH_SWIPE_THRESHOLD_PX || dy.abs() < TOUCH_SWIPE_THRESHOLD_PX {
        return None;
    }

    if dy < 0 {
        Some(protocol::ClientMessage::ScrollDown {
            rows: SCREEN_SCROLL_SWIPE_ROWS,
        })
    } else {
        Some(protocol::ClientMessage::ScrollUp {
            rows: SCREEN_SCROLL_SWIPE_ROWS,
        })
    }
}

async fn wait_touch_release(touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>) {
    loop {
        tokio::select! {
            event = touch_rx.recv() => {
                match event {
                    Some(lcd::TouchEvent::Release(_)) | None => break,
                    Some(lcd::TouchEvent::Press(_)) => {}
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                break;
            }
        }
    }
}

async fn show_idle_shutdown_prompt(
    server: &mut MqttServer,
    gui: &mut UI,
    touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
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
    let mut press_index = None;
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
            event = touch_rx.recv() => {
                match event {
                    Some(lcd::TouchEvent::Press(touch)) => {
                        if press_index.is_none() {
                            press_index = crate::ui::list_touch_index(touch, &item_rects);
                        }
                    }
                    Some(lcd::TouchEvent::Release(touch)) => {
                        let release_index = crate::ui::list_touch_index(touch, &item_rects);
                        if press_index.is_some() && press_index == release_index {
                            log::info!("Idle shutdown cancelled");
                            return Ok(true);
                        }
                        press_index = None;
                    }
                    None => return Ok(true),
                }
            }
            ev = server.recv() => {
                match ev {
                    Some(MqttEvent::Presence { list_changed, .. }) => {
                        if list_changed && !sessions_are_all_idle(&server.session_labels()) {
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

/// 会话选择器:显示 vibetty 会话列表,触摸行选定。
async fn open_session_picker(
    server: &mut MqttServer,
    gui: &mut UI,
    touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
    boot_button: &mut BootButton,
    backlight: &mut BacklightMode,
    audio_prompt: Option<&audio::PromptPlayer>,
    audio_prompt_enabled: &mut bool,
    nvs: &esp_idf_svc::nvs::EspDefaultNvs,
) -> anyhow::Result<()> {
    // 入口:retained presence 在 subscribe 后很快到达,但需 poll recv 才进 sessions 表。
    // 最多等 1500ms 让它们落地。
    if server.session_labels().is_empty() {
        let _ = gui.show_status("Loading sessions...", "").await;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(1500);
        loop {
            if !server.session_labels().is_empty() {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                ev = server.recv() => { if ev.is_none() { return Ok(()); } }
            }
        }
    }

    let mut labels = Vec::new();
    let mut item_rects = Vec::new();
    let mut scroll_offset = 0usize;
    let mut last_session_title =
        render_session_picker(server, gui, &mut labels, &mut item_rects, scroll_offset).await;

    let mut press_touch = None;
    let mut press_started_at = None;
    let mut last_list_change = tokio::time::Instant::now();
    let mut next_title_refresh = last_list_change + crate::ui::MENU_TITLE_REFRESH_DELAY;
    let mut off_since = None;
    loop {
        let shutdown_prompt_at = off_since
            .map(|instant| instant + SESSION_LIST_OFF_SHUTDOWN_PROMPT_DELAY)
            .unwrap_or_else(|| tokio::time::Instant::now() + std::time::Duration::from_secs(3600));
        tokio::select! {
            _ = tokio::time::sleep_until(next_title_refresh), if *backlight != BacklightMode::Off => {
                next_title_refresh = tokio::time::Instant::now() + crate::ui::MENU_TITLE_REFRESH_DELAY;
                let title = session_picker_title();
                if title != last_session_title {
                    gui.refresh_list_title(&title).await?;
                    last_session_title = title;
                }
            }
            _ = tokio::time::sleep_until(last_list_change + SESSION_LIST_IDLE_OFF_DELAY), if *backlight != BacklightMode::Off => {
                log::info!("Session list unchanged for 30s, turning screen off");
                backlight.set(BacklightMode::Off)?;
                off_since = Some(tokio::time::Instant::now());
            }
            _ = tokio::time::sleep_until(shutdown_prompt_at), if *backlight == BacklightMode::Off && off_since.is_some() && sessions_are_all_idle(&labels) => {
                log::info!("Session list screen off for 20min with no working sessions; prompting shutdown");
                backlight.set(BacklightMode::Normal)?;
                off_since = None;
                last_list_change = tokio::time::Instant::now();
                next_title_refresh = last_list_change + crate::ui::MENU_TITLE_REFRESH_DELAY;
                if show_idle_shutdown_prompt(server, gui, touch_rx).await? {
                    press_touch = None;
                    last_session_title = render_session_picker(
                        server,
                        gui,
                        &mut labels,
                        &mut item_rects,
                        scroll_offset,
                    ).await;
                } else {
                    log::warn!("Idle shutdown countdown expired, shutting down");
                    crate::power::shutdown();
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(60));
                    }
                }
            }
            _ = crate::boot::wait_boot_press(boot_button) => {
                log::info!("BOOT button pressed from session list, turning screen off");
                backlight.set(BacklightMode::Off)?;
                off_since = Some(tokio::time::Instant::now());
                press_touch = None;
                press_started_at = None;
            }
            _ = tokio::time::sleep_until(
                press_started_at
                    .map(|instant| instant + SESSION_LIST_LONG_PRESS_MENU_DELAY)
                    .unwrap_or_else(|| tokio::time::Instant::now() + std::time::Duration::from_secs(3600))
            ), if press_started_at.is_some() && *backlight != BacklightMode::Off => {
                log::info!("Session list long press detected, opening BOOT menu");
                press_touch = None;
                press_started_at = None;
                match show_boot_menu(server, gui, touch_rx, *audio_prompt_enabled).await? {
                    BootMenuAction::Restart => {
                        log::warn!("BOOT menu restart selected");
                        esp_idf_svc::hal::reset::restart();
                    }
                    BootMenuAction::PowerOff => {
                        log::warn!("BOOT menu power-off selected");
                        crate::power::shutdown();
                        loop {
                            std::thread::sleep(std::time::Duration::from_secs(60));
                        }
                    }
                    BootMenuAction::ScreenOff => {
                        backlight.set(BacklightMode::Off)?;
                        off_since = Some(tokio::time::Instant::now());
                        last_session_title = render_session_picker(
                            server,
                            gui,
                            &mut labels,
                            &mut item_rects,
                            scroll_offset,
                        ).await;
                    }
                    BootMenuAction::ToggleSound => {
                        *audio_prompt_enabled = !*audio_prompt_enabled;
                        if let Err(e) = audio::save_prompt_enabled(nvs, *audio_prompt_enabled) {
                            log::error!("Failed to save sound setting: {e:?}");
                        }
                        log::info!("Audio prompt enabled={}", *audio_prompt_enabled);
                        last_session_title = render_session_picker(
                            server,
                            gui,
                            &mut labels,
                            &mut item_rects,
                            scroll_offset,
                        ).await;
                    }
                    BootMenuAction::Theme => {
                        if let Some(theme) = select_terminal_theme(server, gui, touch_rx).await? {
                            log::info!("Terminal theme selected: {theme}");
                        }
                        last_session_title = render_session_picker(
                            server,
                            gui,
                            &mut labels,
                            &mut item_rects,
                            scroll_offset,
                        ).await;
                    }
                    BootMenuAction::Back => {
                        last_session_title = render_session_picker(
                            server,
                            gui,
                            &mut labels,
                            &mut item_rects,
                            scroll_offset,
                        ).await;
                    }
                }
            }
            event = touch_rx.recv() => {
                if matches!(event, Some(lcd::TouchEvent::Press(_)) | Some(lcd::TouchEvent::Release(_)))
                    && *backlight == BacklightMode::Off
                {
                    log::info!("Touch while screen is off, restoring backlight");
                    backlight.set(BacklightMode::Normal)?;
                    off_since = None;
                    last_list_change = tokio::time::Instant::now();
                    next_title_refresh = last_list_change + crate::ui::MENU_TITLE_REFRESH_DELAY;
                    press_touch = None;
                    press_started_at = None;
                    last_session_title = render_session_picker(
                        server,
                        gui,
                        &mut labels,
                        &mut item_rects,
                        scroll_offset,
                    ).await;
                    continue;
                }
                match event {
                    Some(lcd::TouchEvent::Press(touch)) => {
                        if press_touch.is_none() {
                            press_touch = Some(touch);
                            press_started_at = Some(tokio::time::Instant::now());
                        } else if let Some(start) = press_touch {
                            let dx = touch.x as i32 - start.x as i32;
                            let dy = touch.y as i32 - start.y as i32;
                            if dx.abs() > TOUCH_SWIPE_THRESHOLD_PX
                                || dy.abs() > TOUCH_SWIPE_THRESHOLD_PX
                            {
                                log::debug!(
                                    "Session list long press cancelled by movement: dx={dx} dy={dy}"
                                );
                                press_started_at = None;
                            }
                        }
                    }
                    Some(lcd::TouchEvent::Release(touch)) => {
                        press_started_at = None;
                        let Some(start) = press_touch.take() else {
                            continue;
                        };
                        if is_back_swipe(start, touch) {
                            log::info!("Session list right swipe detected, refreshing");
                            last_list_change = tokio::time::Instant::now();
                            next_title_refresh = last_list_change + crate::ui::MENU_TITLE_REFRESH_DELAY;
                            scroll_offset = clamp_session_scroll_offset(
                                server,
                                scroll_offset,
                                item_rects.len().max(1),
                            );
                            last_session_title = render_session_picker(
                                server,
                                gui,
                                &mut labels,
                                &mut item_rects,
                                scroll_offset,
                            ).await;
                            continue;
                        }
                        if let Some(delta) = list_scroll_delta(start, touch) {
                            let visible_count = item_rects.len().max(1);
                            let max_offset = labels.len().saturating_sub(visible_count);
                            let next_offset = if delta > 0 {
                                scroll_offset.saturating_add(delta as usize).min(max_offset)
                            } else {
                                scroll_offset.saturating_sub((-delta) as usize)
                            };
                            if next_offset != scroll_offset {
                                scroll_offset = next_offset;
                                log::info!("Session list scroll offset={}", scroll_offset);
                                last_session_title = render_session_picker(
                                    server,
                                    gui,
                                    &mut labels,
                                    &mut item_rects,
                                    scroll_offset,
                                ).await;
                            }
                            continue;
                        }

                        let press_index = crate::ui::list_touch_index(start, &item_rects);
                        let release_index = crate::ui::list_touch_index(touch, &item_rects);
                        if press_index.is_some() && press_index == release_index {
                            let visible_index = press_index.unwrap();
                            let index = scroll_offset + visible_index;
                            if let Some((prefix, ..)) = labels.get(index) {
                                let prefix = prefix.clone();
                                backlight.set(BacklightMode::Normal)?;
                                gui.show_loading_modal().await?;
                                server.set_active(&prefix);
                                server.flush_pending().await?;
                                send_active_sync(server, false).await?;
                                return Ok(());
                            }
                        }
                    }
                    None => {
                        log::warn!("Touch event source closed during session picker");
                        return Ok(());
                    }
                }
            }
            ev = server.recv() => {
                // 等 presence 更新列表;ActiveScreen 在选择器开着时忽略。
                match ev {
                    Some(MqttEvent::Presence { list_changed, .. }) => {
                        if list_changed {
                            last_list_change = tokio::time::Instant::now();
                            next_title_refresh = last_list_change + crate::ui::MENU_TITLE_REFRESH_DELAY;
                            backlight.set(BacklightMode::Normal)?;
                            let next_labels = server.session_labels();
                            let play_prompt = session_became_idle(&labels, &next_labels);
                            if play_prompt && *audio_prompt_enabled {
                                if let Some(prompt) = audio_prompt {
                                    prompt.play_async();
                                }
                            }
                            off_since = None;
                            scroll_offset = clamp_session_scroll_offset(server, scroll_offset, item_rects.len().max(1));
                            last_session_title = render_session_picker_with_labels(
                                gui,
                                &mut labels,
                                next_labels,
                                &mut item_rects,
                                scroll_offset,
                            ).await;
                        }
                    }
                    Some(MqttEvent::ActiveScreen(_)) | Some(MqttEvent::ActiveText(_)) => continue,
                    None => return Ok(()),
                }
            }
        }
    }
}

type SessionLabel = (String, String, bool, bool);

fn sessions_are_all_idle(labels: &[SessionLabel]) -> bool {
    labels.iter().all(|(_, _, _, is_working)| !*is_working)
}

fn session_became_idle(previous: &[SessionLabel], next: &[SessionLabel]) -> bool {
    next.iter().any(|(prefix, _, _, is_working)| {
        !*is_working
            && previous
                .iter()
                .any(|(old_prefix, _, _, old_working)| old_prefix == prefix && *old_working)
    })
}

async fn render_session_picker(
    server: &MqttServer,
    gui: &mut UI,
    labels: &mut Vec<SessionLabel>,
    item_rects: &mut Vec<embedded_graphics::primitives::Rectangle>,
    scroll_offset: usize,
) -> String {
    let next_labels = server.session_labels();
    render_session_picker_with_labels(gui, labels, next_labels, item_rects, scroll_offset).await
}

async fn render_session_picker_with_labels(
    gui: &mut UI,
    labels: &mut Vec<SessionLabel>,
    next_labels: Vec<SessionLabel>,
    item_rects: &mut Vec<embedded_graphics::primitives::Rectangle>,
    scroll_offset: usize,
) -> String {
    *labels = next_labels;
    if labels.is_empty() {
        let _ = gui.show_status("no session", "").await;
        item_rects.clear();
        String::new()
    } else {
        let items: Vec<(String, bool)> = labels
            .iter()
            .skip(scroll_offset)
            .map(|(_, label, _, is_working)| (label.clone(), *is_working))
            .collect();
        let title = session_picker_title();
        *item_rects = gui
            .display_menu_list(&title, &items)
            .await
            .unwrap_or_default();
        title
    }
}

fn session_picker_title() -> String {
    match crate::power::battery_percent() {
        Some(percent) => format!("Session: Battery {percent}%"),
        None => "Session: Battery --".to_string(),
    }
}

fn clamp_session_scroll_offset(
    server: &MqttServer,
    scroll_offset: usize,
    visible_count: usize,
) -> usize {
    server
        .session_labels()
        .len()
        .saturating_sub(visible_count)
        .min(scroll_offset)
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
