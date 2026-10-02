//! Shared microlink session bring-up. Pages that join the tailnet call
//! [`start_and_wait`] instead of driving the FFI dance themselves.

use esp_idf_svc::sys::microlink as ml;

/// Device name reported to the tailnet (kept for all microlink sessions).
pub(crate) const DEVICE_NAME: &str = "vibewatch";

/// Stops and frees the microlink instance whenever the session ends.
/// The CStrings keep `ml->config`'s `auth_key`/`device_name` pointers valid:
/// `microlink_init` shallow-copies the config (pointers only), so the strings
/// must outlive the session or every Hostinfo report reads freed memory.
/// The strings are never read again; their liveness is the point.
#[allow(dead_code)] // field 1: kept for pointer liveness, intentionally unread
pub(crate) struct MicrolinkGuard(pub(crate) *mut ml::microlink_s, [std::ffi::CString; 2]);

impl Drop for MicrolinkGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: handle came from microlink_init and is valid until stop.
            unsafe {
                ml::microlink_stop(self.0);
                ml::microlink_destroy(self.0);
            }
        }
    }
}

impl MicrolinkGuard {
    /// Kicks a WireGuard handshake toward the peer owning `ip`
    /// ("A.B.C.D"). Use before sending application traffic on a tunnel
    /// that is not yet established; safe to call repeatedly. Returns
    /// ESP_FAIL if the IP cannot be parsed.
    pub(crate) fn trigger_handshake(&self, ip: &str) -> esp_idf_svc::sys::esp_err_t {
        let Ok(ip) = std::ffi::CString::new(ip) else {
            return esp_idf_svc::sys::ESP_FAIL;
        };
        // SAFETY: ip is a valid NUL-terminated string.
        let vpn_ip = unsafe { ml::microlink_parse_ip(ip.as_ptr()) };
        if vpn_ip == 0 {
            return esp_idf_svc::sys::ESP_FAIL;
        }
        // SAFETY: valid handle from microlink_init.
        unsafe { ml::microlink_trigger_handshake(self.0, vpn_ip) }
    }

    /// Moves our DERP mailbox to the home region of the peer owning `ip`:
    /// microlink only delivers relayed packets through our own mailbox, so
    /// talking to a peer in another region needs the mailbox next to it.
    /// Returns the region we rehomed to, or None if the peer (or its home
    /// region) is unknown.
    pub(crate) fn rehome_to_peer(&self, ip: &str) -> Option<u16> {
        let Ok(ip) = std::ffi::CString::new(ip) else {
            return None;
        };
        // SAFETY: ip is a valid NUL-terminated string.
        let vpn_ip = unsafe { ml::microlink_parse_ip(ip.as_ptr()) };
        if vpn_ip == 0 {
            return None;
        }
        // SAFETY: valid handle from microlink_init.
        let region = unsafe { ml::microlink_get_peer_home_region(self.0, vpn_ip) };
        if region == 0 {
            return None;
        }
        unsafe { ml::microlink_rehome_derp(self.0, region) };
        Some(region)
    }
}

/// How often to re-check the microlink state machine while connecting.
const STATE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

fn state_label(state: ml::microlink_state_t) -> &'static str {
    match state {
        ml::microlink_state_t_ML_STATE_IDLE => "Idle",
        ml::microlink_state_t_ML_STATE_WIFI_WAIT => "Waiting for WiFi",
        ml::microlink_state_t_ML_STATE_CONNECTING => "Connecting",
        ml::microlink_state_t_ML_STATE_REGISTERING => "Registering",
        ml::microlink_state_t_ML_STATE_CONNECTED => "Connected",
        ml::microlink_state_t_ML_STATE_RECONNECTING => "Reconnecting",
        _ => "Error",
    }
}

/// Initializes microlink, starts connecting, and polls until CONNECTED,
/// showing live progress on `gui` under `title`. WiFi must already be
/// connected. On any failure the status screen shows the reason for two
/// seconds and `None` is returned; on success returns the session guard
/// to pass to the other `ml::` calls.
pub(crate) async fn start_and_wait(
    auth_key: &str,
    device_name: &str,
    title: &str,
    timeout: std::time::Duration,
    gui: &mut crate::ui::UI,
) -> Option<MicrolinkGuard> {
    let auth_key = std::ffi::CString::new(auth_key).unwrap();
    let device_name = std::ffi::CString::new(device_name).unwrap();
    let config = ml::microlink_config_t {
        auth_key: auth_key.as_ptr(),
        device_name: device_name.as_ptr(),
        enable_derp: true,
        enable_stun: true,
        enable_disco: true,
        max_peers: 8,
        wifi_tx_power_dbm: 0,
        priority_peer_ip: 0,
        disco_heartbeat_ms: 0,
        stun_interval_ms: 0,
        ctrl_watchdog_ms: 0,
    };
    // SAFETY: config is shallow-copied by microlink_init (pointers only), so
    // the CStrings are kept alive inside the guard for the whole session.
    let handle = MicrolinkGuard(
        unsafe { ml::microlink_init(&config) },
        [auth_key, device_name],
    );
    if handle.0.is_null() {
        gui.show_status(title, "Init failed").await.ok();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        return None;
    }

    // SAFETY: valid handle from microlink_init; WiFi is connected.
    if unsafe { ml::microlink_start(handle.0) } != esp_idf_svc::sys::ESP_OK {
        gui.show_status(title, "Start failed").await.ok();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        return None;
    }

    // Wait for registration, showing live state in the notice screen.
    let started = std::time::Instant::now();
    let mut last_label = String::new();
    loop {
        let state = unsafe { ml::microlink_get_state(handle.0) };
        match state {
            ml::microlink_state_t_ML_STATE_CONNECTED => return Some(handle),
            ml::microlink_state_t_ML_STATE_ERROR => break,
            _ => {}
        }
        if started.elapsed() > timeout {
            break;
        }
        let label = state_label(state);
        if label != last_label {
            gui.show_status(title, format!("Connecting...\n{label}"))
                .await
                .ok();
            last_label = label.to_string();
        }
        tokio::time::sleep(STATE_POLL_INTERVAL).await;
    }

    gui.show_status(title, "Connection failed").await.ok();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    None
}

/// One node registered under this account on the tailnet (other than
/// ourselves). `online` means it is currently logged in / reachable.
#[derive(Debug, Clone)]
pub(crate) struct TailnetNode {
    pub hostname: String,
    #[allow(dead_code)] // part of the node data; the page shows hostname only
    pub vpn_ip: std::net::Ipv4Addr,
    pub online: bool,
}

/// Lists every node under the account that the coordination server told us
/// about, each with its current online state (online nodes first).
pub(crate) fn tailnet_nodes(handle: &MicrolinkGuard) -> Vec<TailnetNode> {
    // SAFETY: valid handle from microlink_init.
    let count = unsafe { ml::microlink_get_peer_count(handle.0) }.max(0);
    let mut nodes: Vec<TailnetNode> = Vec::with_capacity(count as usize);
    for index in 0..count {
        let mut info = ml::microlink_peer_info_t {
            vpn_ip: 0,
            hostname: [0; 64],
            public_key: [0; 32],
            online: false,
            direct_path: false,
        };
        // SAFETY: valid handle and index within peer count.
        if unsafe { ml::microlink_get_peer_info(handle.0, index, &mut info) }
            == esp_idf_svc::sys::ESP_OK
        {
            let hostname = std::ffi::CStr::from_bytes_until_nul(&info.hostname)
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            nodes.push(TailnetNode {
                hostname,
                vpn_ip: std::net::Ipv4Addr::from(info.vpn_ip),
                online: info.online,
            });
        }
    }
    nodes.sort_by(|a, b| {
        b.online
            .cmp(&a.online)
            .then_with(|| a.hostname.cmp(&b.hostname))
    });
    nodes
}
