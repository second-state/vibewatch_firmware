//! BLE 配网(照 vibekeys `bt_wifi_mode` 裁剪:保留 WiFi 列表 + MQTT broker + ASR)。
//!
//! 协议(沿用 vibekeys 的 UUID,手机端 setup.html 可复用):
//! - CONFIG 特征值(READ|WRITE):写 = JSON 部分更新
//!   `{"wifi_list":[{ssid,pass}...], "server_url":..., "asr_config":...}`,
//!   顺序即连接优先级;读 = 当前整份快照。
//! - AUDIO 特征值(WRITE):第一包 little-endian u32 PCM byte length,后续包 PCM bytes;最大 256KB。
//! - BACKGROUND 特征值(WRITE):第一包 little-endian u32 GIF byte length,后续包 GIF bytes;最大 128KB。
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
const AUDIO_ID: BleUuid = uuid128!("a8ef1f04-b6e8-4d7b-bd40-6f2ebcbb2f49");
const BACKGROUND_ID: BleUuid = uuid128!("3d5a6f16-6e71-4f17-9e61-2f4c1b8f0b23");
const RESET_ID: BleUuid = uuid128!("f0e1d2c3-b4a5-6789-0abc-def123456789");
const MAX_AUDIO_BYTES: usize = 256 * 1024;

/// CONFIG 写载荷:部分配置,缺失字段保持原状。
#[derive(Debug, Deserialize)]
struct ConfigPatch {
    wifi_list: Option<Vec<WifiCred>>,
    server_url: Option<String>,
    asr_config: Option<serde_json::Value>,
}

struct AudioUpload {
    expected_size: usize,
    data: Vec<u8>,
}

struct BackgroundUpload {
    expected_size: usize,
    data: Vec<u8>,
}

/// CONFIG 读快照:整份 wifi_list + server_url + asr_config。
#[derive(Serialize)]
struct ConfigSnapshot<'a> {
    wifi_list: &'a [WifiCred],
    server_url: &'a str,
    asr_config: Option<serde_json::Value>,
    background_gif_size: Option<usize>,
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
    let setting_audio = setting.clone();
    let setting_background = setting.clone();
    let audio_upload = Arc::new(Mutex::new(None::<AudioUpload>));
    let background_upload = Arc::new(Mutex::new(None::<BackgroundUpload>));

    let ch =
        service.create_characteristic(CONFIG_ID, NimbleProperties::READ | NimbleProperties::WRITE);
    ch.lock()
        .on_read(move |c, _| {
            let s = setting_r.lock().unwrap();
            let asr_config = crate::audio::AsrConfig::load_from_nvs(&s.1)
                .and_then(|c| serde_json::to_value(c).ok());
            let snap = ConfigSnapshot {
                wifi_list: &s.0.wifi_list,
                server_url: s.0.server_url.as_str(),
                asr_config,
                background_gif_size: s
                    .1
                    .blob_len(crate::background::BACKGROUND_GIF_KEY)
                    .ok()
                    .flatten(),
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
            if let Some(asr) = patch.asr_config {
                match serde_json::from_value::<crate::audio::AsrConfig>(asr) {
                    Ok(cfg) => {
                        if let Err(e) = cfg.save_to_nvs(&mut s.1) {
                            log::error!("Failed to save asr_config: {:?}", e);
                        }
                    }
                    Err(e) => log::error!("Invalid asr_config: {:?}", e),
                }
            }
        });

    let audio_state = audio_upload.clone();
    let audio = service.create_characteristic(AUDIO_ID, NimbleProperties::WRITE);
    audio.lock().on_write(move |args| {
        if let Err(e) = handle_audio_upload_write(&setting_audio, &audio_state, args.recv_data()) {
            log::error!("BLE audio upload failed: {e:?}");
            *audio_state.lock().unwrap() = None;
        }
    });

    let background_state = background_upload.clone();
    let background = service.create_characteristic(BACKGROUND_ID, NimbleProperties::WRITE);
    background.lock().on_write(move |args| {
        if let Err(e) =
            handle_background_upload_write(&setting_background, &background_state, args.recv_data())
        {
            log::error!("BLE background upload failed: {e:?}");
            *background_state.lock().unwrap() = None;
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

fn handle_audio_upload_write(
    setting: &Arc<Mutex<(Setting, EspDefaultNvs)>>,
    state: &Arc<Mutex<Option<AudioUpload>>>,
    payload: &[u8],
) -> anyhow::Result<()> {
    let mut upload_slot = state.lock().unwrap();
    if upload_slot.is_none() {
        let expected_size = pcm_expected_size(payload)?;
        if expected_size > MAX_AUDIO_BYTES {
            anyhow::bail!(
                "audio upload too large: {}B > {}B",
                expected_size,
                MAX_AUDIO_BYTES
            );
        }
        log::info!("BLE PCM upload start: expected={}B", expected_size);
        *upload_slot = Some(AudioUpload {
            expected_size,
            data: Vec::with_capacity(expected_size),
        });
        return Ok(());
    }

    let upload = upload_slot.as_mut().unwrap();
    let next_len = upload.data.len() + payload.len();
    if next_len > upload.expected_size || next_len > MAX_AUDIO_BYTES {
        anyhow::bail!(
            "audio upload overflow: received {}B, expected {}B",
            next_len,
            upload.expected_size
        );
    }
    upload.data.extend_from_slice(payload);
    log::debug!(
        "BLE audio upload chunk: {}B/{}B",
        upload.data.len(),
        upload.expected_size
    );

    if upload.data.len() == upload.expected_size {
        validate_pcm(&upload.data)?;
        let locked = setting.lock().unwrap();
        locked
            .1
            .set_blob(crate::audio::PROMPT_PCM_KEY, &upload.data)?;
        log::info!(
            "BLE PCM upload complete: {}B saved to NVS key {:?}",
            upload.data.len(),
            crate::audio::PROMPT_PCM_KEY
        );
        *upload_slot = None;
    }

    Ok(())
}

fn validate_pcm(data: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(!data.is_empty(), "PCM data is empty");
    anyhow::ensure!(data.len() % 2 == 0, "PCM data must be i16 aligned");
    Ok(())
}

fn pcm_expected_size(data: &[u8]) -> anyhow::Result<usize> {
    if data.len() != 4 {
        anyhow::bail!("PCM upload must start with 4-byte length packet");
    }
    let len = u32::from_le_bytes(data.try_into().unwrap()) as usize;
    validate_pcm_len(len)?;
    Ok(len)
}

fn validate_pcm_len(len: usize) -> anyhow::Result<()> {
    anyhow::ensure!(len > 0, "PCM data is empty");
    anyhow::ensure!(len <= MAX_AUDIO_BYTES, "PCM data exceeds 256KB");
    anyhow::ensure!(len % 2 == 0, "PCM length must be i16 aligned");
    Ok(())
}

fn handle_background_upload_write(
    setting: &Arc<Mutex<(Setting, EspDefaultNvs)>>,
    state: &Arc<Mutex<Option<BackgroundUpload>>>,
    payload: &[u8],
) -> anyhow::Result<()> {
    let mut upload_slot = state.lock().unwrap();
    if upload_slot.is_none() {
        let expected_size = background_expected_size(payload)?;
        log::info!(
            "BLE background GIF upload start: expected={}B",
            expected_size
        );
        *upload_slot = Some(BackgroundUpload {
            expected_size,
            data: Vec::with_capacity(expected_size),
        });
        return Ok(());
    }

    let upload = upload_slot.as_mut().unwrap();
    let next_len = upload.data.len() + payload.len();
    if next_len > upload.expected_size || next_len > crate::background::MAX_BACKGROUND_GIF_BYTES {
        anyhow::bail!(
            "background upload overflow: received {}B, expected {}B",
            next_len,
            upload.expected_size
        );
    }
    upload.data.extend_from_slice(payload);
    log::debug!(
        "BLE background upload chunk: {}B/{}B",
        upload.data.len(),
        upload.expected_size
    );

    if upload.data.len() == upload.expected_size {
        crate::background::validate_gif(&upload.data)?;
        let locked = setting.lock().unwrap();
        crate::background::save_to_nvs(&locked.1, &upload.data)?;
        log::info!(
            "BLE background GIF upload complete: {}B saved to NVS key {:?}",
            upload.data.len(),
            crate::background::BACKGROUND_GIF_KEY
        );
        *upload_slot = None;
    }

    Ok(())
}

fn background_expected_size(data: &[u8]) -> anyhow::Result<usize> {
    if data.len() != 4 {
        anyhow::bail!("background upload must start with 4-byte length packet");
    }
    let len = u32::from_le_bytes(data.try_into().unwrap()) as usize;
    anyhow::ensure!(len > 0, "background GIF data is empty");
    anyhow::ensure!(
        len <= crate::background::MAX_BACKGROUND_GIF_BYTES,
        "background GIF exceeds {}KB",
        crate::background::MAX_BACKGROUND_GIF_BYTES / 1024
    );
    Ok(len)
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
