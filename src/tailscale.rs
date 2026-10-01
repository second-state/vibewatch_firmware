//! Experimental Tailscale page (try-tailscale branch): joins the tailnet via
//! the microlink component and lists known peers. Back button or right swipe
//! returns to the settings page.

use esp_idf_svc::sys::microlink as ml;

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Peer probed with an HTTP GET once per second while the page is open.
const PROBE_IP: &str = "100.107.32.114";
const PROBE_PORT: u16 = 9090;
const PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

pub async fn run(
    wifi: &mut crate::network::WifiManager,
    setting: &crate::setting::Setting,
    gui: &mut crate::ui::UI,
    touch: &mut crate::touch::TouchInput,
) -> anyhow::Result<()> {
    crate::util::log_heap_usage("page -> tailscale");
    gui.show_status("Tailscale", "Connecting WiFi...")
        .await
        .ok();
    if let Err(e) = wifi.connect(&setting.wifi_list) {
        log::error!("Tailscale wifi connect failed: {e:?}");
        gui.show_status("Tailscale", "Connect WiFi failed")
            .await
            .ok();
        std::thread::sleep(std::time::Duration::from_secs(2));
        return Ok(());
    }

    let Some(handle) = crate::microlink::start_and_wait(
        crate::microlink::TAILSCALE_AUTH_KEY,
        crate::microlink::DEVICE_NAME,
        "Tailscale",
        CONNECT_TIMEOUT,
        gui,
    )
    .await
    else {
        return Ok(());
    };

    let (vpn_ip_str, count, peers) = collect_tailnet_info(&handle);
    let count_text = count_line(count);
    let lines = build_lines(&vpn_ip_str, &count_text, &peers);
    let mut hits = gui.display_ota_page("Tailscale", &lines, "").await?;

    // Move our DERP "mailbox" to the probe target's home region. microlink
    // only delivers relayed packets through our own home region, which
    // returns PeerGone when the peer lives in a different region.
    let probe_ip_cstr = std::ffi::CString::new(PROBE_IP).unwrap();
    let probe_target_ip = unsafe { ml::microlink_parse_ip(probe_ip_cstr.as_ptr()) };
    let peer_region = unsafe { ml::microlink_get_peer_home_region(handle.0, probe_target_ip) };
    if peer_region > 0 {
        log::info!("Tailscale: rehoming our DERP mailbox to peer region {peer_region}");
        // SAFETY: valid handle from microlink_init.
        unsafe {
            ml::microlink_rehome_derp(handle.0, peer_region);
        }
    }

    // Kick the WG handshake toward the probe target. Our traffic path is
    // tokio TCP, which unlike microlink_tcp_connect does not trigger the
    // handshake itself — and microlink's peers are passive by default.
    handle.trigger_handshake(PROBE_IP);

    // Probe loop: one HTTP GET per second to the peer over the tunnel,
    // interleaved with touch handling (back/right swipe exits the page).
    let mut probe_tick = tokio::time::interval(PROBE_INTERVAL);
    probe_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = probe_tick.tick() => {
                let ok = http_get_once().await;
                log::info!(
                    "Tailscale probe GET http://{PROBE_IP}:{PROBE_PORT} -> {}",
                    if ok { "ok" } else { "failed" }
                );
                if !ok {
                    // Tunnel still down (e.g. DERP reconnect in flight after
                    // the rehome) — re-kick the WG handshake each attempt.
                    handle.trigger_handshake(PROBE_IP);
                }
                // Refresh the peer list once per second.
                let (vpn_ip_str, count, peers) = collect_tailnet_info(&handle);
                let count_text = count_line(count);
                let lines = build_lines(&vpn_ip_str, &count_text, &peers);
                hits = gui.display_ota_page("Tailscale", &lines, "").await?;
            }
            gesture = touch.next_gesture() => {
                match gesture {
                    Some(crate::touch::TouchGesture::Click { start, end }) => {
                        if hits.back_hit_pair(start, end) {
                            log::info!("Tailscale page: back selected");
                            break;
                        }
                    }
                    Some(crate::touch::TouchGesture::Swipe {
                        direction: crate::touch::SwipeDirection::Right,
                        ..
                    }) => {
                        log::info!("Tailscale page: right swipe, going back to settings");
                        break;
                    }
                    Some(_) => {}
                    None => return Err(anyhow::anyhow!("touch event source closed")),
                }
            }
        }
    }
    // MicrolinkGuard stops/destroys the session on the way out.
    Ok(())
}

/// One plain HTTP/1.0 GET via tokio's TCP stack. Traffic reaches the peer
/// through the WireGuard tunnel as long as microlink's lwIP routes are in
/// place. Reports current memory usage as query params. Returns true if the
/// request was sent and a response arrived.
async fn http_get_once() -> bool {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let attempt = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let mut stream = tokio::net::TcpStream::connect((PROBE_IP, PROBE_PORT)).await?;
        // Report memory usage: total free, historic minimum, internal RAM,
        // and PSRAM free sizes in bytes.
        // SAFETY: plain ESP-IDF heap queries.
        let (free_heap, min_free, internal_free, psram_free) = unsafe {
            (
                esp_idf_svc::sys::esp_get_free_heap_size(),
                esp_idf_svc::sys::esp_get_minimum_free_heap_size(),
                esp_idf_svc::sys::heap_caps_get_free_size(esp_idf_svc::sys::MALLOC_CAP_INTERNAL),
                esp_idf_svc::sys::heap_caps_get_free_size(esp_idf_svc::sys::MALLOC_CAP_SPIRAM),
            )
        };
        let request = format!(
            "GET /?free_heap={free_heap}&min_free={min_free}\
&internal_free={internal_free}&psram_free={psram_free} \
HTTP/1.0\r\nHost: {PROBE_IP}:{PROBE_PORT}\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await?;
        let mut buf = [0u8; 256];
        let received = stream.read(&mut buf).await?;
        Ok::<bool, std::io::Error>(received > 0)
    })
    .await;
    matches!(attempt, Ok(Ok(true)))
}

fn span(text: &str, accent: bool) -> crate::watch_ui::OtaTextSpan<'_> {
    crate::watch_ui::OtaTextSpan { text, accent }
}

/// Snapshot of our VPN IP and the peer table (online peers first).
fn collect_tailnet_info(
    handle: &crate::microlink::MicrolinkGuard,
) -> (String, i32, Vec<(String, bool)>) {
    let mut vpn_ip_buf = [0u8; 16];
    // SAFETY: valid handle and buffer of sufficient size.
    unsafe {
        let ip = ml::microlink_get_vpn_ip(handle.0);
        ml::microlink_ip_to_str(ip, vpn_ip_buf.as_mut_ptr() as *mut _);
    }
    let vpn_ip_str = std::ffi::CStr::from_bytes_until_nul(&vpn_ip_buf)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let nodes = crate::microlink::tailnet_nodes(handle);
    let peers: Vec<(String, bool)> = nodes
        .into_iter()
        .map(|node| (node.hostname, node.online))
        .collect();
    (vpn_ip_str, peers.len() as i32, peers)
}

fn count_line(count: i32) -> String {
    if count == 1 {
        "1 device:".to_string()
    } else {
        format!("{count} devices:")
    }
}

/// Page body: our IP, peer count, then names (online in the accent color).
fn build_lines<'a>(
    vpn_ip: &'a str,
    count_line: &'a str,
    peers: &'a [(String, bool)],
) -> Vec<Vec<crate::watch_ui::OtaTextSpan<'a>>> {
    let mut lines: Vec<Vec<crate::watch_ui::OtaTextSpan<'a>>> = Vec::new();
    lines.push(vec![span("My IP ", false), span(vpn_ip, true)]);
    lines.push(vec![span(count_line, false)]);
    lines.push(vec![]);
    for (name, online) in peers {
        if *online {
            lines.push(vec![span(name, true)]);
        } else {
            lines.push(vec![span(name, false), span("offline", false)]);
        }
    }
    lines
}
