use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::time::Duration;

static POWER_WORKER_STARTED: AtomicBool = AtomicBool::new(false);
static LIGHT_SLEEP_LOCK: AtomicPtr<esp_idf_svc::sys::esp_pm_lock> =
    AtomicPtr::new(std::ptr::null_mut());
static APB_FREQ_LOCK: AtomicPtr<esp_idf_svc::sys::esp_pm_lock> =
    AtomicPtr::new(std::ptr::null_mut());
static LIGHT_SLEEP_LOCK_HELD: AtomicBool = AtomicBool::new(false);

const AXP2101_STATUS1_VBUS_GOOD: u8 = 1 << 5;

#[derive(Clone, Copy, Debug)]
pub struct PmuStatus {
    pub status1: u8,
    pub status2: u8,
}

pub fn init() -> anyhow::Result<()> {
    esp_err("board_pmu_init", unsafe {
        esp_idf_svc::sys::board::board_pmu_init()
    })
}

pub fn init_cpu_frequency_scaling() -> anyhow::Result<()> {
    let config = esp_idf_svc::sys::esp_pm_config_t {
        max_freq_mhz: 240,
        min_freq_mhz: 40,
        light_sleep_enable: true,
    };
    let code = unsafe { esp_idf_svc::sys::esp_pm_configure((&config as *const _) as *const _) };
    esp_err("esp_pm_configure", code)?;
    log::info!(
        "CPU dynamic frequency scaling enabled: {}-{} MHz, light_sleep={}",
        config.min_freq_mhz,
        config.max_freq_mhz,
        config.light_sleep_enable
    );
    hold_light_sleep_lock()?;
    Ok(())
}

pub fn hold_light_sleep_lock() -> anyhow::Result<()> {
    if LIGHT_SLEEP_LOCK_HELD.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let sleep_handle = light_sleep_lock_handle()?;
    let code = unsafe { esp_idf_svc::sys::esp_pm_lock_acquire(sleep_handle) };
    if code != esp_idf_svc::sys::ESP_OK as i32 {
        LIGHT_SLEEP_LOCK_HELD.store(false, Ordering::SeqCst);
        return Err(anyhow::anyhow!(
            "esp_pm_lock_acquire(display) failed: esp_err_t={code}"
        ));
    }

    let apb_handle = apb_freq_lock_handle()?;
    let code = unsafe { esp_idf_svc::sys::esp_pm_lock_acquire(apb_handle) };
    if code != esp_idf_svc::sys::ESP_OK as i32 {
        let _ = unsafe { esp_idf_svc::sys::esp_pm_lock_release(sleep_handle) };
        LIGHT_SLEEP_LOCK_HELD.store(false, Ordering::SeqCst);
        return Err(anyhow::anyhow!(
            "esp_pm_lock_acquire(display_apb) failed: esp_err_t={code}"
        ));
    }

    log::info!("Display power locks acquired");
    Ok(())
}

pub fn release_light_sleep_lock() -> anyhow::Result<()> {
    if LIGHT_SLEEP_LOCK_HELD.load(Ordering::SeqCst) {
        match pmu_status() {
            Some(status) if status.status1 & AXP2101_STATUS1_VBUS_GOOD != 0 => {
                log::info!(
                    "External power connected; keeping display power locks acquired (status1=0x{:02x}, status2=0x{:02x})",
                    status.status1,
                    status.status2
                );
                return Ok(());
            }
            Some(_) => {}
            None => log::warn!("PMU status unavailable; releasing display power locks anyway"),
        }
    }

    if !LIGHT_SLEEP_LOCK_HELD.swap(false, Ordering::SeqCst) {
        return Ok(());
    }

    let mut first_err = None;

    let apb_handle = APB_FREQ_LOCK.load(Ordering::SeqCst);
    if !apb_handle.is_null() {
        let code = unsafe { esp_idf_svc::sys::esp_pm_lock_release(apb_handle) };
        if code != esp_idf_svc::sys::ESP_OK as i32 {
            first_err = Some(("esp_pm_lock_release(display_apb)", code));
        }
    }

    let sleep_handle = LIGHT_SLEEP_LOCK.load(Ordering::SeqCst);
    if !sleep_handle.is_null() {
        let code = unsafe { esp_idf_svc::sys::esp_pm_lock_release(sleep_handle) };
        if code != esp_idf_svc::sys::ESP_OK as i32 && first_err.is_none() {
            first_err = Some(("esp_pm_lock_release(display)", code));
        }
    }

    if let Some((context, code)) = first_err {
        LIGHT_SLEEP_LOCK_HELD.store(true, Ordering::SeqCst);
        return Err(anyhow::anyhow!("{context} failed: esp_err_t={code}"));
    }

    log::info!("Display power locks released");
    dump_pm_locks_to_stdout();
    Ok(())
}

fn light_sleep_lock_handle() -> anyhow::Result<esp_idf_svc::sys::esp_pm_lock_handle_t> {
    let existing = LIGHT_SLEEP_LOCK.load(Ordering::SeqCst);
    if !existing.is_null() {
        return Ok(existing);
    }

    let mut handle = std::ptr::null_mut();
    let code = unsafe {
        esp_idf_svc::sys::esp_pm_lock_create(
            esp_idf_svc::sys::esp_pm_lock_type_t_ESP_PM_NO_LIGHT_SLEEP,
            0,
            b"display\0".as_ptr().cast(),
            &mut handle,
        )
    };
    esp_err("esp_pm_lock_create(display)", code)?;
    LIGHT_SLEEP_LOCK.store(handle, Ordering::SeqCst);
    log::info!("Display light sleep lock created");
    Ok(handle)
}

fn apb_freq_lock_handle() -> anyhow::Result<esp_idf_svc::sys::esp_pm_lock_handle_t> {
    let existing = APB_FREQ_LOCK.load(Ordering::SeqCst);
    if !existing.is_null() {
        return Ok(existing);
    }

    let mut handle = std::ptr::null_mut();
    let code = unsafe {
        esp_idf_svc::sys::esp_pm_lock_create(
            esp_idf_svc::sys::esp_pm_lock_type_t_ESP_PM_APB_FREQ_MAX,
            0,
            b"display_apb\0".as_ptr().cast(),
            &mut handle,
        )
    };
    esp_err("esp_pm_lock_create(display_apb)", code)?;
    APB_FREQ_LOCK.store(handle, Ordering::SeqCst);
    log::info!("Display APB max lock created");
    Ok(handle)
}

fn dump_pm_locks_to_stdout() {
    log::info!("Dumping PM locks to stdout");
    let stdout = unsafe {
        core::ptr::addr_of_mut!(esp_idf_svc::sys::__sf)
            .cast::<esp_idf_svc::sys::FILE>()
            .add(1)
    };
    let code = unsafe { esp_idf_svc::sys::esp_pm_dump_locks(stdout) };
    if code != esp_idf_svc::sys::ESP_OK as i32 {
        log::warn!("esp_pm_dump_locks(stdout) failed: esp_err_t={code}");
    }
    unsafe {
        esp_idf_svc::sys::fflush(stdout);
    }
}

pub fn start_power_key_worker() {
    if POWER_WORKER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    if let Err(e) = std::thread::Builder::new()
        .name("power-key".to_string())
        .stack_size(4096)
        .spawn(|| loop {
            if unsafe { esp_idf_svc::sys::board::board_pmu_take_pkey_long_press() } {
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
    let err = unsafe { esp_idf_svc::sys::board::board_pmu_shutdown() };
    if err != esp_idf_svc::sys::ESP_OK as i32 {
        log::error!("board_pmu_shutdown failed: esp_err_t={err}");
    }
}

pub fn battery_percent() -> Option<u8> {
    let percent = unsafe { esp_idf_svc::sys::board::board_pmu_battery_percent() };
    if (0..=100).contains(&percent) {
        Some(percent as u8)
    } else {
        None
    }
}

pub fn pmu_status() -> Option<PmuStatus> {
    let status1 = unsafe { esp_idf_svc::sys::board::board_pmu_status1() };
    let status2 = unsafe { esp_idf_svc::sys::board::board_pmu_status2() };
    if (0..=u8::MAX as i32).contains(&status1) && (0..=u8::MAX as i32).contains(&status2) {
        Some(PmuStatus {
            status1: status1 as u8,
            status2: status2 as u8,
        })
    } else {
        None
    }
}

#[allow(dead_code)]
pub fn external_power_connected() -> Option<bool> {
    pmu_status().map(|status| status.status1 & AXP2101_STATUS1_VBUS_GOOD != 0)
}

fn esp_err(context: &str, code: i32) -> anyhow::Result<()> {
    if code == esp_idf_svc::sys::ESP_OK as i32 {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{context} failed: esp_err_t={code}"))
    }
}
