use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::modem::WifiModemPeripheral,
    wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};
use log::info;

use crate::ble_provision::{pick_cred, WifiCred};

/// 以 STA 连接:扫描周围 WiFi,从 `wifi_list` 里挑第一个在范围内的(顺序=优先级)连上。
pub fn wifi_connect(
    modem: impl WifiModemPeripheral + 'static,
    sysloop: EspSystemEventLoop,
    wifi_list: &[WifiCred],
) -> anyhow::Result<Box<EspWifi<'static>>> {
    let mut esp_wifi = EspWifi::new(modem, sysloop.clone(), None)?;
    let mut wifi = BlockingWifi::wrap(&mut esp_wifi, sysloop)?;

    // 先以默认 Client 起来,才能 scan。
    wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))?;
    info!("Starting wifi...");
    wifi.start()?;

    info!("Scanning...");
    let scan_list: Vec<String> = wifi
        .scan()?
        .into_iter()
        .map(|a| a.ssid.as_str().to_string())
        .collect();

    let cred = pick_cred(&scan_list, wifi_list).ok_or_else(|| {
        anyhow::anyhow!(
            "no configured WiFi in range (scan saw {} networks)",
            scan_list.len()
        )
    })?;

    let auth_method = if cred.pass.is_empty() {
        AuthMethod::None
    } else {
        AuthMethod::WPA2Personal
    };
    info!("Connecting to {} (auth {:?})", cred.ssid, auth_method);
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
    wifi.connect()?;
    info!("Waiting for DHCP lease...");
    wifi.wait_netif_up()?;

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
    info!("Wifi DHCP info: {:?}", ip_info);

    Ok(Box::new(esp_wifi))
}
