use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::peripheral,
    http::{client::EspHttpConnection, Method},
    io::{Read, Write},
    wifi::{AuthMethod, BlockingWifi, EspWifi},
};
use log::info;

pub fn wifi(
    ssid: &str,
    pass: &str,
    modem: impl peripheral::Peripheral<P = esp_idf_svc::hal::modem::Modem> + 'static,
    sysloop: EspSystemEventLoop,
) -> anyhow::Result<Box<EspWifi<'static>>> {
    let mut auth_method = AuthMethod::WPA2Personal;
    if ssid.is_empty() {
        anyhow::bail!("Missing WiFi name")
    }
    if pass.is_empty() {
        auth_method = AuthMethod::None;
        info!("Wifi password is empty");
    }
    let mut esp_wifi = EspWifi::new(modem, sysloop.clone(), None)?;

    let mut wifi = BlockingWifi::wrap(&mut esp_wifi, sysloop)?;

    wifi.set_configuration(&esp_idf_svc::wifi::Configuration::Client(
        esp_idf_svc::wifi::ClientConfiguration::default(),
    ))?;

    info!("Starting wifi...");

    wifi.start()?;

    info!("Scanning...");

    let ap_infos = wifi.scan()?;

    let ours = ap_infos.into_iter().find(|a| a.ssid == ssid);

    let channel = if let Some(ours) = ours {
        info!(
            "Found configured access point {} on channel {}",
            ssid, ours.channel
        );
        Some(ours.channel)
    } else {
        info!(
            "Configured access point {} not found during scanning, will go with unknown channel",
            ssid
        );
        None
    };

    wifi.set_configuration(&esp_idf_svc::wifi::Configuration::Client(
        esp_idf_svc::wifi::ClientConfiguration {
            ssid: ssid
                .try_into()
                .expect("Could not parse the given SSID into WiFi config"),
            password: pass
                .try_into()
                .expect("Could not parse the given password into WiFi config"),
            channel,
            auth_method,
            ..Default::default()
        },
    ))?;

    info!("Connecting wifi...");

    wifi.connect()?;

    info!("Waiting for DHCP lease...");

    wifi.wait_netif_up()?;

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;

    info!("Wifi DHCP info: {:?}", ip_info);

    Ok(Box::new(esp_wifi))
}

#[allow(unused)]
pub fn http_get(url: &str) -> anyhow::Result<EspHttpConnection> {
    let configuration = esp_idf_svc::http::client::Configuration::default();
    let mut conn = EspHttpConnection::new(&configuration)?;
    conn.initiate_request(Method::Get, url, &[])?;

    conn.initiate_response()?;

    Ok(conn)
}

#[allow(unused)]
pub fn http_post(url: &str, data: &[u8]) -> anyhow::Result<EspHttpConnection> {
    let configuration = esp_idf_svc::http::client::Configuration::default();
    let len = data.len().to_string();
    let mut conn = EspHttpConnection::new(&configuration)?;
    conn.initiate_request(Method::Post, url, &[("Content-Length", &len)])?;

    let mut offset = 0;

    while offset < data.len() {
        offset += conn.write(&data[offset..])?;
        log::info!("Wrote {} bytes", offset);
    }

    conn.initiate_response()?;

    Ok(conn)
}

const CHANNEL: u8 = 11;
pub const SSID: &str = "GaiaAI";
const STACK_SIZE: usize = 10240;
static INDEX_HTML: &str = include_str!("../assets/index.html");

#[derive(Debug, serde::Deserialize)]
pub struct FormData {
    pub wifi_username: String,
    pub wifi_password: String,
    pub server_url: String,
}

pub fn http_setting_server() -> anyhow::Result<FormData> {
    let server_configuration = esp_idf_svc::http::server::Configuration {
        stack_size: STACK_SIZE,
        max_resp_headers: 16,
        ..Default::default()
    };
    let mut server = esp_idf_svc::http::server::EspHttpServer::new(&server_configuration)?;

    let (tx, rx) = std::sync::mpsc::sync_channel(1);

    let _ = server.fn_handler("/", Method::Get, |req| {
        req.into_ok_response()?
            .write_all(INDEX_HTML.as_bytes())
            .map(|_| ())
    });
    let _ = server.fn_handler::<anyhow::Error, _>("/setting", Method::Post, move |mut req| {
        let len = req
            .header("Content-Length")
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);

        log::info!(
            "Received request with Content-Length: {}",
            req.header("Content-Length").unwrap_or("0")
        );
        log::info!("Content-Length: {}", len);

        let mut buf = vec![0; len];
        req.read_exact(&mut buf)?;
        log::info!("buf string: {:?}", String::from_utf8(buf.clone()));
        let mut resp = req.into_ok_response()?;

        if let Ok(form) = serde_json::from_slice::<FormData>(&buf) {
            log::info!("Received form data: {:?}", form);
            write!(
                resp,
                "Hello. Your wifi name is {}, password is {}, server url is {}",
                form.wifi_username, form.wifi_password, form.server_url
            )?;
            tx.send(form).unwrap_or_else(|_| {
                log::error!("Failed to send form data");
            });
        } else {
            resp.write_all("JSON error".as_bytes())?;
        }

        Ok(())
    });

    let r = rx
        .recv()
        .map_err(|_| anyhow::anyhow!("Failed to receive form data"));
    log::info!("Received form data: {:?}", r);
    r
}

pub fn wifi_http_server(
    modem: impl peripheral::Peripheral<P = esp_idf_svc::hal::modem::Modem> + 'static,
    sys_loop: EspSystemEventLoop,
) -> anyhow::Result<FormData> {
    log::info!("Starting wifi http server...");
    let mut wifi = BlockingWifi::wrap(EspWifi::new(modem, sys_loop.clone(), None)?, sys_loop)?;
    let wifi_configuration = esp_idf_svc::wifi::Configuration::AccessPoint(
        esp_idf_svc::wifi::AccessPointConfiguration {
            ssid: SSID.try_into().unwrap(),
            ssid_hidden: false,
            auth_method: AuthMethod::None,
            channel: CHANNEL,
            ..Default::default()
        },
    );

    wifi.set_configuration(&wifi_configuration)?;

    wifi.start()?;
    wifi.wait_netif_up()?;

    http_setting_server()
}
