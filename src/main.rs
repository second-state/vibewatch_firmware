use esp_idf_svc::{eventloop::EspSystemEventLoop, hal::reset::restart};

mod audio;
mod ble_provision;
mod lcd;
mod mqtt;
mod network;
mod new_jpg;
mod power;
mod protocol;
mod remote;
mod setting;
mod ui;
mod util;

const DEFAULT_SNTP_SERVERS: [&str; 4] = [
    "time.windows.com",
    "time.google.com",
    "ntp.aliyun.com",
    "time.cloudflare.com",
];

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    // A/B 双槽 OTA:标记当前启动槽为有效(确认本次正常启动;配合回滚机制)。
    {
        let mut ota = esp_idf_svc::ota::EspOta::new()?;
        ota.mark_running_slot_valid()?;
    }

    let peripherals = esp_idf_svc::hal::peripherals::Peripherals::take().unwrap();
    let sysloop = EspSystemEventLoop::take()?;
    let _fs = esp_idf_svc::io::vfs::MountedEventfs::mount(20)?;
    let partition = esp_idf_svc::nvs::EspDefaultNvsPartition::take()?;
    let nvs = esp_idf_svc::nvs::EspDefaultNvs::new(partition, "setting", true)?;
    let setting = setting::Setting::load_from_nvs(&nvs)?;
    let asr_config = audio::AsrConfig::load_from_nvs(&nvs);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    // === LCD + touch: Waveshare ESP32-S3-Touch-AMOLED-2.06 BSP ===
    lcd::init()?;
    lcd::touch_init()?;
    let (touch_tx, touch_rx) = tokio::sync::mpsc::channel::<lcd::TouchEvent>(16);
    lcd::start_touch_worker(touch_tx)?;
    lcd::set_backlight(30)?;
    power::init()?;
    power::start_power_key_worker();
    // ===

    // === Audio: Waveshare BSP I2S + ES8311 speaker + ES7210 microphone ===
    audio::init()?;
    // ===

    ui::ui_background().ok();
    if let Err(e) = ui::render_terminal_ans_demo() {
        log::warn!("terminal ans demo failed: {e:?}");
    }
    let mut gui = ui::UI::default();

    if setting.need_init() {
        // 首次启动:BLE 配网(手机连蓝牙 "Watch",通过 setup.html 写 WiFi 列表 + MQTT broker)
        gui.state = "Setup".to_string();
        gui.text = "Connect BLE \"Watch\"\nopen setup.html".to_string();
        gui.display_flush().ok();

        if let Err(e) = ble_provision::provision(nvs) {
            log::error!("BLE provision failed: {e:?}");
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        restart();
    }

    // 连 WiFi:从 wifi_list 里挑第一个在范围内的(顺序=优先级)
    gui.state = "Connecting WiFi...".to_string();
    gui.text.clear();
    gui.display_flush().ok();

    let wifi = network::wifi_connect(peripherals.modem, sysloop, &setting.wifi_list);
    if let Err(e) = wifi.as_ref() {
        gui.state = "WiFi failed".to_string();
        gui.text = format!("{e:?}\nReset in 5s...");
        gui.display_flush().ok();
        std::thread::sleep(std::time::Duration::from_secs(5));
        restart();
    }
    let _wifi = wifi.unwrap();
    log::info!("WiFi connected");

    // mqtts:// / Whisper https:// 需要 TLS 证书校验 → 先 SNTP 同步时间。
    if setting.server_url.starts_with("mqtts")
        || asr_config.as_ref().map_or(false, |c| c.requires_tls())
    {
        gui.state = "Syncing time...".to_string();
        gui.text.clear();
        gui.display_flush().ok();
        if let Err(e) = sync_time() {
            log::warn!("sync_time failed: {e:?}");
        }
    }

    // Remote:MQTT 连 vibetty → 进入 session list(停留等输入选会话)
    gui.state = "Connecting MQTT...".to_string();
    gui.text = setting.server_url.clone();
    gui.display_flush().ok();

    let client_id = wifi_sta_mac_client_id();
    let (asr_tx, asr_rx) = std::sync::mpsc::channel::<audio::AsrRequest>();
    if let Err(e) = std::thread::Builder::new()
        .name("asr-worker".to_string())
        .stack_size(1024 * 16)
        .spawn(move || {
            let mut driver = audio::Driver::new()
                .map_err(|e| log::error!("Failed to create audio driver: {e:?}"))
                .ok();
            while let Ok(req) = asr_rx.recv() {
                let result = match driver.as_mut() {
                    Some(driver) => driver.start_asr(
                        &req.config,
                        || {},
                        || req.cancel.load(std::sync::atomic::Ordering::Relaxed),
                    ),
                    None => Err(anyhow::anyhow!("audio driver unavailable")),
                };
                let _ = req.respond.send(result);
            }
            log::info!("ASR worker thread exited");
        })
    {
        log::error!("Failed to spawn ASR worker thread: {e:?}");
    }

    let r = runtime.block_on(remote::run(
        setting.server_url,
        client_id,
        &mut gui,
        touch_rx,
        asr_tx,
        asr_config.as_ref(),
    ));
    log::info!("remote exited: {:?}", r);

    let mut gui = ui::UI::default();
    gui.state = "Disconnected".to_string();
    gui.text = format!("{:?}", r);
    gui.display_flush().ok();
    std::thread::sleep(std::time::Duration::from_secs(5));
    restart();
}

/// SNTP 同步时间:4 个 server 并发查询,谁先回用谁,最多等 15s。
fn sync_time() -> anyhow::Result<()> {
    use esp_idf_svc::sntp::{EspSntp, OperatingMode, SntpConf, SyncMode, SyncStatus};
    log::info!("SNTP sync ({} servers)", DEFAULT_SNTP_SERVERS.len());
    let conf = SntpConf {
        servers: DEFAULT_SNTP_SERVERS,
        operating_mode: OperatingMode::Poll,
        sync_mode: SyncMode::Immediate,
    };
    let ntp = EspSntp::new(&conf)?;
    for i in 0..15 {
        if ntp.get_sync_status() == SyncStatus::Completed {
            log::info!("SNTP sync completed");
            return Ok(());
        }
        log::info!("sntp waiting ({})", i);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    Err(anyhow::anyhow!("SNTP sync timeout"))
}

/// 用 WiFi STA MAC 生成 broker 内唯一的 MQTT client_id(esp_read_mac 直接读 efuse)。
fn wifi_sta_mac_client_id() -> String {
    unsafe {
        let mut mac = [0u8; 6];
        esp_idf_svc::sys::esp_read_mac(
            mac.as_mut_ptr(),
            esp_idf_svc::sys::esp_mac_type_t_ESP_MAC_WIFI_STA,
        );
        format!(
            "watch-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        )
    }
}

/// 切到另一个 OTA 槽并重启:设置页选「OTA Update」后调用 → 进救援固件(ota_0)。
/// 触发入口待接触屏后接入(与 session 选择共用输入层);现在先就位,未接线故 allow(dead_code)。
#[allow(dead_code)]
pub(crate) fn goto_next_firmware() -> anyhow::Result<()> {
    use esp_idf_svc::sys::{esp_ota_get_next_update_partition, esp_ota_set_boot_partition};
    unsafe {
        let partition = esp_ota_get_next_update_partition(std::ptr::null());
        esp_idf_svc::sys::esp!(esp_ota_set_boot_partition(partition))?;
    }
    esp_idf_svc::hal::reset::restart();
}
