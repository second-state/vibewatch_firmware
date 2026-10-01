use esp_idf_svc::{eventloop::EspSystemEventLoop, hal::reset::restart};

mod app;
mod audio;
mod background;
mod ble_provision;
mod boot;
mod lcd;
mod microlink;
mod mqtt;
mod network;
mod new_jpg;
mod ota;
mod power;
mod protocol;
mod remote;
mod setting;
mod tailscale;
// Heap trace debug tool (unlinked). To use: uncomment this, the call sites
// marked "heap_trace", and the sdkconfig lines — see src/heap_trace.rs.
// mod heap_trace;
mod touch;
mod ui;
mod util;
mod watch_ui;

/// C entry for the PSRAM-stacked ASR worker (see util::PsramTask).
/// Owns the channel receiver for the worker's whole lifetime.
unsafe extern "C" fn asr_worker_entry(arg: *mut core::ffi::c_void) {
    let asr_rx = Box::from_raw(arg as *mut std::sync::mpsc::Receiver<audio::AsrRequest>);
    let mut driver = audio::Driver::new()
        .map_err(|e| log::error!("Failed to create audio driver: {e:?}"))
        .ok();
    while let Ok(req) = asr_rx.recv() {
        let mut listening = Some(req.listening);
        let result = match driver.as_mut() {
            Some(driver) => driver.start_asr(
                &req.config,
                || {
                    if let Some(tx) = listening.take() {
                        let _ = tx.send(());
                    }
                },
                || req.cancel.load(std::sync::atomic::Ordering::Relaxed),
            ),
            None => Err(anyhow::anyhow!("audio driver unavailable")),
        };
        let _ = req.respond.send(result);
    }
    log::info!("ASR worker thread exited");
}

fn main() -> anyhow::Result<()> {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    util::log_heap_usage("startup: begin");
    power::init_cpu_frequency_scaling()?;

    let peripherals = esp_idf_svc::hal::peripherals::Peripherals::take().unwrap();
    let sysloop = EspSystemEventLoop::take()?;
    let _fs = esp_idf_svc::io::vfs::MountedEventfs::mount(20)?;
    // take() is a once-per-boot global; keep the partition alive so the BLE
    // provisioning page can clone it instead of double-taking (which fails
    // with ESP_ERR_INVALID_STATE).
    let partition = esp_idf_svc::nvs::EspDefaultNvsPartition::take()?;
    let mut nvs = esp_idf_svc::nvs::EspDefaultNvs::new(partition.clone(), "setting", true)?;
    let setting = setting::Setting::load_from_nvs(&nvs)?;
    ui::set_clock_utc_offset_secs(setting.timezone_offset_secs);
    let asr_config = audio::AsrConfig::load_from_nvs(&nvs);
    let audio_prompt = audio::Prompt::load_from_nvs(&nvs);
    let audio_prompt_enabled = audio::prompt_enabled(&nvs);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    // === LCD + touch: Waveshare ESP32-S3-Touch-AMOLED-2.06 BSP ===
    lcd::init()?;
    lcd::touch_init()?;
    let (touch_tx, touch_rx) = tokio::sync::mpsc::channel::<lcd::TouchEvent>(16);
    lcd::start_touch_worker(touch_tx)?;
    let mut touch = touch::TouchInput::new(touch_rx);
    let mut boot_button = boot::new_boot_button(peripherals.pins.gpio0.into())?;
    lcd::set_backlight(50)?;
    power::init()?;
    power::start_power_key_worker();
    util::log_heap_usage("startup: lcd/touch/power ready");
    // ===

    // === Audio: Waveshare BSP I2S + ES8311 speaker + ES7210 microphone ===
    audio::init()?;
    let audio_prompt_player =
        audio_prompt.and_then(|prompt| match audio::PromptPlayer::start(prompt) {
            Ok(player) => Some(player),
            Err(e) => {
                log::error!("Failed to start audio prompt worker: {e:?}");
                None
            }
        });
    if audio_prompt_enabled {
        if let Some(player) = audio_prompt_player.as_ref() {
            player.play_async();
        }
    }
    util::log_heap_usage("startup: audio ready");
    // ===

    // Trace every allocation that stays alive during display/gui setup. (heap_trace)
    // util::heap_trace_window_start(1024);
    // XXX(tailscale-experiment): background GIF disabled to reclaim
    // ~32 KB internal (gif canvas) + 64 KB PSRAM (raw data) while profiling.
    // let background_gif = background::load_from_nvs(&nvs);
    // if let Some(background_gif) = background_gif {
    //     runtime.block_on(ui::ui_background(background_gif)).ok();
    // }
    let mut gui = ui::UI::default();
    // if let Some(background_gif) = background_gif {
    //     if let Err(e) = gui.set_status_background_gif(background_gif) {
    //         log::error!("Failed to apply custom background GIF: {e:?}");
    //     }
    // }
    util::log_heap_usage("startup: display/gui ready");
    // util::heap_trace_window_stop(); (heap_trace)

    // A/B 双槽 OTA:标记当前启动槽为有效(确认本次正常启动;配合回滚机制)。
    {
        let mut ota = esp_idf_svc::ota::EspOta::new()?;
        ota.mark_running_slot_valid()?;
    }

    if setting.need_init() {
        // 首次启动:BLE 配网(手机连蓝牙 "Watch",通过 setup.html 写 WiFi 列表 + MQTT broker)
        match ble_provision::provision(&partition, &mut gui, &mut touch)? {
            ble_provision::BleProvisionOutcome::Reset => {}
            ble_provision::BleProvisionOutcome::Back => {
                log::warn!("BLE provisioning exited without config; restarting anyway");
            }
        }
        restart();
    }

    let mut wifi = network::WifiManager::new(peripherals.modem, sysloop)?;
    util::log_heap_usage("startup: wifi manager ready");
    // Volatile by design: after a reboot, the clock must be verified/synced again.
    let mut time_synced = false;
    // Set after leaving the OTA page via back/right swipe so the next remote
    // session reopens directly on the settings page.
    let mut reenter_remote_settings = false;

    // ASR worker: created ONCE, with its 16 KB stack allocated from PSRAM
    // (util::PsramTask) instead of scarce internal RAM. It serves every
    // remote session through the long-lived channel below; re-spawning per
    // iteration would leak the old worker (it never exits: the channel
    // never closes).
    const ASR_TASK_NO_AFFINITY: i32 = i32::MAX; // FreeRTOS tskNO_AFFINITY
    let (asr_tx, asr_rx) = std::sync::mpsc::channel::<audio::AsrRequest>();
    let _asr_task: Option<util::PsramTask> = unsafe {
        util::PsramTask::spawn(
            &std::ffi::CString::new("asr-worker").unwrap(),
            Some(asr_worker_entry),
            Box::into_raw(Box::new(asr_rx)).cast(),
            1024 * 16,
            5,
            ASR_TASK_NO_AFFINITY,
        )
    }
    .map_err(|e| log::error!("Failed to spawn ASR worker thread: {e:?}"))
    .ok();

    loop {
        let skip_home = reenter_remote_settings;
        reenter_remote_settings = false;
        if !skip_home {
            'home: loop {
                runtime.block_on(ui::clock_screen(&mut gui, &mut touch, &mut boot_button))?;
                loop {
                    match runtime.block_on(ui::main_menu(&mut gui, &mut touch))? {
                        ui::MainMenuSelection::Clock => continue 'home,
                        ui::MainMenuSelection::Remote => break 'home,
                        ui::MainMenuSelection::Setting => {
                            loop {
                                match runtime.block_on(ui::setting_menu(&mut gui, &mut touch))? {
                                    ui::SettingMenuSelection::Ota => {
                                        runtime.block_on(ota::run(
                                            &mut wifi, &setting, &mut gui, &mut touch, &mut nvs,
                                        ))?;
                                        // OTA page backed out: reopen the settings page.
                                    }
                                    ui::SettingMenuSelection::SyncTime => {
                                        if runtime
                                            .block_on(sync_time_from_settings(
                                                &mut wifi, &setting, &mut gui, &mut touch, &mut nvs,
                                            ))
                                            .unwrap_or(false)
                                        {
                                            time_synced = true;
                                        }
                                    }
                                    ui::SettingMenuSelection::Tailscale => {
                                        runtime.block_on(tailscale::run(
                                            &mut wifi, &setting, &mut gui, &mut touch,
                                        ))?;
                                    }
                                    ui::SettingMenuSelection::Ble => {
                                        match ble_provision::provision(
                                            &partition, &mut gui, &mut touch,
                                        )? {
                                            ble_provision::BleProvisionOutcome::Reset => {
                                                restart();
                                            }
                                            ble_provision::BleProvisionOutcome::Back => {}
                                        }
                                    }
                                    ui::SettingMenuSelection::Reboot => restart(),
                                    ui::SettingMenuSelection::PowerOff => {
                                        crate::power::shutdown();
                                        loop {
                                            std::thread::sleep(std::time::Duration::from_secs(60));
                                        }
                                    }
                                    ui::SettingMenuSelection::Back => break,
                                }
                            }
                        }
                    }
                }
            }
        }

        // 连 WiFi:从 wifi_list 里挑第一个在范围内的(顺序=优先级)
        runtime
            .block_on(gui.show_status("Connecting WiFi...", ""))
            .ok();

        let wifi_result = wifi.connect(&setting.wifi_list);
        if let Err(e) = wifi_result.as_ref() {
            runtime
                .block_on(gui.show_status("WiFi failed", format!("{e:?}\nReset in 5s...")))
                .ok();
            std::thread::sleep(std::time::Duration::from_secs(5));
            restart();
        }
        log::info!("WiFi connected");
        if time_synced {
            log::info!("Time already synced since boot; skipping time sync");
        } else {
            time_synced = runtime.block_on(network::sync_time_and_timezone_with_ui(
                &mut gui, &mut touch, &mut nvs,
            ))?;
            log::info!("Time sync completed for this boot: {time_synced}");
        }

        // Remote:MQTT 连 vibetty → 进入 session list(停留等输入选会话)
        runtime
            .block_on(gui.show_status("Connecting MQTT...", setting.server_url.clone()))
            .ok();

        let client_id = wifi_sta_mac_client_id();
        let asr_tx = asr_tx.clone();

        match runtime.block_on(remote::run(
            setting.server_url.clone(),
            client_id,
            &mut gui,
            &mut touch,
            &mut boot_button,
            asr_tx,
            asr_config.as_ref(),
            audio_prompt_player.as_ref(),
            audio_prompt_enabled,
            &nvs,
            skip_home,
        )) {
            Ok(selection) => {
                wifi.disconnect_and_stop();
                match selection {
                    ui::SettingMenuSelection::Tailscale => {
                        runtime
                            .block_on(tailscale::run(&mut wifi, &setting, &mut gui, &mut touch))?;
                        // Back from the Tailscale page: reopen the settings page.
                        reenter_remote_settings = true;
                    }
                    ui::SettingMenuSelection::Ota => {
                        runtime.block_on(ota::run(
                            &mut wifi, &setting, &mut gui, &mut touch, &mut nvs,
                        ))?;
                        // OTA page backed out: reopen the remote UI on the settings page.
                        reenter_remote_settings = true;
                    }
                    ui::SettingMenuSelection::SyncTime => {
                        if runtime
                            .block_on(sync_time_from_settings(
                                &mut wifi, &setting, &mut gui, &mut touch, &mut nvs,
                            ))
                            .unwrap_or(false)
                        {
                            time_synced = true;
                        }
                    }
                    ui::SettingMenuSelection::Ble => {
                        match ble_provision::provision(&partition, &mut gui, &mut touch)? {
                            ble_provision::BleProvisionOutcome::Reset => restart(),
                            ble_provision::BleProvisionOutcome::Back => {
                                // Reopen the remote UI on the settings page.
                                reenter_remote_settings = true;
                            }
                        }
                    }
                    ui::SettingMenuSelection::Reboot => restart(),
                    ui::SettingMenuSelection::PowerOff => {
                        crate::power::shutdown();
                        loop {
                            std::thread::sleep(std::time::Duration::from_secs(60));
                        }
                    }
                    ui::SettingMenuSelection::Back => {}
                }
            }
            Err(e) => {
                log::info!("remote exited: {:?}", e);
                let mut gui = ui::UI::default();
                runtime
                    .block_on(gui.show_status("Disconnected", format!("{e:?}")))
                    .ok();
                std::thread::sleep(std::time::Duration::from_secs(5));
                restart();
            }
        }
    }
}

async fn sync_time_from_settings(
    wifi: &mut network::WifiManager,
    setting: &setting::Setting,
    gui: &mut ui::UI,
    touch: &mut touch::TouchInput,
    nvs: &mut esp_idf_svc::nvs::EspDefaultNvs,
) -> anyhow::Result<bool> {
    gui.show_status("Sync Time", "Connecting WiFi...")
        .await
        .ok();
    match wifi.connect(&setting.wifi_list) {
        Ok(()) => {
            let result = network::sync_time_and_timezone_with_ui(gui, touch, nvs).await;
            wifi.disconnect_and_stop();
            match result {
                Ok(synced) => {
                    gui.show_status("Sync Time", if synced { "Done" } else { "Skipped" })
                        .await
                        .ok();
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                    Ok(synced)
                }
                Err(e) => {
                    gui.show_status("Sync Time", format!("{e:?}")).await.ok();
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    Err(e)
                }
            }
        }
        Err(e) => {
            wifi.disconnect_and_stop();
            gui.show_status("WiFi failed", format!("{e:?}")).await.ok();
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            Err(e)
        }
    }
}

/// 用 WiFi STA MAC 生成 broker 内唯一的 MQTT client_id(esp_read_mac 直接读 efuse)。
fn wifi_sta_mac_client_id() -> String {
    unsafe {
        let mut mac = [0u8; 6];
        esp_idf_svc::sys::esp_read_mac(
            mac.as_mut_ptr(),
            esp_idf_svc::sys::esp_mac_type_t_ESP_MAC_WIFI_STA,
        );
        format!(
            "watch-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        )
    }
}
