//! OTA mode used by the main firmware Settings menu.

use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::reset::restart,
    http::server::{Configuration as HttpServerConf, EspHttpServer, Method},
    io::Write,
    ota::EspOta,
};

/// URL used by the "download latest" OTA action.
///
/// Defaults to this watch firmware's stable GitHub release asset. CI or local
/// release builds can override it with `VIBEWATCH_OTA_URL`, for example to pin a
/// prerelease tag that GitHub's `releases/latest` would not return.
const DEFAULT_OTA_URL: &str =
    "https://github.com/second-state/vibewatch_firmware/releases/latest/download/vibewatch_ota.bin";

pub const OTA_DOWNLOAD_URL: &str = match option_env!("VIBEWATCH_OTA_URL") {
    Some(url) => url,
    None => match option_env!("VIBEKEYS_OTA_URL") {
        Some(url) => url,
        None => DEFAULT_OTA_URL,
    },
};

static OTA_INDEX_HTML: &str = include_str!("../assets/ota_index.html");

enum OtaEvent {
    DataChunk(Vec<u8>),
    Complete,
    DownloadLatest,
}

pub async fn run<M>(
    modem: M,
    sysloop: EspSystemEventLoop,
    setting: &crate::setting::Setting,
    gui: &mut crate::ui::UI,
    touch: &mut crate::touch::TouchInput,
) -> anyhow::Result<()>
where
    M: esp_idf_svc::hal::modem::WifiModemPeripheral + 'static,
{
    gui.show_status("OTA Mode", "Connecting WiFi...").await.ok();

    let wifi = crate::network::wifi_connect(modem, sysloop, &setting.wifi_list);
    if let Err(e) = wifi.as_ref() {
        log::error!("OTA wifi connect failed: {e:?}");
        gui.show_status("OTA Mode", "Connect WiFi failed\nRestarting...")
            .await
            .ok();
        std::thread::sleep(std::time::Duration::from_secs(3));
        restart();
    }
    let wifi = wifi.unwrap();
    crate::network::sync_time_with_ui(gui, touch).await?;

    let ip = wifi.sta_netif().get_ip_info()?.ip;
    log::info!("OTA: WiFi connected, IP {}", ip);

    let (tx, rx) = std::sync::mpsc::channel::<OtaEvent>();
    let screen_tx = tx.clone();
    let _http_server = ota_http_server(tx)?;
    let ota_worker = std::thread::Builder::new()
        .name("ota-worker".to_string())
        .stack_size(1024 * 24)
        .spawn(move || {
            if let Err(e) = ota_task(rx) {
                log::error!("OTA worker failed: {e:?}");
            }
        })?;

    let items = vec![
        ("Update release".to_string(), false),
        ("Restart".to_string(), false),
    ];
    let title = format!("OTA: {}", ip);
    let index = crate::ui::select_menu_item(gui, touch, &title, &items).await?;
    match index {
        0 => {
            log::info!("OTA screen button selected: download latest");
            gui.show_status("OTA Mode", "Downloading latest...\nDevice will reboot")
                .await
                .ok();
            screen_tx.send(OtaEvent::DownloadLatest).map_err(|e| {
                log::error!("OTA channel closed: {:?}", e);
                anyhow::anyhow!("OTA channel closed: {:?}", e)
            })?;
        }
        1 => {
            log::info!("OTA screen button selected: restart");
            gui.show_status("OTA Mode", "Restarting...").await.ok();
            std::thread::sleep(std::time::Duration::from_millis(500));
            restart();
        }
        _ => unreachable!(),
    }

    let _ = ota_worker.join();
    Ok(())
}

fn ota_http_server(
    tx: std::sync::mpsc::Sender<OtaEvent>,
) -> anyhow::Result<EspHttpServer<'static>> {
    let mut server = EspHttpServer::new(&HttpServerConf {
        stack_size: 10240,
        ..Default::default()
    })?;

    let upload_tx = tx.clone();
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
            upload_tx
                .send(OtaEvent::DataChunk(buf[..n].to_vec()))
                .map_err(|e| {
                    log::error!("OTA channel closed: {:?}", e);
                    anyhow::anyhow!("OTA channel closed: {:?}", e)
                })?;
        }

        upload_tx.send(OtaEvent::Complete).map_err(|e| {
            log::error!("OTA channel closed: {:?}", e);
            anyhow::anyhow!("OTA channel closed: {:?}", e)
        })?;

        let mut resp = request.into_ok_response()?;
        resp.write_all(format!("OTA received: {} bytes", total).as_bytes())?;
        Result::<(), anyhow::Error>::Ok(())
    })?;

    server.fn_handler("/ota/download", Method::Post, move |request| {
        tx.send(OtaEvent::DownloadLatest).map_err(|e| {
            log::error!("OTA channel closed: {:?}", e);
            anyhow::anyhow!("OTA channel closed: {:?}", e)
        })?;

        let mut resp = request.into_ok_response()?;
        resp.write_all(b"Download started. Device will reboot after OTA completes.")?;
        Result::<(), anyhow::Error>::Ok(())
    })?;

    server.fn_handler("/", Method::Get, |req| {
        let html = OTA_INDEX_HTML.replace("{{OTA_DOWNLOAD_URL}}", OTA_DOWNLOAD_URL);
        req.into_ok_response()?.write_all(html.as_bytes())?;
        Result::<(), anyhow::Error>::Ok(())
    })?;

    server.fn_handler("/favicon.ico", Method::Get, |req| {
        req.into_ok_response()?.write_all(&[])?;
        Result::<(), anyhow::Error>::Ok(())
    })?;

    Ok(server)
}

fn ota_task(rx: std::sync::mpsc::Receiver<OtaEvent>) -> anyhow::Result<()> {
    while let Ok(ev) = rx.recv() {
        match ev {
            OtaEvent::DataChunk(data) => return ota_write_upload(rx, data),
            OtaEvent::DownloadLatest => return ota_download_latest(),
            OtaEvent::Complete => {}
        }
    }

    Ok(())
}

fn ota_write_upload(
    rx: std::sync::mpsc::Receiver<OtaEvent>,
    first_chunk: Vec<u8>,
) -> anyhow::Result<()> {
    let mut ota = EspOta::new()?;
    ota.mark_running_slot_valid()?;

    let mut update = ota.initiate_update()?;
    log::info!("OTA upload first chunk: {} bytes", first_chunk.len());
    update.write(&first_chunk)?;

    while let Ok(ev) = rx.recv() {
        match ev {
            OtaEvent::DataChunk(data) => {
                log::info!("OTA chunk: {} bytes", data.len());
                update.write(&data)?;
            }
            OtaEvent::Complete => break,
            OtaEvent::DownloadLatest => {
                log::warn!("Ignoring download request while upload OTA is active");
            }
        }
    }
    update.complete()?;
    log::info!("OTA upload complete, restarting into new firmware");
    restart();
}

fn ota_download_latest() -> anyhow::Result<()> {
    log::info!("OTA download latest from {}", OTA_DOWNLOAD_URL);

    let config = esp_idf_svc::http::client::Configuration {
        buffer_size: Some(16 * 1024),
        buffer_size_tx: Some(1024),
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        timeout: Some(std::time::Duration::from_secs(60)),
        ..Default::default()
    };
    let conn = esp_idf_svc::http::client::EspHttpConnection::new(&config)?;
    let mut client = embedded_svc::http::client::Client::wrap(conn);
    let request = client.get(OTA_DOWNLOAD_URL)?;
    let mut response = request.submit()?;
    let status = response.status();
    log::info!("OTA download HTTP status: {}", status);
    if status != 200 {
        anyhow::bail!("OTA download failed: HTTP {}", status);
    }

    let content_len = response
        .header("content-length")
        .and_then(|value| value.parse::<usize>().ok());

    let mut ota = EspOta::new()?;
    ota.mark_running_slot_valid()?;
    let mut update = match content_len {
        Some(len) => {
            log::info!("OTA download content-length: {} bytes", len);
            ota.initiate_update_with_known_size(len)?
        }
        None => {
            log::warn!("OTA download missing content-length; erasing full OTA partition");
            ota.initiate_update()?
        }
    };

    let mut buf = vec![0u8; 8192];
    let mut total = 0usize;
    loop {
        let n = response.read(&mut buf)?;
        if n == 0 {
            break;
        }
        update.write(&buf[..n])?;
        total += n;
        log::info!("OTA download chunk: {} bytes, total {}", n, total);
    }

    update.complete()?;
    log::info!("OTA download complete: {} bytes, restarting", total);
    restart();
}
