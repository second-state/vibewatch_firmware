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

type ColorFormat = Rgb565;

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

const ALPHA: f32 = 0.5;
const MENU_ITEM_H: u16 = 66;
const MENU_START_Y: u16 = 30;
const MENU_FONT_H: u16 = 17;

pub enum MainMenuSelection {
    Remote,
    Setting,
}

pub enum SettingMenuSelection {
    Ota,
    Back,
}

pub struct UI {
    state: String,
    state_area: Rectangle,
    state_background: Vec<Pixel<ColorFormat>>,
    text: String,
    text_area: Rectangle,
    text_background: Vec<Pixel<ColorFormat>>,

    display: Box<FastFramebuffer>,
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
        ("Back".to_string(), false),
    ];
    let index = select_menu_item(gui, touch_rx, "Setting", &items).await?;
    Ok(match index {
        0 => SettingMenuSelection::Ota,
        1 => SettingMenuSelection::Back,
        _ => unreachable!(),
    })
}

pub async fn select_menu_item(
    gui: &mut UI,
    touch_rx: &mut tokio::sync::mpsc::Receiver<crate::lcd::TouchEvent>,
    title: &str,
    items: &[(String, bool)],
) -> anyhow::Result<usize> {
    let item_rects = gui.display_list(title, items, 0)?;
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

    /// 渲染通用列表:标题 + 每行一个标签(焦点行蓝底)。
    /// `items` = (标签, is_working);`focus` = 焦点行。整屏 flush。
    pub fn display_list(
        &mut self,
        title: &str,
        items: &[(String, bool)],
        focus: usize,
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
        for (i, (label, is_working)) in items.iter().enumerate() {
            let item_top = MENU_START_Y as i32 + (i as i32) * MENU_ITEM_H as i32;
            if item_top + MENU_ITEM_H as i32 > DISPLAY_HEIGHT as i32 {
                break;
            }
            let item_rect = Rectangle::new(
                Point::new(0, item_top),
                Size::new(DISPLAY_WIDTH as u32, MENU_ITEM_H as u32),
            );
            item_rects.push(item_rect);
            if i == focus {
                item_rect
                    .into_styled(
                        PrimitiveStyleBuilder::new()
                            .fill_color(ColorFormat::CSS_DARK_BLUE)
                            .stroke_color(ColorFormat::CSS_DARK_BLUE)
                            .stroke_width(1)
                            .build(),
                    )
                    .draw(display)?;
            }
            let color = if *is_working {
                ColorFormat::CSS_WHITE
            } else {
                ColorFormat::CSS_DARK_ORANGE
            };
            let text_y = item_top + (MENU_ITEM_H as i32 + MENU_FONT_H as i32) / 2;
            Text::with_alignment(
                label,
                Point::new((DISPLAY_WIDTH / 2) as i32, text_y),
                U8g2TextStyle::new(u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312, color),
                Alignment::Center,
            )
            .draw(display)?;
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
}
