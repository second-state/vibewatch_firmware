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
    audio, boot::BootButton, lcd, mqtt::MqttEvent, mqtt::MqttServer, new_jpg, protocol, ui::UI,
};

const BACKLIGHT_NORMAL: u8 = 50;
const SESSION_LIST_IDLE_OFF_DELAY: std::time::Duration = std::time::Duration::from_secs(30);
const SESSION_LIST_OFF_SHUTDOWN_PROMPT_DELAY: std::time::Duration =
    std::time::Duration::from_secs(10 * 60);
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
        let level = match mode {
            Self::Normal => BACKLIGHT_NORMAL,
            Self::Off => 0,
        };
        crate::lcd::set_backlight(level)?;
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
    audio_prompt: Option<&audio::Prompt>,
    mut audio_prompt_enabled: bool,
    nvs: &esp_idf_svc::nvs::EspDefaultNvs,
) -> anyhow::Result<()> {
    log::info!("Connecting to MQTT broker {uri} as {client_id}");
    let mut server = match MqttServer::new(&uri, &client_id).await {
        Ok(s) => s,
        Err(e) => {
            log::error!("MQTT connect failed: {e:?}");
            let _ = gui.show_status("MQTT failed", format!("{e:?}"));
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
    // 选定会话后:主循环,解码并刷屏
    loop {
        // 把活跃会话落实为 `{prefix}/screen` 订阅(不可被取消)。
        server.flush_pending().await?;

        tokio::select! {
            event = touch_rx.recv() => {
                match event {
                    Some(lcd::TouchEvent::Press(touch)) if is_asr_touch(touch) => {
                        swipe_start = None;
                        wait_touch_release(&mut touch_rx).await;
                        run_touch_asr(&mut server, gui, &mut touch_rx, &asr_tx, asr_config).await?;
                    }
                    Some(lcd::TouchEvent::Press(touch)) => {
                        if swipe_start.is_none() {
                            swipe_start = Some(touch);
                        }
                    }
                    Some(lcd::TouchEvent::Release(touch)) => {
                        if let Some(start) = swipe_start.take() {
                            let dx = touch.x as i32 - start.x as i32;
                            let dy = touch.y as i32 - start.y as i32;
                            log::info!("Swipe candidate: dx={} dy={}", dx, dy);
                            if is_screen_menu_touch(start, touch) {
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
                                    match try_local_text_scroll(gui, &msg) {
                                        Ok(true) => continue,
                                        Ok(false) => {}
                                        Err(e) => log::warn!("Local text scroll failed: {e:?}"),
                                    }
                                }
                                log::info!("Vertical swipe detected, sending {msg:?}");
                                server.send(msg).await?;
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
                handle_mqtt_event(ev, gui, &mut backlight).await?;
            }
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
                    if let Err(e) = gui.show_jpeg_screen(display) {
                        log::error!("flush screen failed: {e:?}");
                    }
                }
                Err(e) => log::error!("decode JPEG failed: {e:?}"),
            }
        }
        MqttEvent::ActiveText(frame) => {
            log::info!("Screen text frame: {}B", frame.len());
            if let Err(e) = gui.show_terminal_text_frame(&frame) {
                log::error!("flush text screen failed: {e:?}");
            }
        }
        MqttEvent::Presence {
            prefix,
            online,
            list_changed,
        } => {
            log::info!("Presence: {prefix} online={online}");
            if list_changed {
                backlight.set(BacklightMode::Normal)?;
            }
        }
    }

    Ok(())
}

async fn send_active_sync(server: &mut MqttServer, close: bool) -> anyhow::Result<()> {
    let msg = if server.active_uses_text_screen() {
        let (cols, rows) = crate::ui::terminal_text_cells();
        log::info!("Sending text-mode sync: cols={cols} rows={rows} close={close}");
        protocol::ClientMessage::sync_cells(cols, rows, close)
    } else {
        protocol::ClientMessage::sync_with_close(close)
    };
    server.send(msg).await
}

fn try_local_text_scroll(gui: &mut UI, msg: &protocol::ClientMessage) -> anyhow::Result<bool> {
    match msg {
        protocol::ClientMessage::ScrollUp { .. } => {
            gui.scroll_terminal_text(crate::ui::TerminalScroll::Up)
        }
        protocol::ClientMessage::ScrollDown { .. } => {
            gui.scroll_terminal_text(crate::ui::TerminalScroll::Down)
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
    RestoreScreen,
    ToggleSound,
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
    let items = vec![
        boot_menu_item(0, "Reboot", crate::ui::UiColor::CSS_DARK_ORANGE),
        boot_menu_item(1, "Power Off", crate::ui::UiColor::CSS_RED),
        boot_menu_item(2, "Screen Off", crate::ui::UiColor::CSS_GRAY),
        boot_menu_item(3, "Restore", crate::ui::UiColor::CSS_DARK_BLUE),
        boot_menu_item(4, sound_label, crate::ui::UiColor::CSS_GREEN),
        boot_menu_item(5, "Back", crate::ui::UiColor::CSS_BLACK),
    ];
    let index = select_remote_list_item(server, gui, touch_rx, "System", &items).await?;
    Ok(match index {
        0 => BootMenuAction::Restart,
        1 => BootMenuAction::PowerOff,
        2 => BootMenuAction::ScreenOff,
        3 => BootMenuAction::RestoreScreen,
        4 => BootMenuAction::ToggleSound,
        5 => BootMenuAction::Back,
        _ => unreachable!(),
    })
}

fn boot_menu_item(index: usize, text: &str, bg: crate::ui::UiColor) -> crate::ui::ListItem {
    const ITEM_H: i32 = 66;
    const START_Y: i32 = 30;
    crate::ui::ListItem::new(
        Rectangle::new(
            Point::new(0, START_Y + index as i32 * ITEM_H),
            Size::new(lcd::LCD_WIDTH as u32, ITEM_H as u32),
        ),
        text,
        Some(bg),
        Some(crate::ui::TEXT_LIGHT),
    )
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
        redraw_active_cached_screen(server, gui)?;
        return Ok(());
    };
    let action = match index {
        0 => ScreenAction::Esc,
        1 => ScreenAction::Next,
        2 => ScreenAction::Yolo,
        3 => ScreenAction::Enter,
        _ => return Ok(()),
    };
    match action {
        ScreenAction::Esc => {
            server
                .send(protocol::ClientMessage::pty_input_str("\x1b"))
                .await?
        }
        ScreenAction::Next => {
            server
                .send(protocol::ClientMessage::pty_input_str("\x1b[B"))
                .await?
        }
        ScreenAction::Yolo => {
            server
                .send(protocol::ClientMessage::pty_input_str("\x1b[Z"))
                .await?
        }
        ScreenAction::Enter => {
            server
                .send(protocol::ClientMessage::pty_input_str("\r"))
                .await?
        }
    }
    if server.active_uses_text_screen() {
        redraw_active_cached_screen(server, gui)?;
    } else {
        send_active_sync(server, false).await?;
    }
    Ok(())
}

fn redraw_active_cached_screen(server: &MqttServer, gui: &mut UI) -> anyhow::Result<()> {
    if server.active_uses_text_screen() {
        if !gui.redraw_cached_terminal_text()? {
            log::warn!("no cached terminal text screen to redraw");
        }
    } else if !gui.redraw_cached_jpeg_screen()? {
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
    let item_rects = gui.display_menu_list(title, items)?;
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
) -> anyhow::Result<usize> {
    let item_rects = gui.display_list(title, items)?;
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
                            return Ok(press_index.unwrap());
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
    let _ = gui.show_asr_editor(&editor.display_text(), hint);

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
                            let _ = gui.show_asr_editor(&editor.display_text(), hint);
                        } else if let Some(action) = top_asr_action(touch) {
                            apply_top_asr_action(action, &mut editor);
                            let _ = gui.show_asr_editor(&editor.display_text(), hint);
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
                                    server.send(protocol::ClientMessage::Input(text)).await?;
                                    if text_mode {
                                        if let Err(e) = gui.redraw_cached_terminal_text() {
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

    let (respond, result) = tokio::sync::oneshot::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let req = audio::AsrRequest {
        config,
        cancel: cancel.clone(),
        respond,
    };

    let _ = gui.show_asr_editor(display_text, "Listening...");

    if asr_tx.send(req).is_err() {
        wait_touch_release(touch_rx).await;
        return Err(anyhow::anyhow!("ASR unavailable"));
    }

    let mut result = std::pin::pin!(result);
    let asr_result = loop {
        tokio::select! {
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
                let _ = gui.show_asr_editor(&editor.display_text(), hint);
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
    const MIN_VERTICAL_SWIPE_PX: i32 = 80;
    const MAX_HORIZONTAL_DRIFT_PX: i32 = 100;

    if is_asr_touch(start) || top_asr_action(start).is_some() {
        return None;
    }

    let dx = (end.x as i32 - start.x as i32).abs();
    let dy = end.y as i32 - start.y as i32;
    if dx > MAX_HORIZONTAL_DRIFT_PX || dy.abs() < MIN_VERTICAL_SWIPE_PX {
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
    const MIN_RIGHT_SWIPE_PX: i32 = 80;
    const MAX_VERTICAL_DRIFT_PX: i32 = 80;

    let dx = end.x as i32 - start.x as i32;
    let dy = (end.y as i32 - start.y as i32).abs();
    dx >= MIN_RIGHT_SWIPE_PX && dy <= MAX_VERTICAL_DRIFT_PX
}

fn is_screen_menu_touch(start: lcd::TouchPoint, end: lcd::TouchPoint) -> bool {
    is_screen_menu_point(start) && is_screen_menu_point(end)
}

fn is_screen_menu_point(touch: lcd::TouchPoint) -> bool {
    touch.y < 80 && touch.x >= lcd::LCD_WIDTH * 2 / 3
}

fn scroll_swipe_message(
    start: lcd::TouchPoint,
    end: lcd::TouchPoint,
) -> Option<protocol::ClientMessage> {
    const MIN_VERTICAL_SWIPE_PX: i32 = 80;
    const MAX_HORIZONTAL_DRIFT_PX: i32 = 80;

    let dx = (end.x as i32 - start.x as i32).abs();
    let dy = end.y as i32 - start.y as i32;
    if dx > MAX_HORIZONTAL_DRIFT_PX || dy.abs() < MIN_VERTICAL_SWIPE_PX {
        return None;
    }

    if dy < 0 {
        Some(protocol::ClientMessage::ScrollDown { rows: 10 })
    } else {
        Some(protocol::ClientMessage::ScrollUp { rows: 10 })
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
    let item_rects = gui.display_list(&title, &items)?;
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
                gui.refresh_list_title(&title)?;
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
    audio_prompt: Option<&audio::Prompt>,
    audio_prompt_enabled: &mut bool,
    nvs: &esp_idf_svc::nvs::EspDefaultNvs,
) -> anyhow::Result<()> {
    // 入口:retained presence 在 subscribe 后很快到达,但需 poll recv 才进 sessions 表。
    // 最多等 1500ms 让它们落地。
    if server.session_labels().is_empty() {
        let _ = gui.show_status("Loading sessions...", "");
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
        render_session_picker(server, gui, &mut labels, &mut item_rects, scroll_offset);

    let mut press_touch = None;
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
                    gui.refresh_list_title(&title)?;
                    last_session_title = title;
                }
            }
            _ = tokio::time::sleep_until(last_list_change + SESSION_LIST_IDLE_OFF_DELAY), if *backlight != BacklightMode::Off => {
                log::info!("Session list unchanged for 30s, turning screen off");
                backlight.set(BacklightMode::Off)?;
                off_since = Some(tokio::time::Instant::now());
            }
            _ = tokio::time::sleep_until(shutdown_prompt_at), if *backlight == BacklightMode::Off && off_since.is_some() && sessions_are_all_idle(&labels) => {
                log::info!("Session list screen off for 10min with no working sessions; prompting shutdown");
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
                    );
                } else {
                    log::warn!("Idle shutdown countdown expired, shutting down");
                    crate::power::shutdown();
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(60));
                    }
                }
            }
            _ = crate::boot::wait_boot_press(boot_button) => {
                log::info!("BOOT menu requested from session list");
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
                        );
                    }
                    BootMenuAction::RestoreScreen => {
                        backlight.set(BacklightMode::Normal)?;
                        off_since = None;
                        last_list_change = tokio::time::Instant::now();
                        next_title_refresh = last_list_change + crate::ui::MENU_TITLE_REFRESH_DELAY;
                        last_session_title = render_session_picker(
                            server,
                            gui,
                            &mut labels,
                            &mut item_rects,
                            scroll_offset,
                        );
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
                        );
                    }
                    BootMenuAction::Back => {
                        last_session_title = render_session_picker(
                            server,
                            gui,
                            &mut labels,
                            &mut item_rects,
                            scroll_offset,
                        );
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
                    last_session_title = render_session_picker(
                        server,
                        gui,
                        &mut labels,
                        &mut item_rects,
                        scroll_offset,
                    );
                    continue;
                }
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
                                );
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
                                server.set_active(&prefix);
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
                            if *audio_prompt_enabled {
                                if let Some(prompt) = audio_prompt {
                                    prompt.play_async();
                                }
                            }
                            off_since = None;
                            scroll_offset = clamp_session_scroll_offset(server, scroll_offset, item_rects.len().max(1));
                            last_session_title = render_session_picker(
                                server,
                                gui,
                                &mut labels,
                                &mut item_rects,
                                scroll_offset,
                            );
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

fn render_session_picker(
    server: &MqttServer,
    gui: &mut UI,
    labels: &mut Vec<SessionLabel>,
    item_rects: &mut Vec<embedded_graphics::primitives::Rectangle>,
    scroll_offset: usize,
) -> String {
    *labels = server.session_labels();
    if labels.is_empty() {
        let _ = gui.show_status("no session", "");
        item_rects.clear();
        String::new()
    } else {
        let items: Vec<(String, bool)> = labels
            .iter()
            .skip(scroll_offset)
            .map(|(_, label, _, is_working)| (label.clone(), *is_working))
            .collect();
        let title = session_picker_title();
        *item_rects = gui.display_menu_list(&title, &items).unwrap_or_default();
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
    const MIN_VERTICAL_SWIPE_PX: i32 = 80;
    const MAX_HORIZONTAL_DRIFT_PX: i32 = 80;

    let dx = (end.x as i32 - start.x as i32).abs();
    let dy = end.y as i32 - start.y as i32;
    if dx > MAX_HORIZONTAL_DRIFT_PX || dy.abs() < MIN_VERTICAL_SWIPE_PX {
        return None;
    }

    if dy < 0 {
        Some(-1)
    } else {
        Some(1)
    }
}
