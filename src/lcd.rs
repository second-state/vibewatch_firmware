// hello.c provides a small C bridge over the Waveshare BSP component.
extern "C" {
    fn board_display_init() -> std::ffi::c_int;
    fn board_display_set_brightness(percent: u8) -> std::ffi::c_int;
    fn board_touch_init() -> std::ffi::c_int;
    fn board_touch_read(x: *mut u16, y: *mut u16, strength: *mut u16) -> bool;
    fn get_panel_handle() -> esp_idf_svc::sys::esp_lcd_panel_handle_t;
}

pub const LCD_WIDTH: u16 = 410;
pub const LCD_HEIGHT: u16 = 502;
pub const LCD_COLOR_BITS: u16 = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TouchPoint {
    pub x: u16,
    pub y: u16,
    pub strength: u16,
}

pub fn init() -> anyhow::Result<()> {
    esp_err("board_display_init", unsafe { board_display_init() })?;
    clear();
    Ok(())
}

pub fn touch_init() -> anyhow::Result<()> {
    esp_err("board_touch_init", unsafe { board_touch_init() })
}

pub fn set_backlight(light: u8) -> anyhow::Result<()> {
    esp_err("board_display_set_brightness", unsafe {
        board_display_set_brightness(light.min(100))
    })
}

pub fn read_touch() -> Option<TouchPoint> {
    let mut x = 0;
    let mut y = 0;
    let mut strength = 0;
    let touched = unsafe { board_touch_read(&mut x, &mut y, &mut strength) };

    touched.then_some(TouchPoint { x, y, strength })
}

pub fn clear() {
    let byte_per_pixel = LCD_COLOR_BITS / 8;
    let mut color = vec![0_u8; LCD_HEIGHT as usize * LCD_WIDTH as usize * byte_per_pixel as usize];

    unsafe {
        esp_idf_svc::sys::esp_lcd_panel_draw_bitmap(
            get_panel_handle(),
            0,
            0,
            LCD_WIDTH as i32,
            LCD_HEIGHT as i32,
            color.as_mut_ptr().cast(),
        );
    }
}

pub fn flush_display(color_data: &[u8], x_start: i32, y_start: i32, x_end: i32, y_end: i32) -> i32 {
    unsafe {
        let panel = get_panel_handle();
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

fn esp_err(context: &str, code: i32) -> anyhow::Result<()> {
    if code == esp_idf_svc::sys::ESP_OK as i32 {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{context} failed: esp_err_t={code}"))
    }
}
