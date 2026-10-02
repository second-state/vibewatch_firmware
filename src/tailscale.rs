//! Tailscale page: joins the tailnet via the microlink component and lists
//! the nodes under the account. There is no menu entry — the page is only
//! reached by writing {"tailscale_key": "..."} over BLE provisioning, which
//! stores the key in NVS and hands control to this page. Back button or
//! right swipe exits.

use esp_idf_svc::nvs::EspDefaultNvs;
use esp_idf_svc::sys::microlink as ml;

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// NVS key (in the "setting" namespace) holding the tailnet auth key written
/// over BLE.
pub(crate) const NVS_KEY: &str = "tailscale_key";

/// Reads the stored tailnet auth key, if one has been provisioned.
pub(crate) fn stored_auth_key(nvs: &EspDefaultNvs) -> Option<String> {
    // str_len counts the NUL terminator; get_str returns the string WITHOUT
    // it — use its return value, not the raw buffer (whose last byte is NUL).
    let len = nvs.str_len(NVS_KEY).ok()??;
    if len <= 1 {
        return None;
    }
    let mut buffer = vec![0u8; len];
    nvs.get_str(NVS_KEY, &mut buffer)
        .ok()?
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
}

pub async fn run(
    wifi: &mut crate::network::WifiManager,
    nvs: &mut EspDefaultNvs,
    setting: &crate::setting::Setting,
    gui: &mut crate::ui::UI,
    touch: &mut crate::touch::TouchInput,
) -> anyhow::Result<()> {
    crate::util::log_heap_usage("page -> tailscale");
    let Some(auth_key) = stored_auth_key(nvs) else {
        gui.show_status("Tailscale", "No Tailscale key\n(provision via BLE)")
            .await
            .ok();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        return Ok(());
    };

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

    // The DERP relay speaks TLS, which needs a trustworthy clock (a fresh
    // boot starts at 1970). Sync before microlink; a user-skipped sync is
    // tolerated — DERP may just fail.
    let synced = crate::network::sync_time_and_timezone_with_ui(gui, touch, nvs).await?;
    if !synced {
        log::warn!("Tailscale: time sync skipped; DERP TLS may fail");
    }

    let Some(handle) = crate::microlink::start_and_wait(
        &auth_key,
        crate::microlink::DEVICE_NAME,
        "Tailscale",
        CONNECT_TIMEOUT,
        gui,
    )
    .await
    else {
        return Ok(());
    };

    // Node list page: refresh once per second (online states change as peers
    // come and go), exit on back / right swipe. The snapshot values live in
    // this scope because the page lines borrow them.
    let (mut vpn_ip_str, mut count_text, mut nodes) = tailnet_snapshot(&handle);
    let lines = build_lines(&vpn_ip_str, &count_text, &nodes);
    let mut hits = gui.display_ota_page("Tailscale", &lines, "").await?;
    let mut refresh = tokio::time::interval(std::time::Duration::from_secs(1));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = refresh.tick() => {
                (vpn_ip_str, count_text, nodes) = tailnet_snapshot(&handle);
                let lines = build_lines(&vpn_ip_str, &count_text, &nodes);
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
    // Microlink teardown (stop + destroy) takes a while; acknowledge the
    // exit immediately with a notice box so the node list doesn't linger
    // as if the gesture was missed.
    gui.show_status("Tailscale", "Shutting down...").await.ok();
    // MicrolinkGuard stops/destroys the session on the way out.
    Ok(())
}

/// Page body: our VPN IP, node count, then names (online in the accent
/// color, offline dimmed with a marker). Borrows the snapshot values so they
/// must outlive the rendered page.
fn build_lines<'a>(
    vpn_ip_str: &'a str,
    count_text: &'a str,
    nodes: &'a [crate::microlink::TailnetNode],
) -> Vec<Vec<crate::watch_ui::OtaTextSpan<'a>>> {
    fn span(text: &str, accent: bool) -> crate::watch_ui::OtaTextSpan<'_> {
        crate::watch_ui::OtaTextSpan { text, accent }
    }

    let mut lines: Vec<Vec<crate::watch_ui::OtaTextSpan<'_>>> = Vec::new();
    lines.push(vec![span("My IP ", false), span(vpn_ip_str, true)]);
    lines.push(vec![span(count_text, false)]);
    lines.push(vec![]);
    for node in nodes {
        if node.online {
            lines.push(vec![span(&node.hostname, true)]);
        } else {
            lines.push(vec![span(&node.hostname, false), span("offline", false)]);
        }
    }
    lines
}

/// Snapshot of our VPN IP, the "N devices:" line, and the peer table
/// (online peers first).
fn tailnet_snapshot(
    handle: &crate::microlink::MicrolinkGuard,
) -> (String, String, Vec<crate::microlink::TailnetNode>) {
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
    let count_text = if nodes.len() == 1 {
        "1 device:".to_string()
    } else {
        format!("{} devices:", nodes.len())
    };
    (vpn_ip_str, count_text, nodes)
}
