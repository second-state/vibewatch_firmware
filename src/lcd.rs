use esp_idf_svc::hal::interrupt::asynch::HalIsrNotification;

static TOUCH_NOTIFY: HalIsrNotification = HalIsrNotification::new();
static LCD_COLOR_TRANS_DONE_NOTIFY: HalIsrNotification = HalIsrNotification::new();

pub const LCD_WIDTH: u16 = 410;
pub const LCD_HEIGHT: u16 = 502;
pub const LCD_COLOR_BITS: u16 = 16;
const FLUSH_CHUNK_ROWS: i32 = 64;
const FLUSH_RETRY_WAIT: std::time::Duration = std::time::Duration::from_millis(100);
const TOUCH_RELEASE_SEND_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);
const TOUCH_RELEASE_SEND_RETRIES: usize = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TouchPoint {
    pub x: u16,
    pub y: u16,
    pub strength: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TouchEvent {
    Press(TouchPoint),
    Release(TouchPoint),
}

pub fn init() -> anyhow::Result<()> {
    esp_err("board_display_init", unsafe {
        esp_idf_svc::sys::board::board_display_init()
    })?;
    register_color_transfer_done_callback()?;
    clear();
    Ok(())
}

pub fn touch_init() -> anyhow::Result<()> {
    esp_err("board_touch_init", unsafe {
        esp_idf_svc::sys::board::board_touch_init(Some(touch_interrupt_callback))
    })
}

pub fn set_backlight(light: u8) -> anyhow::Result<()> {
    esp_err("board_display_set_brightness", unsafe {
        esp_idf_svc::sys::board::board_display_set_brightness(light.min(100))
    })
}

pub fn set_display_on(on: bool) -> anyhow::Result<()> {
    let panel = unsafe { esp_idf_svc::sys::board::get_panel_handle() };
    if panel.is_null() {
        return Err(anyhow::anyhow!("get_panel_handle returned null"));
    }
    esp_err("esp_lcd_panel_disp_on_off", unsafe {
        esp_idf_svc::sys::esp_lcd_panel_disp_on_off(panel as _, on)
    })
}

pub fn read_touch() -> Option<TouchPoint> {
    let mut x = 0;
    let mut y = 0;
    let mut strength = 0;
    let touched =
        unsafe { esp_idf_svc::sys::board::board_touch_read(&mut x, &mut y, &mut strength) };

    touched.then_some(TouchPoint { x, y, strength })
}

#[allow(dead_code)]
pub async fn wait_touch_interrupt() {
    let _ = TOUCH_NOTIFY.wait().await;
}

pub fn start_touch_worker(tx: tokio::sync::mpsc::Sender<TouchEvent>) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("touch-worker".to_string())
        .stack_size(4096)
        .spawn(move || loop {
            esp_idf_svc::hal::task::block_on(wait_touch_interrupt());

            let mut logged_press = false;
            let mut last_touch = None;
            loop {
                match read_touch() {
                    Some(touch) => {
                        last_touch = Some(touch);
                        if !logged_press {
                            log::info!(
                                "Touch detected: x={} y={} strength={}",
                                touch.x,
                                touch.y,
                                touch.strength
                            );
                            logged_press = true;
                        }
                        if !send_touch_press(&tx, touch) {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    None => {
                        if let Some(touch) = last_touch {
                            if logged_press {
                                log::info!(
                                    "Touch released: x={} y={} strength={}",
                                    touch.x,
                                    touch.y,
                                    touch.strength
                                );
                            }
                            if !send_touch_release(&tx, touch) {
                                return;
                            }
                        } else if logged_press {
                            log::info!("Touch released");
                        }
                        break;
                    }
                }
            }
        })?;

    Ok(())
}

fn send_touch_press(tx: &tokio::sync::mpsc::Sender<TouchEvent>, touch: TouchPoint) -> bool {
    match tx.try_send(TouchEvent::Press(touch)) {
        Ok(()) => true,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            log::debug!("Touch press dropped because channel is full");
            true
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
    }
}

fn send_touch_release(tx: &tokio::sync::mpsc::Sender<TouchEvent>, touch: TouchPoint) -> bool {
    let mut event = TouchEvent::Release(touch);
    for attempt in 0..=TOUCH_RELEASE_SEND_RETRIES {
        match tx.try_send(event) {
            Ok(()) => return true,
            Err(tokio::sync::mpsc::error::TrySendError::Full(returned)) => {
                event = returned;
                if attempt == TOUCH_RELEASE_SEND_RETRIES {
                    log::warn!("Touch release dropped because channel stayed full");
                    return true;
                }
                std::thread::sleep(TOUCH_RELEASE_SEND_RETRY_DELAY);
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return false,
        }
    }
    true
}

unsafe extern "C" fn touch_interrupt_callback(
    _touch: *mut esp_idf_svc::sys::board::esp_lcd_touch_s,
) {
    TOUCH_NOTIFY.notify_lsb();
}

unsafe extern "C" fn color_transfer_done_callback(
    _panel_io: esp_idf_svc::sys::esp_lcd_panel_io_handle_t,
    _edata: *mut esp_idf_svc::sys::esp_lcd_panel_io_event_data_t,
    _user_ctx: *mut std::ffi::c_void,
) -> bool {
    LCD_COLOR_TRANS_DONE_NOTIFY.notify_lsb();
    false
}

fn register_color_transfer_done_callback() -> anyhow::Result<()> {
    let panel_io = unsafe { esp_idf_svc::sys::board::get_panel_io_handle() }
        .cast::<esp_idf_svc::sys::esp_lcd_panel_io_t>();
    if panel_io.is_null() {
        return Err(anyhow::anyhow!("get_panel_io_handle returned null"));
    }

    let callbacks = esp_idf_svc::sys::esp_lcd_panel_io_callbacks_t {
        on_color_trans_done: Some(color_transfer_done_callback),
    };
    esp_err("esp_lcd_panel_io_register_event_callbacks", unsafe {
        esp_idf_svc::sys::esp_lcd_panel_io_register_event_callbacks(
            panel_io,
            &callbacks,
            std::ptr::null_mut(),
        )
    })
}

pub fn clear() {
    let byte_per_pixel = LCD_COLOR_BITS / 8;
    let mut color = vec![0_u8; LCD_HEIGHT as usize * LCD_WIDTH as usize * byte_per_pixel as usize];
    let panel = unsafe { esp_idf_svc::sys::board::get_panel_handle() };

    unsafe {
        esp_idf_svc::sys::esp_lcd_panel_draw_bitmap(
            panel as _,
            0,
            0,
            LCD_WIDTH as i32,
            LCD_HEIGHT as i32,
            color.as_mut_ptr().cast(),
        );
    }
}

async fn wait_color_transfer_done_or_timeout() -> bool {
    tokio::time::timeout(FLUSH_RETRY_WAIT, LCD_COLOR_TRANS_DONE_NOTIFY.wait())
        .await
        .is_ok()
}

pub async fn async_flush_display(
    color_data: &[u8],
    x_start: i32,
    y_start: i32,
    x_end: i32,
    y_end: i32,
) -> i32 {
    let width = x_end.saturating_sub(x_start);
    let height = y_end.saturating_sub(y_start);
    if width <= 0 || height <= 0 {
        return 0;
    }

    let bytes_per_pixel = (LCD_COLOR_BITS / 8) as usize;
    let row_bytes = width as usize * bytes_per_pixel;
    let required_len = row_bytes.saturating_mul(height as usize);
    if color_data.len() < required_len {
        log::warn!(
            "flush_display buffer too small: got {}, need {}",
            color_data.len(),
            required_len
        );
        return esp_idf_svc::sys::ESP_ERR_INVALID_SIZE as i32;
    }

    let panel = unsafe { esp_idf_svc::sys::board::get_panel_handle() };
    let mut y = y_start;
    let mut offset = 0usize;

    while y < y_end {
        let rows = FLUSH_CHUNK_ROWS.min(y_end - y);
        let len = row_bytes * rows as usize;
        let chunk = &color_data[offset..offset + len];

        let mut last_error = 0;
        for _ in 0..5 {
            LCD_COLOR_TRANS_DONE_NOTIFY.reset();
            let e = unsafe {
                esp_idf_svc::sys::esp_lcd_panel_draw_bitmap(
                    panel as _,
                    x_start,
                    y,
                    x_end,
                    y + rows,
                    chunk.as_ptr().cast(),
                )
            };
            if e == 0 {
                if !wait_color_transfer_done_or_timeout().await {
                    log::warn!("flush_display transfer wait timeout after successful submit");
                    continue;
                }
                last_error = 0;
                break;
            }

            last_error = e;
            log::warn!("flush_display error: {}, waiting before retry", e);
            let _ = wait_color_transfer_done_or_timeout().await;
        }
        if last_error != 0 {
            return last_error;
        }

        y += rows;
        offset += len;
    }

    0
}

fn esp_err(context: &str, code: i32) -> anyhow::Result<()> {
    if code == esp_idf_svc::sys::ESP_OK as i32 {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{context} failed: esp_err_t={code}"))
    }
}
