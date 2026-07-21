//! 配置数据层:WiFi 凭据列表 + MQTT broker,纯 NVS 存储,无 BLE 依赖。
//! 主固件 / OTA 救援固件共用(救援固件只读 wifi_list,不需要 BLE 配网模块)。
//!
//! NVS("setting" namespace):`wifi_list` 存整份 JSON(顺序=连接优先级),`server_url` 存字符串。

use esp_idf_svc::nvs::EspDefaultNvs;
use serde::{Deserialize, Serialize};

/// NVS key:整份 wifi_list 作为一个 JSON 字符串存。
pub const WIFI_LIST_KEY: &str = "wifi_list";
/// NVS 单值 ~4KB 限额内最多保存的 WiFi 数。
pub const MAX_WIFI_CREDS: usize = 8;

/// 单条 WiFi 凭据。顺序即连接优先级。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WifiCred {
    pub ssid: String,
    pub pass: String,
}

#[derive(Debug, Clone)]
pub struct Setting {
    /// 多组已配置 WiFi;连接时与扫描结果匹配,顺序即优先级。
    pub wifi_list: Vec<WifiCred>,
    /// MQTT broker URI(mqtt:// 或 mqtts://),主固件连 vibetty 用;救援固件不读。
    pub server_url: String,
}

impl Setting {
    pub fn save_wifi_list(nvs: &mut EspDefaultNvs, list: &[WifiCred]) -> anyhow::Result<()> {
        let json = serde_json::to_string(list)?;
        nvs.set_str(WIFI_LIST_KEY, &json)?;
        Ok(())
    }

    /// 清空全部配置(将来的「恢复出厂」设置项用;触屏设置页接入后调用)。
    #[allow(dead_code)]
    pub fn clear_nvs(nvs: &mut EspDefaultNvs) -> anyhow::Result<()> {
        nvs.remove(WIFI_LIST_KEY)?;
        nvs.remove("server_url")?;
        nvs.remove("asr_config")?;
        Ok(())
    }

    pub fn load_from_nvs(nvs: &EspDefaultNvs) -> anyhow::Result<Self> {
        let mut json_buf = [0u8; 4096];
        let wifi_list = nvs
            .get_str(WIFI_LIST_KEY, &mut json_buf)
            .ok()
            .flatten()
            .and_then(|s| match serde_json::from_str::<Vec<WifiCred>>(s) {
                Ok(v) => Some(v),
                Err(e) => {
                    log::error!("Failed to parse wifi_list JSON: {:?}", e);
                    None
                }
            })
            .unwrap_or_default();
        log::info!("Loaded {} wifi creds from NVS", wifi_list.len());

        let mut url_buf = [0u8; 128];
        let server_url = nvs
            .get_str("server_url", &mut url_buf)
            .ok()
            .flatten()
            .unwrap_or("")
            .to_string();

        Ok(Setting {
            wifi_list,
            server_url,
        })
    }

    /// 首次启动判断:wifi_list 或 server_url 任一为空 → 需要配网。
    pub fn need_init(&self) -> bool {
        self.wifi_list.is_empty() || self.server_url.is_empty()
    }
}
