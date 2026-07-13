use std::collections::LinkedList;

use tokio::sync::mpsc::{Sender, UnboundedReceiver};

use esp_idf_svc::hal::{
    gpio::AnyIOPin,
    i2s::{config, I2s, I2sDriver, I2sRx, I2sTx},
    peripheral::Peripheral,
};

use crate::app::Event;

pub const SAMPLE_RATE: u32 = 16000;
pub const SAMPLE_RATE_BUFFER_SIZE: usize = 2 * (SAMPLE_RATE as usize) / 10;

fn max(buf: &[u8]) -> (i32, i32) {
    let mut max = 0;
    let mut min = 0;
    for x in buf.chunks(4) {
        let sample = i16::from_le_bytes([x[0], x[1]]) as i32;
        max = max.max(sample.abs());
        min = min.min(sample.abs());
    }
    (max, min)
}

fn data_to_f32(buf: &[u8]) -> (f32, Vec<f32>) {
    let mut max = 0.0;
    let mut data = Vec::with_capacity(buf.len() / 2);
    for x in buf.chunks(2) {
        let sample = i16::from_le_bytes([x[0], x[1]]) as f32;
        if sample.abs() > max {
            max = sample.abs();
        }
        data.push(sample);
    }
    (max, data)
}

const KEYWORD_VOLUME: i32 = 800;
const MUTE_VOLUME: i32 = 450;
const END_MUTE_MS: u32 = 1500;
const START_MUTE_MS: u32 = 1000 * 10;

mod wake {

    // 56364
    pub static WAKE_WAV: &[u8] = include_bytes!("../assets/hello_beep.wav");

    static mut DATA: (*const f32, usize) = (std::ptr::null(), 0);
    extern "C" fn get_data(offset: usize, len: usize, out: *mut f32) -> i32 {
        unsafe {
            let src = std::slice::from_raw_parts(DATA.0, DATA.1);
            let len = len.min(DATA.1 - offset);
            let dst = std::slice::from_raw_parts_mut(out, len);
            dst.copy_from_slice(&src[offset..offset + len]);
        }
        0
    }

    static mut RESULT: esp_idf_svc::sys::wake::ei_impulse_result_t = unsafe { std::mem::zeroed() };
    static mut ERR_CODE: i32 = 0;

    extern "C" fn run_classifier_() {
        unsafe {
            ERR_CODE = esp_idf_svc::sys::wake::wake_run_classifier(Some(get_data), &raw mut RESULT);
        }
    }

    #[allow(static_mut_refs)]
    /// { "gaia", "no", "noise", "reset", "unknown", "yes" }
    pub(super) fn run_classifier(data_ptr: &[f32]) -> Result<(usize, f32), i32> {
        unsafe {
            DATA = (data_ptr.as_ptr(), data_ptr.len());
            crate::call_function_with_heap(1024 * 512, run_classifier_);
            DATA = (std::ptr::null(), 0);
            if ERR_CODE != esp_idf_svc::sys::wake::EI_IMPULSE_ERROR_EI_IMPULSE_OK {
                Err(ERR_CODE)
            } else {
                let r = RESULT
                    .classification
                    .iter()
                    .enumerate()
                    .max_by(|(_, x), (_, y)| {
                        x.value
                            .partial_cmp(&y.value)
                            .unwrap_or(std::cmp::Ordering::Greater)
                    })
                    .unwrap();
                Ok((r.0, r.1.value))
            }
        }
    }
}

const PORT_TICK_PERIOD_MS: u32 = 1000 / esp_idf_svc::sys::configTICK_RATE_HZ;

fn new_chat(mic: &mut I2sDriver<I2sRx>, tx: Sender<Vec<u8>>, buf: &mut [u8]) -> anyhow::Result<()> {
    let mut mute_total = START_MUTE_MS / 100;
    let mut cache = LinkedList::new();

    loop {
        // read 0.1s timeout 1s
        let n = mic.read(buf, 1000 / PORT_TICK_PERIOD_MS)?;

        let (max, _) = max(&buf[..n]);
        log::info!("record max volume: {} {}", max, mute_total);
        cache.push_back(buf[..n].to_vec());
        if cache.len() >= 5 {
            cache.pop_front();
        }

        if max < KEYWORD_VOLUME as i32 {
            mute_total -= 1;
            if mute_total == 0 {
                log::info!("record end");
                return Ok(());
            }
            continue;
        } else {
            break;
        }
    }
    log::info!("start submit");
    mute_total = END_MUTE_MS / 100;
    while let Some(x) = cache.pop_front() {
        tx.blocking_send(x)
            .map_err(|_| anyhow::anyhow!("Error sending audio data"))?;
    }

    loop {
        // read 0.1s timeout 1s
        let n = mic.read(buf, 1000 / PORT_TICK_PERIOD_MS)?;

        let (max, _) = max(&buf[..n]);
        log::info!("record max volume: {} {}", max, mute_total);

        tx.blocking_send(buf[..n].to_vec())
            .map_err(|_| anyhow::anyhow!("Error sending audio data"))?;

        if max < MUTE_VOLUME as i32 {
            mute_total -= 1;
            if mute_total == 0 {
                log::info!("record end");
                return Ok(());
            }
        } else {
            mute_total = END_MUTE_MS / 100;
        }
    }
}

pub fn new_i2s_mic<'d, I2S: I2s>(
    i2s: impl Peripheral<P = I2S> + 'd,
    ws: AnyIOPin,
    bclk: AnyIOPin,
    din: AnyIOPin,
    tx: Sender<Event>,
    mut ctrl_rx: UnboundedReceiver<Sender<Vec<u8>>>,
) -> anyhow::Result<()> {
    let i2s_config = config::StdConfig::new(
        config::Config::default(),
        config::StdClkConfig::from_sample_rate_hz(SAMPLE_RATE),
        config::StdSlotConfig::philips_slot_default(
            config::DataBitWidth::Bits16,
            config::SlotMode::Mono,
        )
        .slot_mode_mask(config::SlotMode::Mono, config::StdSlotMask::Right),
        config::StdGpioConfig::default(),
    );

    let mclk: Option<esp_idf_svc::hal::gpio::AnyIOPin> = None;

    let mut driver = I2sDriver::new_std_rx(i2s, &i2s_config, bclk, din, mclk, ws)?;
    driver.rx_enable()?;

    // 0.1s
    let mut buf = vec![0u8; SAMPLE_RATE_BUFFER_SIZE];
    let mut cache = LinkedList::new();

    for _ in 0..10 {
        // skip 1s of audio
        driver.read(&mut buf, 1000 / PORT_TICK_PERIOD_MS)?;
    }

    loop {
        let n = driver.read(&mut buf, 1000 / PORT_TICK_PERIOD_MS)?;

        if let Ok(chat_tx) = ctrl_rx.try_recv() {
            new_chat(&mut driver, chat_tx, &mut buf)?;
            continue;
        }

        let (max, data) = data_to_f32(&buf[..n]);
        cache.push_back(data);
        if cache.len() > 4 {
            cache.pop_front();
        }

        if max < KEYWORD_VOLUME as f32 {
            continue;
        }

        log::info!(
            "start wait wake word! max:{} {:?}",
            max,
            esp_idf_svc::hal::cpu::core()
        );

        // start wait wake word
        loop {
            let n = driver.read(&mut buf, 1000 / PORT_TICK_PERIOD_MS)?;
            let (_, data) = data_to_f32(&buf[..n]);
            cache.push_back(data);
            if cache.len() >= 10 {
                break;
            }
        }

        let mut data = Vec::with_capacity(SAMPLE_RATE as usize);
        while let Some(x) = cache.pop_front() {
            data.extend_from_slice(&x);
        }

        let r = wake::run_classifier(&data);

        match r {
            Ok((result, value)) => {
                let result_str: &str = match result {
                    0 => "gaia",
                    1 => "no",
                    2 => "noise",
                    3 => "reset",
                    4 => "unknown",
                    5 => "yes",
                    _ => "unrecognized",
                };
                log::info!("wake word detected: [{result_str}] {:?}", value);
                if value > 0.4 {
                    if let Err(_) = tx.blocking_send(Event::Event(result_str)) {
                        return Err(anyhow::anyhow!("Error sending audio event {result_str}"));
                    }
                } else {
                    if let Err(_) = tx.blocking_send(Event::Event("unknown")) {
                        return Err(anyhow::anyhow!("Error sending audio event {result_str}"));
                    }
                }
            }
            Err(e) => {
                log::error!("Error running classifier: {:?}", e);
            }
        }
    }
}

pub async fn player_hello<'d>(driver: &mut I2sDriver<'d, I2sTx>) -> anyhow::Result<()> {
    driver
        .write_all_async(wake::WAKE_WAV)
        .await
        .map_err(|e| anyhow::anyhow!("Error writing audio: {:?}", e))?;
    Ok(())
}
