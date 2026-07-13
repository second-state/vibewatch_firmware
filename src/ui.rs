use embedded_graphics::{
    framebuffer::{buffer_size, Framebuffer},
    image::GetPixel,
    pixelcolor::{
        raw::{BigEndian, RawU16},
        Rgb565,
    },
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle},
    text::{Alignment, Text},
};
use embedded_text::TextBox;
use u8g2_fonts::U8g2TextStyle;

const GIF_IMG: &[u8] = include_bytes!("../assets/ht.gif");

type ColorFormat = Rgb565;

pub fn ui_background() -> Result<(), std::convert::Infallible> {
    let image = tinygif::Gif::<ColorFormat>::from_slice(GIF_IMG).unwrap();

    // Create a new framebuffer
    let mut display = Box::new(Framebuffer::<
        ColorFormat,
        _,
        BigEndian,
        DISPLAY_WIDTH,
        DISPLAY_HEIGHT,
        { buffer_size::<ColorFormat>(DISPLAY_WIDTH, DISPLAY_HEIGHT) },
    >::new());

    display.clear(ColorFormat::WHITE)?;

    for frame in image.frames() {
        frame.draw(display.as_mut())?;
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

const ALPHA: f32 = 0.5;

pub struct UI {
    pub state: String,
    state_area: Rectangle,
    state_background: Vec<Pixel<ColorFormat>>,
    pub text: String,
    text_area: Rectangle,
    text_background: Vec<Pixel<ColorFormat>>,

    pub reset: bool,
    display: Box<
        Framebuffer<
            ColorFormat,
            RawU16,
            BigEndian,
            DISPLAY_WIDTH,
            DISPLAY_HEIGHT,
            { buffer_size::<ColorFormat>(DISPLAY_WIDTH, DISPLAY_HEIGHT) },
        >,
    >,
}

const DISPLAY_WIDTH: usize = 412;
const DISPLAY_HEIGHT: usize = 412;
const COLOR_WIDTH: u32 = 2;

impl Default for UI {
    fn default() -> Self {
        let mut display = Box::new(Framebuffer::<
            ColorFormat,
            _,
            BigEndian,
            DISPLAY_WIDTH,
            DISPLAY_HEIGHT,
            { buffer_size::<ColorFormat>(DISPLAY_WIDTH, DISPLAY_HEIGHT) },
        >::new());

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
