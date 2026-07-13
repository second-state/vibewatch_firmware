use esp_idf_svc::hal::{self, ledc::LedcDriver};

pub fn spd2010_reset(i2c: &mut esp_idf_svc::hal::i2c::I2cDriver<'static>) -> anyhow::Result<()> {
    super::exio::set_exio(i2c, super::exio::ExioPin::Exio2, false)?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    super::exio::set_exio(i2c, super::exio::ExioPin::Exio2, true)?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    Ok(())
}

pub fn backlight_init(bl_pin: hal::gpio::AnyIOPin) -> anyhow::Result<LedcDriver<'static>> {
    let config = hal::ledc::config::TimerConfig::new()
        .resolution(hal::ledc::Resolution::Bits13)
        .frequency(hal::units::Hertz(5000));
    let time = unsafe { hal::ledc::TIMER0::new() };
    let timer_driver = hal::ledc::LedcTimerDriver::new(time, &config)?;

    let ledc_driver =
        hal::ledc::LedcDriver::new(unsafe { hal::ledc::CHANNEL0::new() }, timer_driver, bl_pin)?;

    Ok(ledc_driver)
}

const LEDC_MAX_DUTY: u32 = (1 << 13) - 1;

pub fn set_backlight<'d>(
    ledc_driver: &mut hal::ledc::LedcDriver<'d>,
    light: u8,
) -> anyhow::Result<()> {
    let light = 100.min(light) as u32;
    let duty = LEDC_MAX_DUTY - (81 * (100 - light));
    let duty = if light == 0 { 0 } else { duty };
    ledc_driver.set_duty(duty)?;
    Ok(())
}

pub fn qspi_init() {
    unsafe {
        if esp_idf_svc::sys::hello::QSPI_Init() == 0 {
            panic!("QSPI_Init failed");
        }
        let panel = std::mem::transmute(esp_idf_svc::sys::hello::get_panel_handle());
        clear_draw_bitmap(panel);
    }
}

pub const LCD_WIDTH: u16 = 412;
pub const LCD_HEIGHT: u16 = 412;
pub const LCD_COLOR_BITS: u16 = 16;

pub fn clear_draw_bitmap(panel: esp_idf_svc::sys::esp_lcd_panel_handle_t) {
    let byte_per_pixel = LCD_COLOR_BITS / 8;
    let mut color = vec![0_u8; LCD_HEIGHT as usize * LCD_WIDTH as usize * byte_per_pixel as usize];

    unsafe {
        esp_idf_svc::sys::esp_lcd_panel_draw_bitmap(
            panel,
            0,
            0,
            LCD_HEIGHT as i32,
            LCD_WIDTH as i32,
            color.as_mut_ptr().cast(),
        );
    }
}

pub fn flush_display(color_data: &[u8], x_start: i32, y_start: i32, x_end: i32, y_end: i32) -> i32 {
    unsafe {
        let panel = std::mem::transmute(esp_idf_svc::sys::hello::get_panel_handle());
        let e = esp_idf_svc::sys::esp_lcd_panel_draw_bitmap(
            panel,
            x_start,
            y_start,
            x_end,
            y_end,
            color_data.as_ptr().cast(),
        );
        if e != 0 {
            log::warn!("flush_display error: {}", e);
        }
        e
    }
}
