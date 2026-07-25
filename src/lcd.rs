use esp_idf_svc::hal::interrupt::asynch::HalIsrNotification;
use std::sync::atomic::{AtomicU32, Ordering};

// hello.c provides a small C bridge over the Waveshare BSP component.
extern "C" {
    fn board_display_init() -> std::ffi::c_int;
    fn board_display_set_brightness(percent: u8) -> std::ffi::c_int;
    fn board_touch_init(
        callback: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    ) -> std::ffi::c_int;
    fn board_touch_read(x: *mut u16, y: *mut u16, strength: *mut u16) -> bool;
    fn get_panel_handle() -> esp_idf_svc::sys::esp_lcd_panel_handle_t;
}

static TOUCH_NOTIFY: HalIsrNotification = HalIsrNotification::new();
static LCD_COLOR_TRANS_DONE_COUNT: AtomicU32 = AtomicU32::new(0);

pub const LCD_WIDTH: u16 = 410;
pub const LCD_HEIGHT: u16 = 502;
pub const LCD_COLOR_BITS: u16 = 16;
const FLUSH_CHUNK_ROWS: i32 = 64;
const FLUSH_DONE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
const FLUSH_DONE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1);

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
    esp_err("board_display_init", unsafe { board_display_init() })?;
    register_color_transfer_done_callback()?;
    clear();
    Ok(())
}

pub fn touch_init() -> anyhow::Result<()> {
    esp_err("board_touch_init", unsafe {
        board_touch_init(Some(touch_interrupt_callback))
    })
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
                        if tx.try_send(TouchEvent::Press(touch)).is_err() {
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
                            if tx.try_send(TouchEvent::Release(touch)).is_err() {
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

unsafe extern "C" fn touch_interrupt_callback(_touch: *mut std::ffi::c_void) {
    TOUCH_NOTIFY.notify_lsb();
}

unsafe extern "C" fn color_transfer_done_callback(
    _panel_io: esp_idf_svc::sys::esp_lcd_panel_io_handle_t,
    _edata: *mut esp_idf_svc::sys::esp_lcd_panel_io_event_data_t,
    _user_ctx: *mut std::ffi::c_void,
) -> bool {
    LCD_COLOR_TRANS_DONE_COUNT.fetch_add(1, Ordering::Relaxed);
    false
}

fn register_color_transfer_done_callback() -> anyhow::Result<()> {
    let panel_io = unsafe { esp_idf_svc::sys::hello::get_panel_io_handle() }
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

    let panel = unsafe { get_panel_handle() };
    let done_start = LCD_COLOR_TRANS_DONE_COUNT.load(Ordering::Relaxed);
    let mut submitted_chunks = 0u32;
    let mut y = y_start;
    let mut offset = 0usize;

    while y < y_end {
        let rows = FLUSH_CHUNK_ROWS.min(y_end - y);
        let len = row_bytes * rows as usize;
        let chunk = &color_data[offset..offset + len];

        let e = unsafe {
            esp_idf_svc::sys::esp_lcd_panel_draw_bitmap(
                panel,
                x_start,
                y,
                x_end,
                y + rows,
                chunk.as_ptr().cast(),
            )
        };
        if e != 0 {
            log::warn!("flush_display error: {}", e);
            return e;
        }

        submitted_chunks = submitted_chunks.saturating_add(1);
        y += rows;
        offset += len;
    }

    if let Err(e) = wait_color_transfer_done(done_start, submitted_chunks) {
        log::warn!("flush_display wait done error: {}", e);
        return e;
    }

    0
}

fn wait_color_transfer_done(start_count: u32, expected_delta: u32) -> Result<(), i32> {
    let deadline = std::time::Instant::now() + FLUSH_DONE_TIMEOUT;
    while LCD_COLOR_TRANS_DONE_COUNT
        .load(Ordering::Relaxed)
        .wrapping_sub(start_count)
        < expected_delta
    {
        if std::time::Instant::now() >= deadline {
            return Err(esp_idf_svc::sys::ESP_ERR_TIMEOUT as i32);
        }
        std::thread::sleep(FLUSH_DONE_POLL_INTERVAL);
    }
    Ok(())
}

fn esp_err(context: &str, code: i32) -> anyhow::Result<()> {
    if code == esp_idf_svc::sys::ESP_OK as i32 {
        Ok(())
    } else {
        Err(anyhow::anyhow!("{context} failed: esp_err_t={code}"))
    }
}
