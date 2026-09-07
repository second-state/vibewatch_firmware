//! OTA mode used by the main firmware Settings menu.

use esp_idf_svc::{
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
    None => DEFAULT_OTA_URL,
};

static OTA_INDEX_HTML: &str = include_str!("../assets/ota_index.html");

enum OtaEvent {
    DataChunk(Vec<u8>),
    Complete,
    DownloadLatest,
}

pub async fn run(
    wifi: &mut crate::network::WifiManager,
    setting: &crate::setting::Setting,
    gui: &mut crate::ui::UI,
    touch: &mut crate::touch::TouchInput,
    nvs: &mut esp_idf_svc::nvs::EspDefaultNvs,
) -> anyhow::Result<()> {
    gui.show_status("OTA Mode", "Connecting WiFi...").await.ok();

    if let Err(e) = wifi.connect(&setting.wifi_list) {
        log::error!("OTA wifi connect failed: {e:?}");
        gui.show_status("OTA Mode", "Connect WiFi failed\nRestarting...")
            .await
            .ok();
        std::thread::sleep(std::time::Duration::from_secs(3));
        restart();
    }
    crate::network::sync_time_and_timezone_with_ui(gui, touch, nvs).await?;

    let ip = wifi.sta_ip()?;
    log::info!("OTA: WiFi connected, IP {}", ip);

    let (tx, rx) = std::sync::mpsc::channel::<OtaEvent>();
    let ui_tx = tx.clone();
    let _http_server = ota_http_server(tx)?;
    // Worker reports back only on failure; a successful download reboots inside the worker.
    let (attempt_tx, attempt_rx) = std::sync::mpsc::channel::<()>();
    let _ota_worker = std::thread::Builder::new()
        .name("ota-worker".to_string())
        .stack_size(1024 * 24)
        .spawn(move || {
            while let Ok(ev) = rx.recv() {
                let result = match ev {
                    OtaEvent::DataChunk(first) => ota_write_upload(&rx, first),
                    OtaEvent::DownloadLatest => ota_download_latest(),
                    OtaEvent::Complete => continue,
                };
                if let Err(e) = result {
                    log::error!("OTA attempt failed: {e:?}");
                    let _ = attempt_tx.send(());
                }
            }
        })?;

    let title = "OTA";
    fn span(text: &str, accent: bool) -> crate::watch_ui::OtaTextSpan<'_> {
        crate::watch_ui::OtaTextSpan { text, accent }
    }
    let url = format!("http://{ip}");
    let lines = vec![
        vec![
            span("Open", false),
            span(&url, true),
            span("to upload", false),
        ],
        vec![span("a firmware update.", false)],
        vec![],
        vec![
            span("Or", false),
            span("tap the button below", true),
            span("to", false),
        ],
        vec![span("download the latest release.", false)],
    ];
    let button_label = "Update release";
    let mut hits = gui.display_ota_page(title, &lines, button_label).await?;
    loop {
        match touch.next_gesture().await {
            Some(crate::touch::TouchGesture::Click { start, end }) => {
                if hits.back_hit_pair(start, end) {
                    log::info!("OTA screen: back selected");
                    return Ok(());
                }
                if hits.button_hit_pair(start, end) {
                    log::info!("OTA screen button selected: download latest");
                    gui.show_status("OTA Mode", "Downloading latest...\nDevice will reboot")
                        .await
                        .ok();
                    ui_tx.send(OtaEvent::DownloadLatest).map_err(|e| {
                        log::error!("OTA channel closed: {:?}", e);
                        anyhow::anyhow!("OTA channel closed: {:?}", e)
                    })?;
                    // Success never reaches here: the worker reboots the device.
                    let _ = attempt_rx.recv();
                    gui.show_status("OTA Mode", "Download failed").await.ok();
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    hits = gui.display_ota_page(title, &lines, button_label).await?;
                }
            }
            Some(crate::touch::TouchGesture::Swipe {
                direction: crate::touch::SwipeDirection::Right,
                ..
            }) => {
                log::info!("OTA screen: right swipe, going back to settings");
                return Ok(());
            }
            Some(_) => {}
            None => return Err(anyhow::anyhow!("touch event source closed")),
        }
    }
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

fn ota_write_upload(
    rx: &std::sync::mpsc::Receiver<OtaEvent>,
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

    const MAX_ATTEMPTS: usize = 10;

    let mut ota = EspOta::new()?;
    ota.mark_running_slot_valid()?;
    let mut update = ota.initiate_update()?;

    let mut total: usize = 0;
    let mut full_len: Option<usize> = None;
    let mut buf = vec![0u8; 8192];
    let mut completed = false;

    for attempt in 1..=MAX_ATTEMPTS {
        let range_from = if total == 0 { None } else { Some(total) };
        if attempt > 1 {
            log::warn!(
                "OTA download retry #{attempt}, resuming at {} bytes via Range",
                total
            );
        }

        let config = esp_idf_svc::http::client::Configuration {
            buffer_size: Some(16 * 1024),
            buffer_size_tx: Some(1024),
            crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
            timeout: Some(std::time::Duration::from_secs(60)),
            ..Default::default()
        };
        let conn = esp_idf_svc::http::client::EspHttpConnection::new(&config)?;
        let mut client = embedded_svc::http::client::Client::wrap(conn);

        let range = format!("bytes={total}-");
        let headers = [("Range", range.as_str())];
        let mut response = match range_from {
            Some(_) => {
                let request = client.request(Method::Get, OTA_DOWNLOAD_URL, &headers)?;
                request.submit()?
            }
            None => client.get(OTA_DOWNLOAD_URL)?.submit()?,
        };
        let status = response.status();
        log::info!("OTA download HTTP status: {}", status);

        if status != 200 && !(status == 206 && range_from.is_some()) {
            anyhow::bail!("OTA download failed: HTTP {}", status);
        }
        if status == 200 {
            full_len = response
                .header("content-length")
                .and_then(|value| value.parse::<usize>().ok());
            if let Some(len) = full_len {
                log::info!("OTA download content-length: {} bytes", len);
            } else {
                log::warn!("OTA download missing content-length; will rely on EOF");
            }
        }

        let mut round_eof = true;
        loop {
            match response.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    update.write(&buf[..n])?;
                    total += n;
                    log::info!("OTA download chunk: {} bytes, total {}", n, total);
                }
                Err(e) => {
                    log::warn!("OTA download connection lost at {total} bytes: {e:?}");
                    round_eof = false;
                    break;
                }
            }
        }

        if round_eof && full_len.map_or(true, |len| total >= len) {
            completed = true;
            break;
        }

        drop(response);
        drop(client);
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    if !completed {
        anyhow::bail!("OTA download incomplete after {MAX_ATTEMPTS} attempts: got {total} bytes");
    }

    update.complete()?;
    log::info!("OTA download complete: {} bytes, restarting", total);
    restart();
}
