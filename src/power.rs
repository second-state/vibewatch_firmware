use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

extern "C" {
    fn board_pmu_init() -> std::ffi::c_int;
    fn board_pmu_take_pkey_long_press() -> bool;
    fn board_pmu_shutdown() -> std::ffi::c_int;
}

static POWER_WORKER_STARTED: AtomicBool = AtomicBool::new(false);

pub fn init() -> anyhow::Result<()> {
    esp_err("board_pmu_init", unsafe { board_pmu_init() })
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

fn esp_err(context: &str, code: i32) -> anyhow::Result<()> {
    if code == esp_idf_svc::sys::ESP_OK as i32 {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{context} failed: esp_err_t={code}"))
    }
}
