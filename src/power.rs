use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

extern "C" {
    fn board_pmu_init() -> std::ffi::c_int;
    fn board_pmu_take_pkey_long_press() -> bool;
    fn board_pmu_shutdown() -> std::ffi::c_int;
    fn board_pmu_battery_percent() -> std::ffi::c_int;
}

static POWER_WORKER_STARTED: AtomicBool = AtomicBool::new(false);

pub fn init() -> anyhow::Result<()> {
    esp_err("board_pmu_init", unsafe { board_pmu_init() })
}

pub fn init_cpu_frequency_scaling() -> anyhow::Result<()> {
    let config = esp_idf_svc::sys::esp_pm_config_t {
        max_freq_mhz: 240,
        min_freq_mhz: 40,
        light_sleep_enable: false,
    };
    let code = unsafe { esp_idf_svc::sys::esp_pm_configure((&config as *const _) as *const _) };
    esp_err("esp_pm_configure", code)?;
    log::info!(
        "CPU dynamic frequency scaling enabled: {}-{} MHz, light_sleep={}",
        config.min_freq_mhz,
        config.max_freq_mhz,
        config.light_sleep_enable
    );
    Ok(())
}

pub fn start_power_key_worker() {
    if POWER_WORKER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    if let Err(e) = std::thread::Builder::new()
        .name("power-key".to_string())
        .stack_size(4096)
        .spawn(|| loop {
            if unsafe { board_pmu_take_pkey_long_press() } {
                log::warn!("PWR key long-press detected, shutting down");
                shutdown();
                loop {
                    std::thread::sleep(Duration::from_secs(60));
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        })
    {
        POWER_WORKER_STARTED.store(false, Ordering::SeqCst);
        log::error!("Failed to spawn power key worker: {e:?}");
    }
}

pub fn shutdown() {
    log::warn!("Power shutdown requested");
    let _ = crate::lcd::set_backlight(0);
    let err = unsafe { board_pmu_shutdown() };
    if err != esp_idf_svc::sys::ESP_OK as i32 {
        log::error!("board_pmu_shutdown failed: esp_err_t={err}");
    }
}

pub fn battery_percent() -> Option<u8> {
    let percent = unsafe { board_pmu_battery_percent() };
    if (0..=100).contains(&percent) {
        Some(percent as u8)
    } else {
        None
    }
}

fn esp_err(context: &str, code: i32) -> anyhow::Result<()> {
    if code == esp_idf_svc::sys::ESP_OK as i32 {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{context} failed: esp_err_t={code}"))
    }
}
