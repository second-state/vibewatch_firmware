use esp_idf_svc::{eventloop::EspSystemEventLoop, sys::esp_restart};

mod audio;
#[allow(unused)]
mod exio;
mod lcd;
mod network;
mod protocol;
mod ui;
mod ws;

mod app;

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = esp_idf_svc::hal::prelude::Peripherals::take().unwrap();
    let sysloop = EspSystemEventLoop::take()?;
    let _fs = esp_idf_svc::io::vfs::MountedEventfs::mount(20)?;
    let partition = esp_idf_svc::nvs::EspDefaultNvsPartition::take()?;
    let mut nvs = esp_idf_svc::nvs::EspDefaultNvs::new(partition, "setting", true)?;

    let mut ssid_buf = [0; 32];
    let ssid = nvs
        .get_str("ssid", &mut ssid_buf)
        .map_err(|_| anyhow::anyhow!("Failed to get ssid"))?;

    let mut pass_buf = [0; 64];
    let pass = nvs
        .get_str("pass", &mut pass_buf)
        .map_err(|_| anyhow::anyhow!("Failed to get pass"))?;

    let mut server_url = [0; 128];
    let server_url = nvs
        .get_str("server_url", &mut server_url)
        .map_err(|_| anyhow::anyhow!("Failed to get server_url"))?;

    log::info!("SSID: {:?}", ssid);
    log::info!("PASS: {:?}", pass);
    log::info!("Server URL: {:?}", server_url);

    // nvs.set_str("ssid", "ChinaNet-YKyd")?;
    // nvs.set_str("pass", "19960312")?;

    let mut i2c = exio::i2c_init(
        peripherals.i2c0,
        peripherals.pins.gpio11,
        peripherals.pins.gpio10,
    )?;
    exio::exio_init(&mut i2c)?;

    // === LCD
    lcd::spd2010_reset(&mut i2c)?;
    lcd::qspi_init();
    let mut ledc_timer = lcd::backlight_init(peripherals.pins.gpio5.into())?;
    lcd::set_backlight(&mut ledc_timer, 30)?;
    // ===

    ui::ui_background().unwrap();

    let mut gui = ui::UI::default();

    let (ssid, pass, server_url) = match (ssid, pass, server_url) {
        (Some(ssid), Some(pass), Some(server_url)) => {
            (ssid.to_string(), pass.to_string(), server_url.to_string())
        }
        _ => {
            gui.state = "http://192.168.71.1".to_string();
            gui.text = format!(
                "Please connect to wifi {}.\n\nOpen URL: http://192.168.71.1",
                network::SSID,
            );
            gui.display_flush().unwrap();

            let from_data = network::wifi_http_server(peripherals.modem, sysloop.clone())?;
            log::info!("GET SSID: {:?}", from_data.wifi_username);
            log::info!("GET PASS: {:?}", from_data.wifi_password);
            log::info!("GET Server URL: {:?}", from_data.server_url);
            nvs.set_str("ssid", &from_data.wifi_username)?;
            nvs.set_str("pass", &from_data.wifi_password)?;
            nvs.set_str("server_url", &from_data.server_url)?;

            unsafe { esp_restart() }
        }
    };

    gui.state = "Connecting to wifi...".to_string();
    gui.text.clear();
    gui.display_flush().unwrap();

    let _wifi = network::wifi(&ssid, &pass, peripherals.modem, sysloop);
    if _wifi.is_err() {
        for i in 0..3 {
            let i = 3 - i;
            gui.state = format!("Failed to connect to wifi [{ssid}]");
            gui.text = format!("Reset device in {i} seconds...");
            gui.display_flush().unwrap();
            std::thread::sleep(std::time::Duration::from_secs(1));
        }

        nvs.remove("ssid")?;
        nvs.remove("pass")?;
        nvs.remove("server_url")?;

        unsafe { esp_restart() }
    }

    gui.state = "Connected to server...".to_string();
    gui.text.clear();
    gui.display_flush().unwrap();

    let (tx, rx) = tokio::sync::mpsc::channel(10);
    let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::unbounded_channel();

    let b = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let mut conf = esp_idf_svc::hal::task::thread::ThreadSpawnConfiguration::default();
    conf.stack_size = 1024 * 8;
    conf.priority = 24;
    conf.pin_to_core = Some(esp_idf_svc::hal::cpu::Core::Core1);
    let r = conf.set();
    log::info!("Set thread configuration: {:?}", r);
    let mic = std::thread::Builder::new()
        .name("Mic".to_string())
        .spawn(move || {
            audio::new_i2s_mic(
                peripherals.i2s1,
                peripherals.pins.gpio2.into(),
                peripherals.pins.gpio15.into(),
                peripherals.pins.gpio39.into(),
                tx,
                ctrl_rx,
            )
        })?;

    let main_fut = app::app_run(
        server_url,
        peripherals.i2s0,
        peripherals.pins.gpio48.into(),
        peripherals.pins.gpio38.into(),
        peripherals.pins.gpio47.into(),
        ctrl_tx,
        rx,
        nvs,
    );

    drop(gui);

    let r = b.block_on(async move { main_fut.await });

    let mic_r = mic.join();
    log::info!("MIC work: {:?}", mic_r);

    let mut gui = ui::UI::default();
    gui.state = "Disconnected".to_string();
    gui.text = format!("Main:{:?}\n Mic:{:?}", r, mic_r);
    gui.display_flush().unwrap();

    std::thread::sleep(std::time::Duration::from_secs(5));
    unsafe { esp_restart() }
}

pub fn log_heap() {
    unsafe {
        use esp_idf_svc::sys::{heap_caps_get_free_size, MALLOC_CAP_8BIT};

        log::info!(
            "Free heap size: {}",
            heap_caps_get_free_size(MALLOC_CAP_8BIT)
        );
    }
}

pub fn call_function_with_heap(stack_size: usize, shared_stack_function: extern "C" fn()) {
    unsafe {
        use esp_idf_svc::sys::{
            heap_caps_get_free_size, vQueueDelete, xQueueCreateMutex, QueueHandle_t,
            MALLOC_CAP_8BIT,
        };

        log::info!(
            "Free heap size: {}",
            heap_caps_get_free_size(MALLOC_CAP_8BIT)
        );

        let mut stack = vec![0_u8; stack_size];

        let lock = xQueueCreateMutex(1);

        extern "C" {
            fn esp_execute_shared_stack_function(
                lock: QueueHandle_t,
                stack: *mut u8,
                stack_size: usize,
                shared_stack_function: extern "C" fn(),
            );
        }

        esp_execute_shared_stack_function(
            lock,
            stack.as_mut_ptr(),
            stack_size,
            shared_stack_function,
        );

        vQueueDelete(lock);
    }
}
