use std::sync::Arc;

use embedded_svc::io::Write;

pub const SAMPLE_RATE: u32 = 16000;

extern "C" {
    fn board_audio_init() -> std::ffi::c_int;
    fn board_audio_read_mic(data: *mut std::ffi::c_void, len: std::ffi::c_int) -> std::ffi::c_int;
}

pub fn init() -> anyhow::Result<()> {
    let code = unsafe { board_audio_init() };
    esp_result("board_audio_init", code)
}

pub fn read_mic_i16(samples: &mut [i16]) -> anyhow::Result<usize> {
    if samples.is_empty() {
        return Ok(0);
    }

    let byte_len = samples
        .len()
        .checked_mul(std::mem::size_of::<i16>())
        .ok_or_else(|| anyhow::anyhow!("mic buffer length overflow"))?;
    if byte_len > std::ffi::c_int::MAX as usize {
        anyhow::bail!("mic buffer length exceeds C int range");
    }

    let read =
        unsafe { board_audio_read_mic(samples.as_mut_ptr().cast(), byte_len as std::ffi::c_int) };
    if read < 0 {
        esp_result("board_audio_read_mic", read)?;
        unreachable!();
    }

    Ok(read as usize / std::mem::size_of::<i16>())
}

fn esp_result(context: &str, code: i32) -> anyhow::Result<()> {
    if code == esp_idf_svc::sys::ESP_OK as i32 {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{context} failed: esp_err_t={code}"))
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(tag = "platform")]
pub enum AsrConfig {
    #[serde(alias = "whisper")]
    Whisper {
        uri: String,
        api_key: String,
        model: String,
    },
}

impl AsrConfig {
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        Ok(serde_json::from_str(json)?)
    }

    pub fn load_from_nvs(nvs: &esp_idf_svc::nvs::EspDefaultNvs) -> Option<Self> {
        let asr_config_len = nvs.str_len("asr_config").ok()??;
        if asr_config_len == 0 {
            return None;
        }

        let mut buffer = vec![0u8; asr_config_len];
        let json = nvs.get_str("asr_config", &mut buffer).ok()??;
        Self::from_json(json).ok()
    }

    pub fn save_to_nvs(&self, nvs: &mut esp_idf_svc::nvs::EspDefaultNvs) -> anyhow::Result<()> {
        let json = serde_json::to_string(self)?;
        nvs.set_str("asr_config", &json)?;
        Ok(())
    }

    pub fn requires_tls(&self) -> bool {
        match self {
            AsrConfig::Whisper { uri, .. } => uri.starts_with("https://"),
        }
    }
}

pub struct AsrRequest {
    pub config: AsrConfig,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    pub respond: tokio::sync::oneshot::Sender<anyhow::Result<String>>,
}

#[derive(Debug, serde::Deserialize)]
struct AsrResult {
    #[serde(default)]
    text: String,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

impl AsrResult {
    fn parse_text(&self) -> String {
        if self.text.trim().starts_with('[') {
            let mut texts = vec![];
            for line in self.text.lines() {
                if let Some((_, text)) = line.split_once("] ") {
                    texts.push(text.to_string());
                } else {
                    texts.push(line.to_string());
                }
            }
            texts.join("\n")
        } else {
            self.text.clone()
        }
    }
}

pub struct Driver;

impl Driver {
    pub fn new() -> anyhow::Result<Self> {
        init()?;
        Ok(Self)
    }

    pub fn read(&mut self, buffer: &mut [u8]) -> anyhow::Result<usize> {
        if buffer.len() % std::mem::size_of::<i16>() != 0 {
            anyhow::bail!("audio read buffer length must be aligned to i16");
        }

        let samples = unsafe {
            std::slice::from_raw_parts_mut(
                buffer.as_mut_ptr().cast::<i16>(),
                buffer.len() / std::mem::size_of::<i16>(),
            )
        };
        let samples_read = read_mic_i16(samples)?;
        Ok(samples_read * std::mem::size_of::<i16>())
    }

    pub fn start_whisper(
        &mut self,
        uri: &str,
        api_key: &str,
        model: &str,
        mut on_start_listen: impl FnMut(),
        mut is_stop: impl FnMut() -> bool,
    ) -> anyhow::Result<String> {
        let config = esp_idf_svc::http::client::Configuration {
            crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
            ..Default::default()
        };
        let conn = esp_idf_svc::http::client::EspHttpConnection::new(&config)?;
        let mut client = embedded_svc::http::client::Client::wrap(conn);

        let boundary = "----WebKitFormBoundary7MA4YWxkTrZu0gW";
        let content_type = format!("multipart/form-data; boundary={boundary}");
        let authorization = format!("Bearer {api_key}");
        let headers = [
            ("Content-Type", content_type.as_str()),
            ("Authorization", authorization.as_str()),
        ];
        let mut req = client.post(
            uri,
            if api_key.is_empty() {
                &headers[..1]
            } else {
                &headers
            },
        )?;

        let header = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n"
        );
        req.write_all(header.as_bytes())?;

        let wav_header = crate::util::create_unlimited_wav_header(&crate::util::WavConfig {
            sample_rate: SAMPLE_RATE,
            channels: 1,
            bits_per_sample: 16,
        });
        req.write_all(&wav_header)?;

        on_start_listen();

        let mut buffer = vec![0u8; 2 * SAMPLE_RATE as usize / 10];
        let max_chunks = 10 * 30;
        for _ in 0..max_chunks {
            if is_stop() {
                break;
            }
            let len = self.read(&mut buffer)?;
            if len > 0 {
                req.write_all(&buffer[..len])?;
            }
        }

        let model_field = format!(
            "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{model}\r\n"
        );
        req.write_all(model_field.as_bytes())?;
        let footer = format!("--{boundary}--");
        req.write_all(footer.as_bytes())?;
        req.flush()?;

        let mut resp = req.submit()?;
        log::info!("ASR response status: {}", resp.status());
        let bytes_read =
            embedded_svc::utils::io::try_read_full(&mut resp, &mut buffer).map_err(|e| e.0)?;
        let resp_body = std::str::from_utf8(&buffer[..bytes_read])?;
        let asr_result: AsrResult = serde_json::from_str(resp_body)?;
        if let Some(ref e) = asr_result.error {
            log::error!(
                "ASR error: {}",
                serde_json::to_string(e).unwrap_or_default()
            );
        }

        Ok(asr_result.parse_text())
    }

    pub fn start_asr(
        &mut self,
        asr_config: &AsrConfig,
        on_start_listen: impl FnMut(),
        is_stop: impl FnMut() -> bool,
    ) -> anyhow::Result<String> {
        match asr_config {
            AsrConfig::Whisper {
                uri,
                api_key,
                model,
            } => self.start_whisper(uri, api_key, model, on_start_listen, is_stop),
        }
    }
}
