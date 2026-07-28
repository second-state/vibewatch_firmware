use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::modem::WifiModemPeripheral,
    wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};
use log::{info, warn};
use serde::Deserialize;

use crate::setting::WifiCred;

const DEFAULT_SNTP_SERVERS: [&str; 4] = [
    "time.windows.com",
    "time.google.com",
    "ntp.aliyun.com",
    "time.cloudflare.com",
];
const TIMEZONE_BY_IP_URL: &str =
    "http://ip-api.com/json/?fields=status,message,timezone,offset,query";
const TIMEZONE_BY_IP_ATTEMPTS: usize = 3;
const TIMEZONE_BY_IP_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(500);
const HTTP_DATE_SYNC_THRESHOLD: std::time::Duration = std::time::Duration::from_secs(15);
const TIME_SYNC_STATUS_HOLD: std::time::Duration = std::time::Duration::from_millis(2000);

/// 以 STA 连接:按 `wifi_list` 顺序逐个直连(顺序=优先级),不做预扫描。
pub struct WifiManager {
    wifi: BlockingWifi<EspWifi<'static>>,
}

impl WifiManager {
    pub fn new(
        modem: impl WifiModemPeripheral + 'static,
        sysloop: EspSystemEventLoop,
    ) -> anyhow::Result<Self> {
        let esp_wifi = EspWifi::new(modem, sysloop.clone(), None)?;
        let mut wifi = BlockingWifi::wrap(esp_wifi, sysloop)?;

        wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()))?;
        Ok(Self { wifi })
    }

    pub fn connect(&mut self, wifi_list: &[WifiCred]) -> anyhow::Result<()> {
        if self.wifi.is_connected().unwrap_or(false) {
            info!("WiFi already connected");
            return Ok(());
        }
        if !self.wifi.is_started().unwrap_or(false) {
            info!("Starting wifi...");
            self.wifi.start()?;
        }

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
            self.wifi
                .set_configuration(&Configuration::Client(ClientConfiguration {
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
            match self.wifi.connect().and_then(|_| {
                info!("Waiting for DHCP lease...");
                self.wifi.wait_netif_up()
            }) {
                Ok(()) => {
                    info!("Connected to WiFi {}", cred.ssid);
                    last_error = None;
                    break;
                }
                Err(e) => {
                    warn!("WiFi candidate {} ({}) failed: {e:?}", index, cred.ssid);
                    last_error = Some(e);
                    let _ = self.wifi.disconnect();
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

        let ip_info = self.wifi.wifi().sta_netif().get_ip_info()?;
        info!("Wifi DHCP info: {:?}", ip_info);
        enable_wifi_power_save()?;

        Ok(())
    }

    pub fn disconnect_and_stop(&mut self) {
        if let Err(e) = self.wifi.disconnect() {
            warn!("WiFi disconnect failed: {e:?}");
        }
        if let Err(e) = self.wifi.stop() {
            warn!("WiFi stop failed: {e:?}");
        }
    }

    pub fn sta_ip(&self) -> anyhow::Result<std::net::Ipv4Addr> {
        Ok(self.wifi.wifi().sta_netif().get_ip_info()?.ip)
    }
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

pub async fn sync_time_and_timezone_with_ui(
    gui: &mut crate::ui::UI,
    touch: &mut crate::touch::TouchInput,
    nvs: &mut esp_idf_svc::nvs::EspDefaultNvs,
) -> anyhow::Result<()> {
    match sync_timezone_from_ip(gui, nvs).await {
        Ok(result) => {
            info!(
                "Timezone synced from IP: {} offset={}s ip={:?}",
                result.timezone.timezone, result.timezone.offset, result.timezone.query
            );
            match http_date_sync_status(result.server_time) {
                HttpDateSyncStatus::NeedsSntp(diff) => {
                    gui.show_status(
                        "Sync time",
                        format!("Time diff {:.1}s\nRunning SNTP...", diff.as_secs_f32()),
                    )
                    .await
                    .ok();
                    tokio::time::sleep(TIME_SYNC_STATUS_HOLD).await;
                    sync_time_with_ui(gui, touch).await?;
                }
                HttpDateSyncStatus::SkipSntp(diff) => {
                    gui.show_status(
                        "Sync time",
                        format!("HTTP Date OK\nDiff {:.1}s", diff.as_secs_f32()),
                    )
                    .await
                    .ok();
                    tokio::time::sleep(TIME_SYNC_STATUS_HOLD).await;
                }
                HttpDateSyncStatus::Unknown => {
                    gui.show_status("Sync time", "HTTP Date unavailable\nRunning SNTP...")
                        .await
                        .ok();
                    tokio::time::sleep(TIME_SYNC_STATUS_HOLD).await;
                    sync_time_with_ui(gui, touch).await?;
                }
            }
        }
        Err(e) => {
            warn!("Timezone sync from IP failed: {e:?}");
            gui.show_status("Sync timezone failed", format!("{e:?}\nRunning SNTP..."))
                .await
                .ok();
            tokio::time::sleep(TIME_SYNC_STATUS_HOLD).await;
            sync_time_with_ui(gui, touch).await?;
        }
    }
    Ok(())
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

#[derive(Debug, Deserialize)]
struct TimezoneByIpResponse {
    status: String,
    message: Option<String>,
    timezone: String,
    offset: i32,
    query: Option<String>,
}

#[derive(Debug)]
struct TimezoneSyncResult {
    timezone: TimezoneByIpResponse,
    server_time: Option<std::time::SystemTime>,
}

async fn sync_timezone_from_ip(
    gui: &mut crate::ui::UI,
    nvs: &mut esp_idf_svc::nvs::EspDefaultNvs,
) -> anyhow::Result<TimezoneSyncResult> {
    let result = fetch_timezone_from_ip_with_retry(gui).await?;
    let timezone = &result.timezone;
    if timezone.status != "success" {
        anyhow::bail!(
            "timezone API returned status={} message={:?}",
            timezone.status,
            timezone.message
        );
    }
    let current_offset = nvs
        .get_i32(crate::setting::TIMEZONE_OFFSET_SECS_KEY)
        .ok()
        .flatten();
    if current_offset == Some(timezone.offset) {
        info!("Timezone offset unchanged: {}s", timezone.offset);
    } else {
        crate::setting::Setting::save_timezone_offset_secs(nvs, timezone.offset)?;
    }
    crate::ui::set_clock_utc_offset_secs(timezone.offset);
    gui.show_status(
        "Timezone",
        format!("{}\nUTC{:+}", timezone.timezone, timezone.offset / 3600),
    )
    .await
    .ok();
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    Ok(result)
}

async fn fetch_timezone_from_ip_with_retry(
    gui: &mut crate::ui::UI,
) -> anyhow::Result<TimezoneSyncResult> {
    let mut last_error = None;
    for attempt in 1..=TIMEZONE_BY_IP_ATTEMPTS {
        gui.show_status(
            "Sync timezone...",
            format!("Attempt {attempt}/{TIMEZONE_BY_IP_ATTEMPTS}"),
        )
        .await
        .ok();
        match fetch_timezone_from_ip() {
            Ok(result) => return Ok(result),
            Err(e) => {
                warn!("Timezone by IP request failed ({attempt}/{TIMEZONE_BY_IP_ATTEMPTS}): {e:?}");
                last_error = Some(e);
                if attempt < TIMEZONE_BY_IP_ATTEMPTS {
                    tokio::time::sleep(TIMEZONE_BY_IP_RETRY_DELAY).await;
                }
            }
        }
    }

    Err(last_error
        .unwrap_or_else(|| anyhow::anyhow!("timezone by IP request failed without error")))
}

fn fetch_timezone_from_ip() -> anyhow::Result<TimezoneSyncResult> {
    info!("Fetching timezone by public IP: {TIMEZONE_BY_IP_URL}");
    let config = esp_idf_svc::http::client::Configuration {
        buffer_size: Some(2048),
        buffer_size_tx: Some(1024),
        timeout: Some(std::time::Duration::from_secs(10)),
        ..Default::default()
    };
    let conn = esp_idf_svc::http::client::EspHttpConnection::new(&config)?;
    let mut client = embedded_svc::http::client::Client::wrap(conn);
    let request = client.get(TIMEZONE_BY_IP_URL)?;
    let mut response = request.submit()?;
    let status = response.status();
    info!("Timezone HTTP status: {status}");
    if status != 200 {
        anyhow::bail!("timezone request failed: HTTP {status}");
    }
    let server_time = response
        .header("date")
        .or_else(|| response.header("Date"))
        .and_then(|date| match parse_http_date(date) {
            Ok(time) => Some(time),
            Err(e) => {
                warn!("Failed to parse HTTP Date header {:?}: {e:?}", date);
                None
            }
        });

    let mut body = Vec::with_capacity(512);
    let mut buf = [0u8; 256];
    loop {
        let n = response.read(&mut buf)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
        if body.len() > 2048 {
            anyhow::bail!("timezone response too large");
        }
    }

    let body = std::str::from_utf8(&body)?;
    log::debug!("Timezone response: {body}");
    Ok(TimezoneSyncResult {
        timezone: serde_json::from_str(body)?,
        server_time,
    })
}

enum HttpDateSyncStatus {
    NeedsSntp(std::time::Duration),
    SkipSntp(std::time::Duration),
    Unknown,
}

fn http_date_sync_status(server_time: Option<std::time::SystemTime>) -> HttpDateSyncStatus {
    let Some(server_time) = server_time else {
        warn!("HTTP Date header missing; falling back to SNTP sync");
        return HttpDateSyncStatus::Unknown;
    };
    let diff = match std::time::SystemTime::now().duration_since(server_time) {
        Ok(diff) => diff,
        Err(e) => e.duration(),
    };
    if diff > HTTP_DATE_SYNC_THRESHOLD {
        info!(
            "System time differs from HTTP Date by {:.2}s; running SNTP",
            diff.as_secs_f32()
        );
        HttpDateSyncStatus::NeedsSntp(diff)
    } else {
        info!(
            "System time differs from HTTP Date by {:.2}s; skipping SNTP",
            diff.as_secs_f32()
        );
        HttpDateSyncStatus::SkipSntp(diff)
    }
}

fn parse_http_date(date: &str) -> anyhow::Result<std::time::SystemTime> {
    let mut parts = date.split_ascii_whitespace();
    let _weekday = parts.next();
    let day = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing day"))?
        .parse::<u32>()?;
    let month = parse_http_month(
        parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("missing month"))?,
    )?;
    let year = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing year"))?
        .parse::<i64>()?;
    let time = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing time"))?;
    let zone = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing zone"))?;
    anyhow::ensure!(zone == "GMT", "unsupported HTTP Date zone: {zone}");

    let mut time_parts = time.split(':');
    let hour = time_parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing hour"))?
        .parse::<u32>()?;
    let minute = time_parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing minute"))?
        .parse::<u32>()?;
    let second = time_parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing second"))?
        .parse::<u32>()?;

    anyhow::ensure!(day >= 1 && day <= 31, "invalid day: {day}");
    anyhow::ensure!(hour < 24, "invalid hour: {hour}");
    anyhow::ensure!(minute < 60, "invalid minute: {minute}");
    anyhow::ensure!(second < 60, "invalid second: {second}");

    let days = days_from_civil(year, month, day);
    let secs = days
        .checked_mul(24 * 60 * 60)
        .and_then(|v| v.checked_add((hour * 3600 + minute * 60 + second) as i64))
        .ok_or_else(|| anyhow::anyhow!("HTTP Date overflow"))?;
    anyhow::ensure!(secs >= 0, "HTTP Date before UNIX epoch");
    Ok(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64))
}

fn parse_http_month(month: &str) -> anyhow::Result<u32> {
    match month {
        "Jan" => Ok(1),
        "Feb" => Ok(2),
        "Mar" => Ok(3),
        "Apr" => Ok(4),
        "May" => Ok(5),
        "Jun" => Ok(6),
        "Jul" => Ok(7),
        "Aug" => Ok(8),
        "Sep" => Ok(9),
        "Oct" => Ok(10),
        "Nov" => Ok(11),
        "Dec" => Ok(12),
        _ => Err(anyhow::anyhow!("invalid month: {month}")),
    }
}

fn days_from_civil(mut year: i64, month: u32, day: u32) -> i64 {
    year -= (month <= 2) as i64;
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month = month as i64;
    let day = day as i64;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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
