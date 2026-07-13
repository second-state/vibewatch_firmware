//! OTA 救援固件(独立 `[[bin]]`,烧到 ota_0 槽)。
//!
//! 主固件 `goto_next_firmware()` 把启动槽切到 ota_0 后重启 → 进入本固件:
//! 复位屏 → 连 WiFi(复用 wifi_list)→ 起 HTTP server → 屏上显示 `http://<ip>/`
//! → 浏览器拖入 .bin → `EspOta` 写到另一槽(ota_1)→ complete → 重启进新主固件。
//!
//! 手表无物理按键:进本槽即意味着要 OTA,不做 Accept/ESC 确认(将来接触屏可加)。
//! 复用本 crate 的 lcd / exio / network / setting / ui 模块(不拉 BLE / MQTT / JPEG)。

// 救援固件只用共享模块(setting/ui/new_jpg)的一部分;跨 bin 编译时其余 API 算 dead_code,
// 这里整体 allow,避免噪音(主固件那份编译不受影响)。
#![allow(dead_code)]

mod exio;
mod lcd;
mod network;
mod setting;
mod ui;

use esp_idf_svc::{
    hal::reset::restart,
    http::server::{Configuration as HttpServerConf, EspHttpServer, Method},
    io::Write,
    ota::EspOta,
};

use setting::Setting;

static INDEX_HTML: &str = include_str!("../assets/ota_index.html");

enum OtaEvent {
    DataChunk(Vec<u8>),
    Complete,
}

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = esp_idf_svc::hal::peripherals::Peripherals::take().unwrap();
    let sysloop = esp_idf_svc::eventloop::EspSystemEventLoop::take()?;
    let partition = esp_idf_svc::nvs::EspDefaultNvsPartition::take()?;
    let nvs = esp_idf_svc::nvs::EspDefaultNvs::new(partition, "setting", true)?;
    let setting = Setting::load_from_nvs(&nvs)?;

    // === LCD:与主固件相同的初始化序列(I2C 扩展 IO 复位 SPD2010 + QSPI + 背光) ===
    let mut i2c = exio::i2c_init(
        peripherals.i2c0,
        peripherals.pins.gpio11,
        peripherals.pins.gpio10,
    )?;
    exio::exio_init(&mut i2c)?;
    lcd::spd2010_reset(&mut i2c)?;
    lcd::qspi_init();
    let mut ledc_timer = lcd::backlight_init(peripherals.pins.gpio5.into())?;
    lcd::set_backlight(&mut ledc_timer, 30)?;
    // ===

    ui::ui_background().ok();
    let mut gui = ui::UI::default();
    show(&mut gui, "OTA Mode", "Connecting WiFi...");

    // 连 WiFi:失败就切回主固件,避免卡在救援槽。
    let wifi = network::wifi_connect(peripherals.modem, sysloop, &setting.wifi_list);
    if let Err(e) = wifi.as_ref() {
        log::error!("OTA wifi connect failed: {e:?}");
        show(&mut gui, "OTA Mode", "Connect WiFi failed\nGoto main...");
        std::thread::sleep(std::time::Duration::from_secs(2));
        goto_next_firmware()?;
    }
    let wifi = wifi.unwrap();

    let ip = wifi.sta_netif().get_ip_info()?.ip;
    log::info!("OTA: WiFi connected, IP {}", ip);
    show(
        &mut gui,
        "OTA Mode",
        &format!("Upload firmware at:\nhttp://{}", ip),
    );

    let (tx, rx) = std::sync::mpsc::channel::<OtaEvent>();
    let _http_server = ota_http_server(tx)?;
    ota_task(rx)?; // 正常路径:写完 → complete → restart,不返回

    Ok(())
}

/// 起上传用的 HTTP server:`PUT /ota` 流式收固件,`GET /` 返回上传页。
fn ota_http_server(
    tx: std::sync::mpsc::Sender<OtaEvent>,
) -> anyhow::Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&HttpServerConf {
        stack_size: 10240,
        ..Default::default()
    })?;

    server.fn_handler("/ota", Method::Put, move |mut request| {
        let mut buf = vec![0u8; 4096];
        let mut total = 0usize;

        loop {
            let n = request.read(&mut buf).map_err(|e| {
                log::error!("Failed to read OTA body: {:?}", e);
                anyhow::anyhow!("Failed to read OTA body: {:?}", e)
            })?;
            total += n;
            if n == 0 {
                break;
            }
            tx.send(OtaEvent::DataChunk(buf[..n].to_vec()))
                .map_err(|e| {
                    log::error!("OTA channel closed: {:?}", e);
                    anyhow::anyhow!("OTA channel closed: {:?}", e)
                })?;
        }

        tx.send(OtaEvent::Complete).map_err(|e| {
            log::error!("OTA channel closed: {:?}", e);
            anyhow::anyhow!("OTA channel closed: {:?}", e)
        })?;

        let mut resp = request.into_ok_response()?;
        resp.write_all(format!("OTA received: {} bytes", total).as_bytes())?;
        Result::<(), anyhow::Error>::Ok(())
    })?;

    server.fn_handler("/", Method::Get, |req| {
        req.into_ok_response()?.write_all(INDEX_HTML.as_bytes())?;
        Result::<(), anyhow::Error>::Ok(())
    })?;

    Ok(server)
}

/// 收完所有分片 → 写到另一槽 → complete → 重启进新固件。
fn ota_task(rx: std::sync::mpsc::Receiver<OtaEvent>) -> anyhow::Result<()> {
    let mut ota = EspOta::new()?;
    ota.mark_running_slot_valid()?;

    let mut update = ota.initiate_update()?;
    while let Ok(ev) = rx.recv() {
        match ev {
            OtaEvent::DataChunk(data) => {
                log::info!("OTA chunk: {} bytes", data.len());
                update.write(&data)?;
            }
            OtaEvent::Complete => break,
        }
    }
    update.complete()?;
    log::info!("OTA complete, restarting into new firmware");
    restart();
}

/// 切到另一个 OTA 槽并重启(救援固件里用于 WiFi 连不上时退回主固件)。
fn goto_next_firmware() -> anyhow::Result<()> {
    use esp_idf_svc::sys::{esp_ota_get_next_update_partition, esp_ota_set_boot_partition};
    unsafe {
        let partition = esp_ota_get_next_update_partition(std::ptr::null());
        esp_idf_svc::sys::esp!(esp_ota_set_boot_partition(partition))?;
    }
    restart();
}

fn show(gui: &mut ui::UI, state: &str, text: &str) {
    gui.state = state.to_string();
    gui.text = text.to_string();
    let _ = gui.display_flush();
}
