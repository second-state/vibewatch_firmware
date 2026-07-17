//! Remote 模式:MQTT 连 vibetty broker。启动先进入 session list(触摸选会话),
//! 选定后订阅该会话的整屏 JPEG、解码刷到 LCD。
//!
//! picker 支持触摸选择会话:点列表行后设置 active session 并发送 sync。

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use crate::{audio, lcd, mqtt::MqttEvent, mqtt::MqttServer, new_jpg, protocol, ui::UI};

pub async fn run(
    uri: String,
    client_id: String,
    gui: &mut UI,
    mut touch_rx: tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
    asr_tx: std::sync::mpsc::Sender<audio::AsrRequest>,
    asr_config: Option<&audio::AsrConfig>,
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

    // session list:触摸选择会话。
    open_session_picker(&mut server, gui, &mut touch_rx).await?;

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
                            if is_back_swipe(start, touch) {
                                log::info!("Right swipe detected, returning to session list");
                                server.clear_active();
                                server.flush_pending().await?;
                                open_session_picker(&mut server, gui, &mut touch_rx).await?;
                            } else if let Some(msg) = scroll_swipe_message(start, touch) {
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
                handle_mqtt_event(ev).await?;
            }
        }
    }

    Ok(())
}

async fn handle_mqtt_event(ev: MqttEvent) -> anyhow::Result<()> {
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
                    if let Err(e) = display.flush_to_lcd() {
                        log::error!("flush screen failed: {e:?}");
                    }
                }
                Err(e) => log::error!("decode JPEG failed: {e:?}"),
            }
        }
        MqttEvent::Presence { prefix, online, .. } => {
            log::info!("Presence: {prefix} online={online}");
        }
    }

    Ok(())
}

async fn run_touch_asr(
    server: &mut MqttServer,
    gui: &mut UI,
    touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
    asr_tx: &std::sync::mpsc::Sender<audio::AsrRequest>,
    asr_config: Option<&audio::AsrConfig>,
) -> anyhow::Result<()> {
    let Some(config) = asr_config.cloned() else {
        let _ = gui.show_status("ASR not configured", "Set asr_config over BLE");
        wait_touch_release(touch_rx).await;
        return Ok(());
    };

    let (respond, result) = tokio::sync::oneshot::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let req = audio::AsrRequest {
        config,
        cancel: cancel.clone(),
        respond,
    };

    let _ = gui.show_status("Listening...", "Release to stop");

    if asr_tx.send(req).is_err() {
        let _ = gui.show_status("ASR unavailable", "");
        return Ok(());
    }

    let mut result = std::pin::pin!(result);
    let asr_result = loop {
        tokio::select! {
            response = &mut result => {
                break response.unwrap_or_else(|_| Err(anyhow::anyhow!("ASR worker dropped request")));
            }
            event = touch_rx.recv() => {
                if !update_asr_touch_pressed(event) {
                    cancel.store(true, Ordering::Relaxed);
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
        Ok(text) if !text.trim().is_empty() => {
            let text = text.trim().to_string();
            log::info!("Local ASR result: {text}");
            let _ = gui.show_status("Sending ASR", text.clone());
            server.send(protocol::ClientMessage::Input(text)).await?;
            server.send(protocol::ClientMessage::sync()).await?;
        }
        Ok(_) => {
            let _ = gui.show_status("ASR empty", "Tap to return");
            wait_touch_press(touch_rx).await;
            wait_touch_release(touch_rx).await;
            server.send(protocol::ClientMessage::sync()).await?;
        }
        Err(e) => {
            log::error!("ASR failed: {e:?}");
            let _ = gui.show_status("ASR failed", format!("{e:?}"));
        }
    }

    Ok(())
}

async fn wait_touch_press(touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>) {
    loop {
        tokio::select! {
            event = touch_rx.recv() => {
                match event {
                    Some(lcd::TouchEvent::Press(_)) | None => break,
                    Some(lcd::TouchEvent::Release(_)) => {}
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                break;
            }
        }
    }
}

fn update_asr_touch_pressed(event: Option<lcd::TouchEvent>) -> bool {
    match event {
        Some(lcd::TouchEvent::Press(touch)) => is_asr_touch(touch),
        Some(lcd::TouchEvent::Release(_)) | None => false,
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
        Some(protocol::ClientMessage::ScrollDown { rows: 0 })
    } else {
        Some(protocol::ClientMessage::ScrollUp { rows: 0 })
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

/// 会话选择器:显示 vibetty 会话列表,触摸行选定。
async fn open_session_picker(
    server: &mut MqttServer,
    gui: &mut UI,
    touch_rx: &mut tokio::sync::mpsc::Receiver<lcd::TouchEvent>,
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
    render_session_picker(server, gui, &mut labels, &mut item_rects, scroll_offset);

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
                            let max_offset = labels.len().saturating_sub(visible_count);
                            let next_offset = if delta > 0 {
                                scroll_offset.saturating_add(delta as usize).min(max_offset)
                            } else {
                                scroll_offset.saturating_sub((-delta) as usize)
                            };
                            if next_offset != scroll_offset {
                                scroll_offset = next_offset;
                                log::info!("Session list scroll offset={}", scroll_offset);
                                render_session_picker(
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
                                server.set_active(&prefix);
                                server.send(protocol::ClientMessage::sync()).await?;
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
                            scroll_offset = clamp_session_scroll_offset(server, scroll_offset, item_rects.len().max(1));
                            render_session_picker(
                                server,
                                gui,
                                &mut labels,
                                &mut item_rects,
                                scroll_offset,
                            );
                        }
                    }
                    Some(MqttEvent::ActiveScreen(_)) => continue,
                    None => return Ok(()),
                }
            }
        }
    }
}

type SessionLabel = (String, String, bool, bool);

fn render_session_picker(
    server: &MqttServer,
    gui: &mut UI,
    labels: &mut Vec<SessionLabel>,
    item_rects: &mut Vec<embedded_graphics::primitives::Rectangle>,
    scroll_offset: usize,
) {
    *labels = server.session_labels();
    if labels.is_empty() {
        let _ = gui.show_status("no session", "");
        item_rects.clear();
    } else {
        let items: Vec<(String, bool)> = labels
            .iter()
            .skip(scroll_offset)
            .map(|(_, label, _, is_working)| (label.clone(), *is_working))
            .collect();
        *item_rects = gui.display_list("Sessions", &items, 0).unwrap_or_default();
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
