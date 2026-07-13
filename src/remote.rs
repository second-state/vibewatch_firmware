//! Remote 模式:MQTT 连 vibetty broker。启动先进入 session list(停留等输入选会话),
//! 选定后订阅该会话的整屏 JPEG、解码刷到 LCD。
//!
//! 输入暂未实现(stub):picker 无输入,持续显示会话列表。将来接上按键/旋钮后,
//! 在 `open_session_picker` 里响应 NEXT/ACCEPT/ESC,调 `server.set_active` + `sync`。

use crate::{mqtt::MqttEvent, mqtt::MqttServer, new_jpg, protocol, ui::UI};

pub async fn run(uri: String, client_id: String, gui: &mut UI) -> anyhow::Result<()> {
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

    // session list:停留等输入选定会话(stub:无输入,持续显示列表)
    open_session_picker(&mut server, gui).await?;

    // 选定会话后:主循环,解码并刷屏
    loop {
        // 把活跃会话落实为 `{prefix}/screen` 订阅(不可被取消)。
        server.flush_pending().await?;

        let Some(ev) = server.recv().await else {
            log::warn!("MQTT event source closed, exiting remote loop");
            break;
        };

        match ev {
            MqttEvent::ActiveScreen(chunk) => {
                if !matches!(chunk.format, protocol::ImageFormat::Jpeg) {
                    log::warn!("Unsupported screen format {:?}, only JPEG", chunk.format);
                    continue;
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
                        // 412×412 单页,整张刷到 (0,0)。
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
    }

    Ok(())
}

/// 会话选择器:显示 vibetty 会话列表,停留等输入选定。
/// stub(无输入):持续用最新 presence 刷新列表;将来接按键后在此响应 NEXT/ACCEPT/ESC。
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
            // stub:无输入,焦点恒为 0。将来接按键:响应 NEXT 移焦点、ACCEPT 调
            // server.set_active(focus_prefix) + server.send(ClientMessage::sync()) 后 return。
            let _ = gui.display_session_list("Sessions", &items, 0);
        }

        // 等 presence 更新列表;ActiveScreen 在选择器开着时忽略。
        match server.recv().await {
            Some(MqttEvent::Presence { .. }) => continue,
            Some(MqttEvent::ActiveScreen(_)) => continue,
            None => return Ok(()),
        }
    }
}
