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

pub struct UI {
    pub state: String,
    state_area: Rectangle,
    state_background: Vec<Pixel<ColorFormat>>,
    pub text: String,
    text_area: Rectangle,
    text_background: Vec<Pixel<ColorFormat>>,

    pub reset: bool,
    display: Box<FastFramebuffer>,
}

const DISPLAY_WIDTH: usize = crate::lcd::LCD_WIDTH as usize;
const DISPLAY_HEIGHT: usize = crate::lcd::LCD_HEIGHT as usize;
const COLOR_WIDTH: u32 = 2;

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
            reset: false,
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

fn flush_area<const COLOR_WIDTH: u32>(data: &[u8], size: Size, area: Rectangle) -> i32 {
    let start_y = area.top_left.y as u32;
    let end_y = start_y + area.size.height;

    let start_index = start_y * size.width * COLOR_WIDTH;
    let data_len = area.size.height * size.width * COLOR_WIDTH;
    if let Some(area_data) = data.get(start_index as usize..(start_index + data_len) as usize) {
        crate::lcd::flush_display(
            area_data,
            0,
            start_y as i32,
            size.width as i32,
            end_y as i32,
        )
    } else {
        -1
    }
}

impl UI {
    // 横向42个字符
    pub fn display_flush(&mut self) -> anyhow::Result<()> {
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

        if !self.reset {
            // let lines = self.text.lines().count();
            // Text::with_alignment(
            //     &self.text,
            //     self.text_area.center() + Point::new(0, -6 * (lines as i32)),
            //     U8g2TextStyle::new(
            //         u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312b,
            //         ColorFormat::CSS_WHEAT,
            //     ),
            //     Alignment::Center,
            // )
            // .draw(self.display.as_mut())?;
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
        } else {
            Text::with_alignment(
                &format!("Do you want to reset the device?\n[yes] or [no]"),
                self.text_area.center(),
                U8g2TextStyle::new(
                    u8g2_fonts::fonts::u8g2_font_unifont_t_gb2312b,
                    ColorFormat::CSS_SANDY_BROWN,
                ),
                Alignment::Center,
            )
            .draw(self.display.as_mut())?;
        }

        for i in 0..5 {
            // let e = crate::lcd::flush_display(
            //     self.display.data(),
            //     0,
            //     0,
            //     DISPLAY_WIDTH as _,
            //     DISPLAY_HEIGHT as _,
            // );

            let e = flush_area::<COLOR_WIDTH>(
                self.display.data(),
                self.display.size(),
                Rectangle::new(
                    self.text_area.top_left,
                    Size::new(
                        self.text_area.size.width,
                        self.text_area.size.height + self.state_area.size.height,
                    ),
                ),
            );
            if e == 0 {
                break;
            }
            log::warn!("flush_display error: {} retry {i}", e);
        }
        Ok(())
    }

    /// 渲染 vibetty 会话列表:标题 + 每行一个会话标签(焦点行蓝底)。
    /// `items` = (标签, is_working);`focus` = 焦点行。整屏 flush。
    pub fn display_session_list(
        &mut self,
        title: &str,
        items: &[(String, bool)],
        focus: usize,
    ) -> anyhow::Result<()> {
        let display = self.display.as_mut();
        display.clear(ColorFormat::WHITE)?;

        Text::with_alignment(
            title,
            Point::new(8, 18),
            U8g2TextStyle::new(
                u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312,
                ColorFormat::CSS_DARK_BLUE,
            ),
            Alignment::Left,
        )
        .draw(display)?;

        let item_h: i32 = 22;
        let start_y: i32 = 30;
        for (i, (label, is_working)) in items.iter().enumerate() {
            let y = start_y + (i as i32) * item_h;
            if y + item_h > DISPLAY_HEIGHT as i32 {
                break;
            }
            if i == focus {
                Rectangle::new(
                    Point::new(0, y - 17),
                    Size::new(DISPLAY_WIDTH as u32, item_h as u32),
                )
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
            Text::with_alignment(
                label,
                Point::new(10, y),
                U8g2TextStyle::new(u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312, color),
                Alignment::Left,
            )
            .draw(display)?;
        }

        let e = crate::lcd::flush_display(
            self.display.data(),
            0,
            0,
            DISPLAY_WIDTH as i32,
            DISPLAY_HEIGHT as i32,
        );
        if e != 0 {
            log::warn!("flush session list error: {e}");
        }
        Ok(())
    }
}
