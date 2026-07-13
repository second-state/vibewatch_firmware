//! BLE 配网(照 vibekeys `bt_wifi_mode` 裁剪:只保留 WiFi 列表 + MQTT broker,去掉键盘/ASR/壁纸)。
//!
//! 协议(沿用 vibekeys 的 UUID,手机端 setup.html 可复用):
//! - CONFIG 特征值(READ|WRITE):写 = JSON 部分更新 `{"wifi_list":[{ssid,pass}...], "server_url":...}`,
//!   顺序即连接优先级;读 = 当前整份快照。
//! - RESET 特征值(WRITE):写入 `b"RESET"` 触发重启应用配置。
//!
//! NVS("setting" namespace):`wifi_list` 存整份 JSON,`server_url` 存字符串。
//! 配网阶段不起 WiFi(modem 留给重启后 STA),规避 BLE/WiFi 共存。

use std::sync::{Arc, Mutex};

use esp32_nimble::{
    utilities::BleUuid, uuid128, BLEAdvertisementData, BLEDevice, BLEService, NimbleProperties,
};
use esp_idf_svc::nvs::EspDefaultNvs;
use serde::{Deserialize, Serialize};

// WifiCred / Setting / NVS 存储已抽到 crate::setting(无 BLE 依赖,OTA 救援固件共用)。
use crate::setting::{Setting, WifiCred, MAX_WIFI_CREDS};

pub const SERVICE_ID: BleUuid = uuid128!("623fa3e2-631b-4f8f-a6e7-a7b09c03e7e0");
const CONFIG_ID: BleUuid = uuid128!("cef520a9-bcb5-4fc6-87f7-82804eee2b20");
const RESET_ID: BleUuid = uuid128!("f0e1d2c3-b4a5-6789-0abc-def123456789");

/// CONFIG 写载荷:部分配置,缺失字段保持原状。
#[derive(Debug, Deserialize)]
struct ConfigPatch {
    wifi_list: Option<Vec<WifiCred>>,
    server_url: Option<String>,
}

/// CONFIG 读快照:整份 wifi_list + server_url。
#[derive(Serialize)]
struct ConfigSnapshot<'a> {
    wifi_list: &'a [WifiCred],
    server_url: &'a str,
}

#[derive(Debug)]
pub enum BtEvent {
    Reset,
}

/// 注册配网 GATT 服务(CONFIG + RESET)。
pub fn new_setting_service(
    service: &mut BLEService,
    setting: Arc<Mutex<(Setting, EspDefaultNvs)>>,
    evt_tx: tokio::sync::mpsc::Sender<BtEvent>,
) -> anyhow::Result<()> {
    let setting_r = setting.clone();
    let setting_w = setting.clone();

    let ch =
        service.create_characteristic(CONFIG_ID, NimbleProperties::READ | NimbleProperties::WRITE);
    ch.lock()
        .on_read(move |c, _| {
            let s = setting_r.lock().unwrap();
            let snap = ConfigSnapshot {
                wifi_list: &s.0.wifi_list,
                server_url: s.0.server_url.as_str(),
            };
            if let Ok(json) = serde_json::to_string(&snap) {
                c.set_value(json.as_bytes());
            }
        })
        .on_write(move |args| {
            log::info!("BLE config write: {:?}", args.recv_data());
            let Ok(payload) = std::str::from_utf8(args.recv_data()) else {
                return;
            };
            let Ok(patch) = serde_json::from_str::<ConfigPatch>(payload) else {
                log::warn!("BLE config: invalid JSON, ignored");
                return;
            };
            let mut s = setting_w.lock().unwrap();
            if let Some(mut list) = patch.wifi_list {
                if list.len() > MAX_WIFI_CREDS {
                    log::warn!(
                        "wifi_list has {} entries, truncating to {}",
                        list.len(),
                        MAX_WIFI_CREDS
                    );
                    list.truncate(MAX_WIFI_CREDS);
                }
                s.0.wifi_list = list;
                let l = s.0.wifi_list.clone();
                if let Err(e) = Setting::save_wifi_list(&mut s.1, &l) {
                    log::error!("Failed to save wifi_list: {:?}", e);
                }
            }
            if let Some(url) = patch.server_url {
                s.0.server_url = url.clone();
                if let Err(e) = s.1.set_str("server_url", &url) {
                    log::error!("Failed to save server_url: {:?}", e);
                }
            }
        });

    let reset = service.create_characteristic(RESET_ID, NimbleProperties::WRITE);
    reset.lock().on_write(move |args| {
        if args.recv_data() == b"RESET" {
            let _ = evt_tx.blocking_send(BtEvent::Reset);
        } else {
            log::warn!("BLE reset: invalid payload, ignored");
        }
    });

    Ok(())
}

/// 启动 BLE 配网:广播 "Watch",阻塞等手机写完配置 + 写 RESET,然后重启。
/// `nvs` 被 move 进 BLE 回调;函数正常情况下由 restart 收尾,不返回。
pub fn provision(nvs: EspDefaultNvs) -> anyhow::Result<()> {
    let setting = Setting::load_from_nvs(&nvs)?;
    let setting_arc = Arc::new(Mutex::new((setting, nvs)));

    BLEDevice::set_device_name("Watch")?;
    let ble = BLEDevice::take();
    let server = ble.get_server();
    let svc = server.create_service(SERVICE_ID);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<BtEvent>(8);
    {
        let mut lock = svc.lock();
        new_setting_service(&mut lock, setting_arc, tx)?;
    }
    server.start()?;

    let adv = ble.get_advertising();
    let mut data = BLEAdvertisementData::new();
    data.name("Watch");
    data.add_service_uuid(SERVICE_ID);
    adv.lock().scan_response(true).set_data(&mut data)?;
    adv.lock().start()?;
    log::info!("BLE provisioning: advertising as \"Watch\", waiting for config + RESET...");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        while let Some(ev) = rx.recv().await {
            if matches!(ev, BtEvent::Reset) {
                log::info!("BLE RESET received, restarting to apply config");
                break;
            }
        }
    });

    std::thread::sleep(std::time::Duration::from_secs(1)); // 给 BLE 回调收尾、手机收到写响应
    esp_idf_svc::hal::reset::restart();
}
