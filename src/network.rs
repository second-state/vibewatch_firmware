use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::modem::WifiModemPeripheral,
    wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};
use log::{info, warn};

use crate::setting::WifiCred;

const DEFAULT_SNTP_SERVERS: [&str; 4] = [
    "time.windows.com",
    "time.google.com",
    "ntp.aliyun.com",
    "time.cloudflare.com",
];

/// 以 STA 连接:按 `wifi_list` 顺序逐个直连(顺序=优先级),不做预扫描。
pub fn wifi_connect(
    modem: impl WifiModemPeripheral + 'static,
    sysloop: EspSystemEventLoop,
    wifi_list: &[WifiCred],
) -> anyhow::Result<Box<EspWifi<'static>>> {
    let mut esp_wifi = EspWifi::new(modem, sysloop.clone(), None)?;
    let mut wifi = BlockingWifi::wrap(&mut esp_wifi, sysloop)?;

    wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))?;
    info!("Starting wifi...");
    wifi.start()?;

    let mut last_error = None;
    for (index, cred) in wifi_list
        .iter()
        .filter(|cred| !cred.ssid.is_empty())
        .enumerate()
    {
        let auth_method = if cred.pass.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        };
        info!(
            "Connecting WiFi candidate {}: {} (auth {:?})",
            index, cred.ssid, auth_method
        );
        wifi.set_configuration(&Configuration::Client(ClientConfiguration {
            ssid: cred
                .ssid
                .as_str()
                .try_into()
                .expect("Could not parse the given SSID into WiFi config"),
            password: cred
                .pass
                .as_str()
                .try_into()
                .expect("Could not parse the given password into WiFi config"),
            auth_method,
            ..Default::default()
        }))?;

        info!("Connecting wifi...");
        match wifi.connect().and_then(|_| {
            info!("Waiting for DHCP lease...");
            wifi.wait_netif_up()
        }) {
            Ok(()) => {
                info!("Connected to WiFi {}", cred.ssid);
                last_error = None;
                break;
            }
            Err(e) => {
                warn!("WiFi candidate {} ({}) failed: {e:?}", index, cred.ssid);
                last_error = Some(e);
                let _ = wifi.disconnect();
            }
        }
    }

    if let Some(e) = last_error {
        return Err(anyhow::anyhow!(
            "all configured WiFi candidates failed: {e:?}"
        ));
    }
    if wifi_list.iter().all(|cred| cred.ssid.is_empty()) {
        return Err(anyhow::anyhow!("wifi_list is empty"));
    }

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
    info!("Wifi DHCP info: {:?}", ip_info);
    enable_wifi_power_save()?;

    Ok(Box::new(esp_wifi))
}

pub async fn sync_time_with_ui(
    gui: &mut crate::ui::UI,
    touch: &mut crate::touch::TouchInput,
) -> anyhow::Result<()> {
    loop {
        match sync_time(gui).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!("SNTP sync failed after WiFi connect: {e:?}");
                let items = vec![("Retry".to_string(), false), ("Skip".to_string(), false)];
                let index =
                    crate::ui::select_menu_item(gui, touch, "Time sync failed", &items).await?;
                if index == 1 {
                    warn!("SNTP sync skipped by user");
                    return Ok(());
                }
            }
        }
    }
}

async fn sync_time(gui: &mut crate::ui::UI) -> anyhow::Result<()> {
    use esp_idf_svc::sntp::{EspSntp, OperatingMode, SntpConf, SyncMode, SyncStatus};

    info!("SNTP sync ({} servers)", DEFAULT_SNTP_SERVERS.len());
    let conf = SntpConf {
        servers: DEFAULT_SNTP_SERVERS,
        operating_mode: OperatingMode::Poll,
        sync_mode: SyncMode::Immediate,
    };
    let ntp = EspSntp::new(&conf)?;
    for i in 0..30 {
        let dots = i % 3 + 1;
        gui.show_status(format!("Sync time{}", ".".repeat(dots)), "")
            .await
            .ok();
        if ntp.get_sync_status() == SyncStatus::Completed {
            info!("SNTP sync completed");
            gui.show_status("Sync time...", "Done").await.ok();
            return Ok(());
        }
        info!("sntp waiting ({})", i);
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    Err(anyhow::anyhow!("SNTP sync timeout"))
}

fn enable_wifi_power_save() -> anyhow::Result<()> {
    let code = unsafe {
        esp_idf_svc::sys::esp_wifi_set_ps(esp_idf_svc::sys::wifi_ps_type_t_WIFI_PS_MAX_MODEM)
    };
    if code == esp_idf_svc::sys::ESP_OK as i32 {
        info!("WiFi power save enabled: WIFI_PS_MAX_MODEM");
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "esp_wifi_set_ps(WIFI_PS_MAX_MODEM) failed: esp_err_t={code}"
        ))
    }
}
