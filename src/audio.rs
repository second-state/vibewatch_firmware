use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};

use embedded_svc::io::Write;

pub const SAMPLE_RATE: u32 = 16000;
pub const PROMPT_PCM_KEY: &str = "audio_pcm";
const PROMPT_ENABLED_KEY: &str = "audio_prompt_on";

extern "C" {
    fn board_audio_init() -> std::ffi::c_int;
    fn board_audio_read_mic(data: *mut std::ffi::c_void, len: std::ffi::c_int) -> std::ffi::c_int;
    fn board_audio_write_speaker(
        data: *const std::ffi::c_void,
        len: std::ffi::c_int,
    ) -> std::ffi::c_int;
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

pub fn write_speaker_bytes(bytes: &[u8]) -> anyhow::Result<usize> {
    if bytes.is_empty() {
        return Ok(0);
    }
    if bytes.len() > std::ffi::c_int::MAX as usize {
        anyhow::bail!("speaker buffer length exceeds C int range");
    }

    let written =
        unsafe { board_audio_write_speaker(bytes.as_ptr().cast(), bytes.len() as std::ffi::c_int) };
    if written < 0 {
        esp_result("board_audio_write_speaker", written)?;
        unreachable!();
    }

    Ok(written as usize)
}

pub fn prompt_enabled(nvs: &esp_idf_svc::nvs::EspDefaultNvs) -> bool {
    nvs.get_u8(PROMPT_ENABLED_KEY)
        .ok()
        .flatten()
        .map(|value| value != 0)
        .unwrap_or(true)
}

pub fn save_prompt_enabled(
    nvs: &esp_idf_svc::nvs::EspDefaultNvs,
    enabled: bool,
) -> anyhow::Result<()> {
    nvs.set_u8(PROMPT_ENABLED_KEY, u8::from(enabled))?;
    Ok(())
}

fn esp_result(context: &str, code: i32) -> anyhow::Result<()> {
    if code == esp_idf_svc::sys::ESP_OK as i32 {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{context} failed: esp_err_t={code}"))
    }
}

#[derive(Clone)]
pub struct Prompt {
    pcm: Arc<Vec<u8>>,
}

impl Prompt {
    pub fn load_from_nvs(nvs: &esp_idf_svc::nvs::EspDefaultNvs) -> Option<Self> {
        let len = nvs.blob_len(PROMPT_PCM_KEY).ok()??;
        if len == 0 {
            return None;
        }

        let mut pcm = vec![0u8; len];
        let pcm = nvs.get_blob(PROMPT_PCM_KEY, &mut pcm).ok()??;
        if pcm.len() % 2 != 0 {
            log::error!("Invalid audio prompt PCM length: {}B", pcm.len());
            return None;
        }
        log::info!("Loaded audio prompt: {}B PCM", pcm.len());
        Some(Self {
            pcm: Arc::new(pcm.to_vec()),
        })
    }
}

#[derive(Clone)]
pub struct PromptPlayer {
    tx: mpsc::SyncSender<()>,
    playing: Arc<AtomicBool>,
}

impl PromptPlayer {
    pub fn start(prompt: Prompt) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::sync_channel(1);
        let pcm = prompt.pcm;
        let playing = Arc::new(AtomicBool::new(false));
        let worker_playing = playing.clone();

        std::thread::Builder::new()
            .name("audio-prompt".to_string())
            .stack_size(4096)
            .spawn(move || {
                while rx.recv().is_ok() {
                    if let Err(e) = play_pcm(&pcm) {
                        log::error!("Audio prompt playback failed: {e:?}");
                    }
                    worker_playing.store(false, Ordering::Release);
                }
                log::info!("Audio prompt worker thread exited");
            })?;

        Ok(Self { tx, playing })
    }

    pub fn play_async(&self) {
        if self
            .playing
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            log::debug!("Audio prompt already playing, skipping");
            return;
        }

        if let Err(e) = self.tx.try_send(()) {
            log::error!("Failed to queue audio prompt playback: {e:?}");
            self.playing.store(false, Ordering::Release);
        }
    }
}

fn play_pcm(pcm: &[u8]) -> anyhow::Result<()> {
    const CHUNK_BYTES: usize = 2048;
    for chunk in pcm.chunks(CHUNK_BYTES) {
        let written = write_speaker_bytes(chunk)?;
        if written != chunk.len() {
            anyhow::bail!("short speaker write: {}B/{}B", written, chunk.len());
        }
    }
    Ok(())
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
}

pub struct AsrRequest {
    pub config: AsrConfig,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    pub listening: tokio::sync::oneshot::Sender<()>,
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
