use esp_idf_svc::hal::gpio::{AnyIOPin, Input, InterruptType, PinDriver, Pull};

pub type BootButton = PinDriver<'static, Input>;

pub fn new_boot_button(pin: AnyIOPin<'static>) -> anyhow::Result<BootButton> {
    let mut button = PinDriver::input(pin, Pull::Up)?;
    button.set_interrupt_type(InterruptType::AnyEdge)?;
    Ok(button)
}

pub async fn wait_boot_press(button: &mut BootButton) {
    loop {
        let _ = button.wait_for_any_edge().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        if button.is_low() {
            log::info!("BOOT button pressed");
            return;
        }
    }
}
