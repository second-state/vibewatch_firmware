use embedded_graphics::{
    draw_target::DrawTarget,
    framebuffer::{buffer_size, Framebuffer},
    geometry::OriginDimensions,
    image::{GetPixel, ImageRaw},
    pixelcolor::{
        raw::{BigEndian, RawData, RawU16},
        Rgb565, RgbColor,
    },
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle},
    text::{Alignment, Text},
};
use embedded_text::TextBox;
use u8g2_fonts::U8g2TextStyle;

const GIF_IMG: &[u8] = include_bytes!("../assets/ht.gif");
const TERMINAL_ANS: &str = include_str!("../../embedded-graphics-terminal/vibetty.ans");

pub type UiColor = Rgb565;
type ColorFormat = UiColor;

#[derive(Debug, Clone)]
struct MyTextStyle {
    font_style: U8g2TextStyle<ColorFormat>,
    vertical_offset: i32,
    bg_color: Option<ColorFormat>,
}

impl embedded_graphics::text::renderer::TextRenderer for MyTextStyle {
    type Color = ColorFormat;

    fn draw_string<D>(
        &self,
        text: &str,
        mut position: Point,
        baseline: embedded_graphics::text::Baseline,
        target: &mut D,
    ) -> Result<Point, D::Error>
    where
        D: DrawTarget<Color = Self::Color>,
    {
        position.y += self.vertical_offset;
        if let Some(bg) = self.bg_color {
            let text_metrics = self.font_style.measure_string(text, position, baseline);
            Rectangle::new(
                position,
                Size::new(text_metrics.bounding_box.size.width + 1, self.line_height()),
            )
            .into_styled(PrimitiveStyleBuilder::new().fill_color(bg).build())
            .draw(target)?;
        }
        self.font_style
            .draw_string(text, position, baseline, target)
    }

    fn draw_whitespace<D>(
        &self,
        width: u32,
        mut position: Point,
        baseline: embedded_graphics::text::Baseline,
        target: &mut D,
    ) -> Result<Point, D::Error>
    where
        D: DrawTarget<Color = Self::Color>,
    {
        position.y += self.vertical_offset;
        if let Some(bg) = self.bg_color {
            Rectangle::new(position, Size::new(width, self.line_height()))
                .into_styled(PrimitiveStyleBuilder::new().fill_color(bg).build())
                .draw(target)?;
        }
        self.font_style
            .draw_whitespace(width, position, baseline, target)
    }

    fn measure_string(
        &self,
        text: &str,
        mut position: Point,
        baseline: embedded_graphics::text::Baseline,
    ) -> embedded_graphics::text::renderer::TextMetrics {
        position.y += self.vertical_offset;
        self.font_style.measure_string(text, position, baseline)
    }

    fn line_height(&self) -> u32 {
        self.font_style.line_height()
    }
}

impl embedded_graphics::text::renderer::CharacterStyle for MyTextStyle {
    type Color = ColorFormat;

    fn set_text_color(&mut self, text_color: Option<Self::Color>) {
        self.font_style
            .set_text_color(Some(text_color.unwrap_or(ColorFormat::CSS_BLACK)));
    }

    fn set_background_color(&mut self, background_color: Option<Self::Color>) {
        self.bg_color = background_color;
    }

    fn set_underline_color(
        &mut self,
        underline_color: embedded_graphics::text::DecorationColor<Self::Color>,
    ) {
        self.font_style.set_underline_color(underline_color);
    }

    fn set_strikethrough_color(
        &mut self,
        strikethrough_color: embedded_graphics::text::DecorationColor<Self::Color>,
    ) {
        self.font_style.set_strikethrough_color(strikethrough_color);
    }
}

fn shifted_text_style(
    font: impl u8g2_fonts::Font,
    color: ColorFormat,
    vertical_offset: i32,
) -> MyTextStyle {
    MyTextStyle {
        font_style: U8g2TextStyle::new(font, color),
        vertical_offset,
        bg_color: None,
    }
}

type DisplayFramebuffer = Framebuffer<
    ColorFormat,
    RawU16,
    BigEndian,
    DISPLAY_WIDTH,
    DISPLAY_HEIGHT,
    { buffer_size::<ColorFormat>(DISPLAY_WIDTH, DISPLAY_HEIGHT) },
>;

struct FastFramebuffer {
    inner: Box<DisplayFramebuffer>,
}

impl FastFramebuffer {
    fn new() -> Self {
        Self {
            inner: Box::new(DisplayFramebuffer::new()),
        }
    }

    fn data(&self) -> &[u8] {
        self.inner.data()
    }

    fn as_image(&self) -> ImageRaw<'_, ColorFormat, BigEndian> {
        self.inner.as_image()
    }

    fn rect_data(&self, rect: Rectangle) -> Option<(Vec<u8>, Rectangle)> {
        let rect = rect.intersection(&self.bounding_box());
        if rect.size.width == 0 || rect.size.height == 0 {
            return None;
        }

        let x = rect.top_left.x.max(0) as usize;
        let y = rect.top_left.y.max(0) as usize;
        let width = rect.size.width as usize;
        let height = rect.size.height as usize;
        let mut out = Vec::with_capacity(width * height * 2);
        let data = self.inner.data();
        for row in y..(y + height) {
            let start = (row * DISPLAY_WIDTH + x) * 2;
            out.extend_from_slice(&data[start..start + width * 2]);
        }
        Some((out, rect))
    }

    fn set_pixel_fast(&mut self, p: Point, color: ColorFormat) {
        if p.x < 0 || p.y < 0 {
            return;
        }
        let x = p.x as usize;
        let y = p.y as usize;
        if x >= DISPLAY_WIDTH || y >= DISPLAY_HEIGHT {
            return;
        }

        let [hi, lo] = rgb565_be(color);
        let index = (y * DISPLAY_WIDTH + x) * 2;
        let data = self.inner.data_mut();
        data[index] = hi;
        data[index + 1] = lo;
    }
}

impl OriginDimensions for FastFramebuffer {
    fn size(&self) -> Size {
        Size::new(DISPLAY_WIDTH as u32, DISPLAY_HEIGHT as u32)
    }
}

impl DrawTarget for FastFramebuffer {
    type Color = ColorFormat;
    type Error = std::convert::Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(p, c) in pixels {
            self.set_pixel_fast(p, c);
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let area = area.intersection(&self.bounding_box());
        if area.size.width == 0 || area.size.height == 0 {
            return Ok(());
        }

        let x_start = area.top_left.x as usize;
        let y_start = area.top_left.y as usize;
        let width = area.size.width as usize;
        let height = area.size.height as usize;
        let raw = RawU16::from(color).into_inner();
        let data = self.inner.data_mut();

        for y in y_start..(y_start + height) {
            let row_start = (y * DISPLAY_WIDTH + x_start) * 2;
            let row = &mut data[row_start..row_start + width * 2];
            fill_rgb565_be_row(row, raw);
        }

        Ok(())
    }
}

fn fill_rgb565_be_row(row: &mut [u8], raw: u16) {
    debug_assert_eq!(row.len() % 2, 0);

    let [hi, lo] = raw.to_be_bytes();
    if row.as_ptr() as usize & 1 == 0 {
        let words = unsafe {
            core::slice::from_raw_parts_mut(row.as_mut_ptr().cast::<u16>(), row.len() / 2)
        };
        words.fill(raw.to_be());
    } else if row.len() >= 2 {
        row[0] = hi;
        row[row.len() - 1] = lo;
        let middle_len = row.len().saturating_sub(2);
        if middle_len > 0 {
            let words = unsafe {
                core::slice::from_raw_parts_mut(
                    row.as_mut_ptr().add(1).cast::<u16>(),
                    middle_len / 2,
                )
            };
            words.fill(raw.to_le());
        }
    } else {
        debug_assert!(row.is_empty());
    }
}

fn rgb565_be(color: ColorFormat) -> [u8; 2] {
    RawU16::from(color).into_inner().to_be_bytes()
}

pub fn ui_background() -> Result<(), std::convert::Infallible> {
    let image = tinygif::Gif::<ColorFormat>::from_slice(GIF_IMG).unwrap();

    // Create a new framebuffer
    let mut display = FastFramebuffer::new();

    display.clear(ColorFormat::WHITE)?;

    for frame in image.frames() {
        frame.draw(&mut display)?;
        crate::lcd::flush_display(
            display.data(),
            0,
            0,
            crate::lcd::LCD_WIDTH as i32,
            crate::lcd::LCD_HEIGHT as i32,
        );
        let delay_ms = frame.delay_centis * 10;
        std::thread::sleep(std::time::Duration::from_millis(delay_ms as u64));
    }

    Ok(())
}

pub fn render_terminal_ans_demo() -> anyhow::Result<()> {
    use embedded_graphics_terminal::TerminalRenderer;
    use u8g2_fonts::fonts::{
        u8g2_font_unifont_t_78_79, u8g2_font_unifont_t_gb2312, u8g2_font_unifont_t_symbols,
    };
    use vt100::Parser;

    let mut display = FastFramebuffer::new();

    let mut renderer = TerminalRenderer::new(
        display.size(),
        u8g2_font_unifont_t_gb2312,
        ColorFormat::WHITE,
        ColorFormat::BLACK,
    )
    .with_fallback_font(u8g2_font_unifont_t_symbols)
    .with_fallback_font(u8g2_font_unifont_t_78_79);
    let (cell_w, cell_h) = renderer.cell_size();
    log::info!(
        "Terminal renderer: {}x{} cells, cell={}x{}, ansi={} bytes",
        renderer.cols(),
        renderer.rows(),
        cell_w,
        cell_h,
        TERMINAL_ANS.len()
    );

    for frame in 1..=2 {
        display.clear(ColorFormat::BLACK)?;

        let parse_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        let mut parser = Parser::new(renderer.rows(), renderer.cols(), 0);
        parser.process(TERMINAL_ANS.as_bytes());
        let parse_elapsed_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() } - parse_start_us;

        let render_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        renderer.render(parser.screen(), &mut display)?;
        let render_elapsed_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() } - render_start_us;
        let total_elapsed_us = parse_elapsed_us + render_elapsed_us;
        log::info!(
            "Terminal frame {} ANSI to framebuffer took {} us ({:.2} ms): parse={} us, render={} us",
            frame,
            total_elapsed_us,
            total_elapsed_us as f32 / 1000.0,
            parse_elapsed_us,
            render_elapsed_us
        );

        let flush_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        let e = crate::lcd::flush_display(
            display.data(),
            0,
            0,
            crate::lcd::LCD_WIDTH as i32,
            crate::lcd::LCD_HEIGHT as i32,
        );
        let flush_elapsed_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() } - flush_start_us;
        log::info!(
            "Terminal frame {} framebuffer flush took {} us ({:.2} ms)",
            frame,
            flush_elapsed_us,
            flush_elapsed_us as f32 / 1000.0
        );
        if e != 0 {
            log::warn!("flush terminal demo frame {frame} error: {e}");
        }
    }

    Ok(())
}

fn new_terminal_renderer() -> embedded_graphics_terminal::TerminalRenderer {
    use embedded_graphics_terminal::TerminalRenderer;
    use u8g2_fonts::fonts::{
        u8g2_font_unifont_t_78_79, u8g2_font_unifont_t_gb2312, u8g2_font_unifont_t_symbols,
    };

    TerminalRenderer::new(
        Size::new(DISPLAY_WIDTH as u32, DISPLAY_HEIGHT as u32),
        u8g2_font_unifont_t_gb2312,
        ColorFormat::WHITE,
        ColorFormat::BLACK,
    )
    .with_fallback_font(u8g2_font_unifont_t_symbols)
    .with_fallback_font(u8g2_font_unifont_t_78_79)
    .with_substitution('›', '>')
    .with_substitution('•', '*')
    .with_substitution('✻', '*')
    .with_substitution('⏺', '*')
}

pub fn terminal_text_cells() -> (u16, u16) {
    let renderer = new_terminal_renderer();
    (renderer.cols() as u16, renderer.rows() as u16)
}

const ALPHA: f32 = 0.5;
const MENU_ITEM_H: u16 = 66;
const MENU_START_Y: u16 = 30;
const MENU_FONT_H: u16 = 17;
const TERMINAL_SCROLL_ROWS: usize = 10;
const TERMINAL_SCROLLBACK_ROWS: usize = 64;

#[derive(Clone)]
pub struct ListItem {
    pub rect: Rectangle,
    pub text: String,
    pub bg_color: Option<UiColor>,
    pub fg_color: Option<UiColor>,
}

impl ListItem {
    pub fn new(
        rect: Rectangle,
        text: impl Into<String>,
        bg_color: Option<UiColor>,
        fg_color: Option<UiColor>,
    ) -> Self {
        Self {
            rect,
            text: text.into(),
            bg_color,
            fg_color,
        }
    }
}

pub enum MainMenuSelection {
    Remote,
    Setting,
}

pub enum SettingMenuSelection {
    Ota,
    Ble,
    Back,
}

pub enum TerminalScroll {
    Up,
    Down,
}

pub struct UI {
    state: String,
    state_area: Rectangle,
    state_background: Vec<Pixel<ColorFormat>>,
    text: String,
    text_area: Rectangle,
    text_background: Vec<Pixel<ColorFormat>>,

    display: Box<FastFramebuffer>,
    terminal_parser: Option<vt100::Parser>,
    terminal_renderer: Option<embedded_graphics_terminal::TerminalRenderer>,
    jpeg_screen: Option<crate::new_jpg::JpegBufferu16>,
}

const DISPLAY_WIDTH: usize = crate::lcd::LCD_WIDTH as usize;
const DISPLAY_HEIGHT: usize = crate::lcd::LCD_HEIGHT as usize;

pub async fn main_menu(
    gui: &mut UI,
    touch_rx: &mut tokio::sync::mpsc::Receiver<crate::lcd::TouchEvent>,
) -> anyhow::Result<MainMenuSelection> {
    let items = vec![
        ("Remote".to_string(), false),
        ("Setting".to_string(), false),
    ];
    let index = select_menu_item(gui, touch_rx, "Main Menu", &items).await?;
    Ok(match index {
        0 => MainMenuSelection::Remote,
        1 => MainMenuSelection::Setting,
        _ => unreachable!(),
    })
}

pub async fn setting_menu(
    gui: &mut UI,
    touch_rx: &mut tokio::sync::mpsc::Receiver<crate::lcd::TouchEvent>,
) -> anyhow::Result<SettingMenuSelection> {
    let items = vec![
        ("OTA Update".to_string(), false),
        ("Enable BLE".to_string(), false),
        ("Back".to_string(), false),
    ];
    let index = select_menu_item(gui, touch_rx, "Setting", &items).await?;
    Ok(match index {
        0 => SettingMenuSelection::Ota,
        1 => SettingMenuSelection::Ble,
        2 => SettingMenuSelection::Back,
        _ => unreachable!(),
    })
}

pub async fn select_menu_item(
    gui: &mut UI,
    touch_rx: &mut tokio::sync::mpsc::Receiver<crate::lcd::TouchEvent>,
    title: &str,
    items: &[(String, bool)],
) -> anyhow::Result<usize> {
    let item_rects = gui.display_menu_list(title, items)?;
    log::info!("{title}: waiting for touch selection");

    let mut press_index = None;
    loop {
        match touch_rx.recv().await {
            Some(crate::lcd::TouchEvent::Press(touch)) => {
                if press_index.is_none() {
                    press_index = list_touch_index(touch, &item_rects);
                }
            }
            Some(crate::lcd::TouchEvent::Release(touch)) => {
                let release_index = list_touch_index(touch, &item_rects);
                if press_index.is_some() && press_index == release_index {
                    let index = press_index.unwrap();
                    log::info!("{title}: selected item {index}");
                    return Ok(index);
                }
                log::info!(
                    "{title}: ignored touch, press={:?} release={:?}",
                    press_index,
                    release_index
                );
                press_index = None;
            }
            None => return Err(anyhow::anyhow!("touch event source closed")),
        }
    }
}

pub fn list_touch_index(touch: crate::lcd::TouchPoint, item_rects: &[Rectangle]) -> Option<usize> {
    let x = touch.x as i32;
    let y = touch.y as i32;
    item_rects.iter().position(|rect| {
        let left = rect.top_left.x;
        let top = rect.top_left.y;
        let right = left + rect.size.width as i32;
        let bottom = top + rect.size.height as i32;
        x >= left && x < right && y >= top && y < bottom
    })
}

impl Default for UI {
    fn default() -> Self {
        let mut display = Box::new(FastFramebuffer::new());

        display.clear(ColorFormat::WHITE).unwrap();

        let state_area = Rectangle::new(
            display.bounding_box().center() + Point::new(-150, 150),
            Size::new(300, 15 * 2),
        );
        let text_area = Rectangle::new(
            display.bounding_box().center() + Point::new(-150, 50),
            Size::new(300, 50 * 2),
        );

        let image = tinygif::Gif::<ColorFormat>::from_slice(GIF_IMG).unwrap();
        for frame in image.frames() {
            frame.draw(display.as_mut()).unwrap();
        }

        let img = display.as_image();

        let state_pixels: Vec<Pixel<ColorFormat>> = state_area
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .stroke_color(ColorFormat::CSS_DARK_BLUE)
                    .stroke_width(1)
                    .fill_color(ColorFormat::CSS_DARK_BLUE)
                    .build(),
            )
            .pixels()
            .map(|p| {
                if let Some(color) = img.pixel(p.0) {
                    Pixel(p.0, alpha_mix(color, p.1, ALPHA))
                } else {
                    p
                }
            })
            .collect();

        let box_pixels: Vec<Pixel<ColorFormat>> = text_area
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .stroke_color(ColorFormat::CSS_BLACK)
                    .stroke_width(5)
                    .fill_color(ColorFormat::CSS_BLACK)
                    .build(),
            )
            .pixels()
            .map(|p| {
                if let Some(color) = img.pixel(p.0) {
                    Pixel(p.0, alpha_mix(color, p.1, ALPHA))
                } else {
                    p
                }
            })
            .collect();

        Self {
            state: String::new(),
            state_background: state_pixels,
            text: String::new(),
            text_background: box_pixels,
            display,
            terminal_parser: None,
            terminal_renderer: None,
            jpeg_screen: None,
            state_area,
            text_area,
        }
    }
}

fn alpha_mix(source: ColorFormat, target: ColorFormat, alpha: f32) -> ColorFormat {
    ColorFormat::new(
        ((1. - alpha) * source.r() as f32 + alpha * target.r() as f32) as u8,
        ((1. - alpha) * source.g() as f32 + alpha * target.g() as f32) as u8,
        ((1. - alpha) * source.b() as f32 + alpha * target.b() as f32) as u8,
    )
}

impl UI {
    pub fn show_status(
        &mut self,
        state: impl Into<String>,
        text: impl Into<String>,
    ) -> anyhow::Result<()> {
        self.state = state.into();
        self.text = text.into();
        self.display_flush()
    }

    /// ASR text editor, adapted from vibekeys_firmware's black TUI-style editor.
    pub fn show_asr_editor(&mut self, text: &str, hint: &str) -> anyhow::Result<()> {
        let display = self.display.as_mut();
        display.clear(ColorFormat::CSS_BLACK)?;

        let outer = Rectangle::new(
            Point::new(2, 2),
            Size::new(
                (DISPLAY_WIDTH as u32).saturating_sub(4),
                (DISPLAY_HEIGHT as u32).saturating_sub(4),
            ),
        );
        outer
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .stroke_color(ColorFormat::CSS_WHITE)
                    .stroke_width(1)
                    .build(),
            )
            .draw(display)?;

        let top_h = 80;
        let top_labels = ["Left", "Del", "Right"];
        for (i, label) in top_labels.iter().enumerate() {
            let x = (DISPLAY_WIDTH / 3 * i) as i32;
            let w = if i == 2 {
                DISPLAY_WIDTH - DISPLAY_WIDTH / 3 * 2
            } else {
                DISPLAY_WIDTH / 3
            };
            let rect = Rectangle::new(Point::new(x + 6, 10), Size::new(w as u32 - 12, 54));
            rect.into_styled(
                PrimitiveStyleBuilder::new()
                    .stroke_color(ColorFormat::CSS_WHEAT)
                    .stroke_width(1)
                    .build(),
            )
            .draw(display)?;
            Text::with_alignment(
                label,
                rect.center() + Point::new(0, 6),
                shifted_text_style(
                    u8g2_fonts::fonts::u8g2_font_wqy12_t_gb2312a,
                    ColorFormat::CSS_WHEAT,
                    3,
                ),
                Alignment::Center,
            )
            .draw(display)?;
        }

        let enter_top = DISPLAY_HEIGHT as i32 - 80;
        let content_rect = Rectangle::new(
            Point::new(12, top_h),
            Size::new(
                (DISPLAY_WIDTH as u32).saturating_sub(24),
                (enter_top - top_h - 8).max(24) as u32,
            ),
        );
        let content_style = embedded_text::style::TextBoxStyleBuilder::new()
            .height_mode(embedded_text::style::HeightMode::FitToText)
            .alignment(embedded_text::alignment::HorizontalAlignment::Left)
            .line_height(embedded_graphics::text::LineHeight::Pixels(24))
            .paragraph_spacing(12)
            .build();
        TextBox::with_textbox_style(
            text,
            content_rect,
            shifted_text_style(
                u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312,
                ColorFormat::CSS_WHITE,
                3,
            ),
            content_style,
        )
        .draw(display)?;

        let record_rect = Rectangle::new(
            Point::new(12, enter_top + 8),
            Size::new((DISPLAY_WIDTH as u32).saturating_sub(24), 60),
        );
        record_rect
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .stroke_color(ColorFormat::CSS_WHEAT)
                    .stroke_width(2)
                    .build(),
            )
            .draw(display)?;

        let hint_rect = Rectangle::new(
            Point::new(12, enter_top + 22),
            Size::new((DISPLAY_WIDTH as u32).saturating_sub(24), 32),
        );
        let hint_style = embedded_text::style::TextBoxStyleBuilder::new()
            .height_mode(embedded_text::style::HeightMode::FitToText)
            .alignment(embedded_text::alignment::HorizontalAlignment::Center)
            .line_height(embedded_graphics::text::LineHeight::Pixels(20))
            .build();
        TextBox::with_textbox_style(
            &format!("Record  {hint}"),
            hint_rect,
            shifted_text_style(
                u8g2_fonts::fonts::u8g2_font_wqy12_t_gb2312a,
                ColorFormat::CSS_WHEAT,
                3,
            ),
            hint_style,
        )
        .draw(display)?;

        for i in 0..5 {
            let e = crate::lcd::flush_display(
                self.display.data(),
                0,
                0,
                DISPLAY_WIDTH as i32,
                DISPLAY_HEIGHT as i32,
            );
            if e == 0 {
                return Ok(());
            }
            log::warn!("flush asr editor error: {e} retry {i}");
        }
        Err(anyhow::anyhow!("flush asr editor failed"))
    }

    fn flush_terminal_dirty(&self, rect: Rectangle) -> anyhow::Result<Option<i64>> {
        let Some((data, rect)) = self.display.rect_data(rect) else {
            return Ok(None);
        };

        let flush_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        for i in 0..5 {
            let e = crate::lcd::flush_display(
                &data,
                rect.top_left.x,
                rect.top_left.y,
                rect.top_left.x + rect.size.width as i32,
                rect.top_left.y + rect.size.height as i32,
            );
            if e == 0 {
                let flush_elapsed_us =
                    unsafe { esp_idf_svc::sys::esp_timer_get_time() } - flush_start_us;
                return Ok(Some(flush_elapsed_us));
            }
            log::warn!("flush terminal dirty rect error: {e} retry {i}");
        }
        Err(anyhow::anyhow!("flush terminal dirty rect failed"))
    }

    fn flush_terminal_full(&self) -> anyhow::Result<i64> {
        let flush_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        for i in 0..5 {
            let e = crate::lcd::flush_display(
                self.display.data(),
                0,
                0,
                DISPLAY_WIDTH as i32,
                DISPLAY_HEIGHT as i32,
            );
            if e == 0 {
                return Ok(unsafe { esp_idf_svc::sys::esp_timer_get_time() } - flush_start_us);
            }
            log::warn!("flush terminal full frame error: {e} retry {i}");
        }
        Err(anyhow::anyhow!("flush terminal full frame failed"))
    }

    pub fn show_terminal_text_frame(&mut self, payload: &[u8]) -> anyhow::Result<()> {
        let Some((&tag, bytes)) = payload.split_first() else {
            log::warn!("empty screen_text frame");
            return Ok(());
        };
        let (cols, rows) = terminal_text_cells();
        let full_frame = tag == 0x00;
        match tag {
            0x00 => {
                log::info!("screen_text full frame: {}B", bytes.len());
                self.terminal_parser =
                    Some(vt100::Parser::new(rows, cols, TERMINAL_SCROLLBACK_ROWS));
            }
            0x01 => {
                log::debug!("screen_text delta frame: {}B", bytes.len());
                if self.terminal_parser.is_none() {
                    log::warn!("screen_text delta before full frame; creating blank terminal");
                    self.terminal_parser =
                        Some(vt100::Parser::new(rows, cols, TERMINAL_SCROLLBACK_ROWS));
                    if let Some(renderer) = self.terminal_renderer.as_mut() {
                        renderer.invalidate();
                    }
                }
            }
            other => {
                log::warn!("unknown screen_text tag: {other}");
                return Ok(());
            }
        }

        let parse_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        if let Some(parser) = self.terminal_parser.as_mut() {
            parser.process(bytes);
        }
        let parse_elapsed_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() } - parse_start_us;

        let mut renderer = self
            .terminal_renderer
            .take()
            .unwrap_or_else(new_terminal_renderer);
        let render_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        let dirty = if full_frame {
            self.display.clear(ColorFormat::CSS_BLACK)?;
            if let Some(parser) = self.terminal_parser.as_ref() {
                renderer.render(parser.screen(), self.display.as_mut())?;
            }
            renderer.invalidate();
            Some(self.display.bounding_box())
        } else {
            match self.terminal_parser.as_ref() {
                Some(parser) => renderer.render_diff(parser.screen(), self.display.as_mut())?,
                None => None,
            }
        };
        let render_elapsed_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() } - render_start_us;
        let cache_len = renderer.cache_len();
        self.terminal_renderer = Some(renderer);

        let flush_elapsed_us = match (full_frame, dirty) {
            (true, Some(_)) => self.flush_terminal_full()?,
            (false, Some(rect)) => self.flush_terminal_dirty(rect)?.unwrap_or(0),
            (_, None) => 0,
        };
        log::info!(
            "screen_text frame tag=0x{tag:02x} bytes={} parse={:.2}ms render={:.2}ms flush={:.2}ms cache_len={} dirty={:?}",
            bytes.len(),
            parse_elapsed_us as f32 / 1000.0,
            render_elapsed_us as f32 / 1000.0,
            flush_elapsed_us as f32 / 1000.0,
            cache_len,
            dirty
        );
        Ok(())
    }

    pub fn scroll_terminal_text(&mut self, direction: TerminalScroll) -> anyhow::Result<bool> {
        let Some(parser) = self.terminal_parser.as_mut() else {
            return Ok(false);
        };
        let before = parser.screen().scrollback();
        let next = match direction {
            TerminalScroll::Up => before.saturating_add(TERMINAL_SCROLL_ROWS),
            TerminalScroll::Down => before.saturating_sub(TERMINAL_SCROLL_ROWS),
        };
        parser.screen_mut().set_scrollback(next);
        let after = parser.screen().scrollback();
        if after == before {
            return Ok(false);
        }

        log::info!("local text scroll: {before} -> {after}");
        let mut renderer = self
            .terminal_renderer
            .take()
            .unwrap_or_else(new_terminal_renderer);
        let dirty = renderer.render_diff(parser.screen(), self.display.as_mut())?;
        log::info!("local text scroll cache_len={}", renderer.cache_len());
        self.terminal_renderer = Some(renderer);
        if let Some(rect) = dirty {
            let _ = self.flush_terminal_dirty(rect)?;
        }
        Ok(true)
    }

    pub fn redraw_cached_terminal_text(&mut self) -> anyhow::Result<bool> {
        let Some(parser) = self.terminal_parser.as_ref() else {
            return Ok(false);
        };

        let render_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        let mut renderer = self
            .terminal_renderer
            .take()
            .unwrap_or_else(new_terminal_renderer);
        self.display.clear(ColorFormat::CSS_BLACK)?;
        renderer.render(parser.screen(), self.display.as_mut())?;
        renderer.invalidate();
        let render_elapsed_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() } - render_start_us;
        let cache_len = renderer.cache_len();
        self.terminal_renderer = Some(renderer);

        let flush_elapsed_us = self.flush_terminal_full()?;
        log::info!(
            "redraw cached terminal text render={:.2}ms flush={:.2}ms cache_len={}",
            render_elapsed_us as f32 / 1000.0,
            flush_elapsed_us as f32 / 1000.0,
            cache_len
        );
        Ok(true)
    }

    pub fn show_jpeg_screen(
        &mut self,
        screen: crate::new_jpg::JpegBufferu16,
    ) -> anyhow::Result<()> {
        screen.flush_to_lcd()?;
        self.jpeg_screen = Some(screen);
        Ok(())
    }

    pub fn redraw_cached_jpeg_screen(&self) -> anyhow::Result<bool> {
        let Some(screen) = self.jpeg_screen.as_ref() else {
            return Ok(false);
        };
        screen.flush_to_lcd()?;
        Ok(true)
    }

    // 横向42个字符
    fn display_flush(&mut self) -> anyhow::Result<()> {
        let image = tinygif::Gif::<ColorFormat>::from_slice(GIF_IMG).unwrap();
        for frame in image.frames() {
            frame.draw(self.display.as_mut())?;
        }

        self.state_background
            .iter()
            .cloned()
            .draw(self.display.as_mut())?;
        self.text_background
            .iter()
            .cloned()
            .draw(self.display.as_mut())?;

        Text::with_alignment(
            &self.state,
            self.state_area.center(),
            U8g2TextStyle::new(
                u8g2_fonts::fonts::u8g2_font_wqy12_t_gb2312a,
                ColorFormat::CSS_LIGHT_CYAN,
            ),
            Alignment::Center,
        )
        .draw(self.display.as_mut())?;

        let textbox_style = embedded_text::style::TextBoxStyleBuilder::new()
            .height_mode(embedded_text::style::HeightMode::FitToText)
            .alignment(embedded_text::alignment::HorizontalAlignment::Center)
            .line_height(embedded_graphics::text::LineHeight::Pixels(20))
            .paragraph_spacing(20)
            .build();
        let text_box = TextBox::with_textbox_style(
            &self.text,
            self.text_area,
            U8g2TextStyle::new(
                u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312,
                ColorFormat::CSS_WHEAT,
            ),
            textbox_style,
        );
        text_box.draw(self.display.as_mut())?;

        for i in 0..5 {
            let e = crate::lcd::flush_display(
                self.display.data(),
                0,
                0,
                DISPLAY_WIDTH as _,
                DISPLAY_HEIGHT as _,
            );
            if e == 0 {
                break;
            }
            log::warn!("flush_display error: {} retry {i}", e);
        }
        Ok(())
    }

    /// Render generic list items. Each item owns its geometry and optional colors.
    pub fn display_list(
        &mut self,
        title: &str,
        items: &[ListItem],
    ) -> anyhow::Result<Vec<Rectangle>> {
        crate::lcd::clear();

        let display = self.display.as_mut();
        display.clear(ColorFormat::WHITE)?;

        Text::with_alignment(
            title,
            Point::new((DISPLAY_WIDTH / 2) as i32, 18),
            U8g2TextStyle::new(
                u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312,
                ColorFormat::CSS_DARK_BLUE,
            ),
            Alignment::Center,
        )
        .draw(display)?;

        let mut item_rects = Vec::new();
        for item in items {
            if item.rect.top_left.y + item.rect.size.height as i32 > DISPLAY_HEIGHT as i32 {
                break;
            }

            item_rects.push(item.rect);
            let draw_rect = Rectangle::new(
                item.rect.top_left + Point::new(4, 3),
                Size::new(
                    item.rect.size.width.saturating_sub(8),
                    item.rect.size.height.saturating_sub(6),
                ),
            );
            let mut style = PrimitiveStyleBuilder::new()
                .stroke_color(ColorFormat::CSS_BLACK)
                .stroke_width(8);
            if let Some(bg_color) = item.bg_color {
                style = style.fill_color(bg_color);
            }
            draw_rect.into_styled(style.build()).draw(display)?;
            if let Some(fg_color) = item.fg_color {
                let text_y =
                    draw_rect.top_left.y + (draw_rect.size.height as i32 + MENU_FONT_H as i32) / 2;
                Text::with_alignment(
                    &item.text,
                    Point::new(draw_rect.center().x, text_y),
                    shifted_text_style(u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312, fg_color, 3),
                    Alignment::Center,
                )
                .draw(display)?;
            }
        }

        log::info!(
            "display_list: title={:?}, items={}, rects={}",
            title,
            items.len(),
            item_rects.len()
        );
        for i in 0..5 {
            let e = crate::lcd::flush_display(
                self.display.data(),
                0,
                0,
                DISPLAY_WIDTH as i32,
                DISPLAY_HEIGHT as i32,
            );
            if e == 0 {
                log::info!("display_list flush ok");
                return Ok(item_rects);
            }
            log::warn!("flush list error: {e} retry {i}");
        }
        Err(anyhow::anyhow!("flush list failed"))
    }

    /// Compatibility helper for the existing menu/session list layout.
    pub fn display_menu_list(
        &mut self,
        title: &str,
        items: &[(String, bool)],
    ) -> anyhow::Result<Vec<Rectangle>> {
        let list_items: Vec<ListItem> = items
            .iter()
            .enumerate()
            .filter_map(|(i, (label, is_working))| {
                let item_top = MENU_START_Y as i32 + (i as i32) * MENU_ITEM_H as i32;
                if item_top + MENU_ITEM_H as i32 > DISPLAY_HEIGHT as i32 {
                    return None;
                }

                let rect = Rectangle::new(
                    Point::new(0, item_top),
                    Size::new(DISPLAY_WIDTH as u32, MENU_ITEM_H as u32),
                );
                let bg_color = if *is_working {
                    ColorFormat::CSS_DARK_BLUE
                } else {
                    ColorFormat::CSS_DARK_ORANGE
                };
                Some(ListItem::new(
                    rect,
                    label.clone(),
                    Some(bg_color),
                    Some(ColorFormat::CSS_WHITE),
                ))
            })
            .collect();

        self.display_list(title, &list_items)
    }
}
