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
    asr_tx: std::sync::mpsc::Sender<audio::AsrRequest>,
    asr_config: Option<&audio::AsrConfig>,
) -> anyhow::Result<()> {
    log::info!("Connecting to MQTT broker {uri} as {client_id}");
    let mut server = match MqttServer::new(&uri, &client_id).await {
        Ok(s) => s,
        Err(e) => {
            log::error!("MQTT connect failed: {e:?}");
            gui.state = "MQTT failed".to_string();
            gui.text = format!("{e:?}");
            let _ = gui.display_flush();
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            return Err(e);
        }
    };
    log::info!("MQTT connected, entering session list");

    // session list:触摸选择会话。
    open_session_picker(&mut server, gui).await?;

    // 选定会话后:主循环,解码并刷屏
    loop {
        // 把活跃会话落实为 `{prefix}/screen` 订阅(不可被取消)。
        server.flush_pending().await?;

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                if asr_touch_pressed() {
                    run_touch_asr(&mut server, gui, &asr_tx, asr_config).await?;
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
        MqttEvent::Presence { prefix, online } => {
            log::info!("Presence: {prefix} online={online}");
        }
    }

    Ok(())
}

async fn run_touch_asr(
    server: &mut MqttServer,
    gui: &mut UI,
    asr_tx: &std::sync::mpsc::Sender<audio::AsrRequest>,
    asr_config: Option<&audio::AsrConfig>,
) -> anyhow::Result<()> {
    let Some(config) = asr_config.cloned() else {
        gui.state = "ASR not configured".to_string();
        gui.text = "Set asr_config over BLE".to_string();
        let _ = gui.display_flush();
        wait_touch_release().await;
        return Ok(());
    };

    let (respond, result) = tokio::sync::oneshot::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let req = audio::AsrRequest {
        config,
        cancel: cancel.clone(),
        respond,
    };

    gui.state = "Listening...".to_string();
    gui.text = "Release to stop".to_string();
    let _ = gui.display_flush();

    if asr_tx.send(req).is_err() {
        gui.state = "ASR unavailable".to_string();
        gui.text.clear();
        let _ = gui.display_flush();
        return Ok(());
    }

    let mut result = std::pin::pin!(result);
    let asr_result = loop {
        tokio::select! {
            response = &mut result => {
                break response.unwrap_or_else(|_| Err(anyhow::anyhow!("ASR worker dropped request")));
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                if !asr_touch_pressed() {
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
            gui.state = "Sending ASR".to_string();
            gui.text = text.clone();
            let _ = gui.display_flush();
            server.send(protocol::ClientMessage::Input(text)).await?;
            server.send(protocol::ClientMessage::sync()).await?;
        }
        Ok(_) => {
            gui.state = "ASR empty".to_string();
            gui.text.clear();
            let _ = gui.display_flush();
        }
        Err(e) => {
            log::error!("ASR failed: {e:?}");
            gui.state = "ASR failed".to_string();
            gui.text = format!("{e:?}");
            let _ = gui.display_flush();
        }
    }

    Ok(())
}

fn asr_touch_pressed() -> bool {
    let Some(touch) = lcd::read_touch() else {
        return false;
    };
    touch.y > lcd::LCD_HEIGHT.saturating_sub(80)
}

async fn wait_touch_release() {
    for _ in 0..20 {
        if !asr_touch_pressed() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// 会话选择器:显示 vibetty 会话列表,触摸行选定。
async fn open_session_picker(server: &mut MqttServer, gui: &mut UI) -> anyhow::Result<()> {
    // 入口:retained presence 在 subscribe 后很快到达,但需 poll recv 才进 sessions 表。
    // 最多等 1500ms 让它们落地。
    if server.session_labels().is_empty() {
        gui.state = "Loading sessions...".to_string();
        gui.text.clear();
        let _ = gui.display_flush();
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

    loop {
        let labels = server.session_labels();
        if labels.is_empty() {
            gui.state = "no session".to_string();
            gui.text.clear();
            let _ = gui.display_flush();
        } else {
            let items: Vec<(String, bool)> = labels
                .iter()
                .map(|(_, label, _, is_working)| (label.clone(), *is_working))
                .collect();
            let _ = gui.display_session_list("Sessions", &items, 0);
        }

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                if let Some(index) = touched_session_index(labels.len()) {
                    let prefix = labels[index].0.clone();
                    server.set_active(&prefix);
                    server.send(protocol::ClientMessage::sync()).await?;
                    return Ok(());
                }
            }
            ev = server.recv() => {
                // 等 presence 更新列表;ActiveScreen 在选择器开着时忽略。
                match ev {
                    Some(MqttEvent::Presence { .. }) => continue,
                    Some(MqttEvent::ActiveScreen(_)) => continue,
                    None => return Ok(()),
                }
            }
        }
    }
}

fn touched_session_index(len: usize) -> Option<usize> {
    let touch = lcd::read_touch()?;
    const ITEM_H: u16 = 22;
    const START_Y: u16 = 30;
    const TEXT_BASELINE_OFFSET: u16 = 17;

    let list_top = START_Y.saturating_sub(TEXT_BASELINE_OFFSET);
    if touch.y < list_top {
        return None;
    }

    let index = ((touch.y - list_top) / ITEM_H) as usize;
    (index < len).then_some(index)
}
