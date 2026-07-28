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
    primitives::{Line, PrimitiveStyleBuilder, Rectangle},
    text::{Alignment, Text},
};
use embedded_text::TextBox;
use std::sync::{
    atomic::{AtomicI32, AtomicUsize, Ordering},
    OnceLock,
};
use u8g2_fonts::U8g2TextStyle;

const GIF_IMG: &[u8] = include_bytes!("../assets/ht.gif");

pub type UiColor = Rgb565;
type ColorFormat = UiColor;
type TerminalRenderer = embedded_graphics_terminal::TerminalRenderer;
pub const TEXT_LIGHT: UiColor = UiColor::CSS_LIGHT_GRAY;

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

pub async fn ui_background() -> Result<(), std::convert::Infallible> {
    let image = tinygif::Gif::<ColorFormat>::from_slice(GIF_IMG).unwrap();

    // Create a new framebuffer
    let mut display = FastFramebuffer::new();

    display.clear(ColorFormat::WHITE)?;

    for frame in image.frames() {
        frame.draw(&mut display)?;
        crate::lcd::async_flush_display(
            display.data(),
            0,
            0,
            crate::lcd::LCD_WIDTH as i32,
            crate::lcd::LCD_HEIGHT as i32,
        )
        .await;
        let delay_ms = frame.delay_centis * 10;
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms as u64)).await;
    }

    Ok(())
}

fn new_terminal_renderer() -> TerminalRenderer {
    use embedded_graphics_terminal::TerminalRenderer;
    use u8g2_fonts::fonts::{
        u8g2_font_unifont_t_78_79, u8g2_font_unifont_t_gb2312, u8g2_font_unifont_t_symbols,
    };

    TerminalRenderer::new(
        Size::new(DISPLAY_WIDTH as u32, DISPLAY_HEIGHT as u32),
        u8g2_font_unifont_t_gb2312,
        TEXT_LIGHT,
        ColorFormat::BLACK,
    )
    .with_theme(TerminalTheme::current().theme())
    .with_fallback_font(u8g2_font_unifont_t_symbols)
    .with_fallback_font(u8g2_font_unifont_t_78_79)
    .with_substitution('›', '>')
    .with_substitution('•', '*')
    .with_substitution('✻', '*')
    .with_substitution('⏺', '*')
}

pub fn terminal_text_cells() -> (u16, u16) {
    *TERMINAL_TEXT_CELLS.get_or_init(|| {
        let renderer = new_terminal_renderer();
        (renderer.cols() as u16, renderer.rows() as u16)
    })
}

pub fn terminal_theme_label() -> &'static str {
    TerminalTheme::current().label()
}

pub fn terminal_theme_count() -> usize {
    TerminalTheme::ALL.len()
}

pub fn terminal_theme_label_at(index: usize) -> &'static str {
    TerminalTheme::from_index(index).label()
}

pub fn current_terminal_theme_index() -> usize {
    TerminalTheme::current().index()
}

const ALPHA: f32 = 0.5;
const MENU_START_Y: u16 = 30;
const MENU_COLUMNS: usize = 2;
const MENU_ROWS: u16 = 4;
const MENU_COLUMN_GAP: i32 = 4;
const MENU_FONT_H: u16 = 17;
const MENU_FOOTER_H: i32 = 24;
const MENU_ITEM_H: u16 = (DISPLAY_HEIGHT as u16 - MENU_START_Y - MENU_FOOTER_H as u16) / MENU_ROWS;
pub const MENU_TITLE_REFRESH_DELAY: std::time::Duration = std::time::Duration::from_secs(60);
const TERMINAL_APPEND_RENDER_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(300);

fn build_version_label() -> &'static str {
    option_env!("VIBEKEYS_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}
const TERMINAL_SCROLLBACK_ROWS: usize = 16;
static TERMINAL_THEME_INDEX: AtomicUsize = AtomicUsize::new(0);
static TERMINAL_TEXT_CELLS: OnceLock<(u16, u16)> = OnceLock::new();

struct TerminalSession {
    parser: vt100::Parser,
    renderer: TerminalRenderer,
}

struct TerminalState {
    cols: u16,
    rows: u16,
    session: Option<TerminalSession>,
    last_render_us: i64,
    append_render_deadline: Option<tokio::time::Instant>,
}

impl TerminalState {
    fn new() -> Self {
        let (cols, rows) = terminal_text_cells();
        Self {
            cols,
            rows,
            session: None,
            last_render_us: 0,
            append_render_deadline: None,
        }
    }

    fn reset_session(&mut self) {
        let renderer = new_terminal_renderer();
        self.cols = renderer.cols() as u16;
        self.rows = renderer.rows() as u16;
        self.session = Some(TerminalSession {
            parser: vt100::Parser::new(self.rows, self.cols, TERMINAL_SCROLLBACK_ROWS),
            renderer,
        });
    }

    fn ensure_session(&mut self) -> &mut TerminalSession {
        if self.session.is_none() {
            self.reset_session();
        }
        self.session.as_mut().expect("terminal session initialized")
    }

    fn set_theme(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.renderer = new_terminal_renderer();
            session.renderer.invalidate();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalTheme {
    Default,
    Sequoia,
    SolarizedDark,
    SolarizedLight,
    Dracula,
    GithubDark,
    Monokai,
    Aura,
}

impl TerminalTheme {
    const ALL: [Self; 8] = [
        Self::Default,
        Self::Sequoia,
        Self::SolarizedDark,
        Self::SolarizedLight,
        Self::Dracula,
        Self::GithubDark,
        Self::Monokai,
        Self::Aura,
    ];

    fn current() -> Self {
        Self::ALL[TERMINAL_THEME_INDEX.load(Ordering::Relaxed) % Self::ALL.len()]
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|theme| *theme == self)
            .unwrap_or(0)
    }

    fn from_index(index: usize) -> Self {
        Self::ALL[index % Self::ALL.len()]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Sequoia => "Sequoia",
            Self::SolarizedDark => "Solarized",
            Self::SolarizedLight => "Solarized Light",
            Self::Dracula => "Dracula",
            Self::GithubDark => "GitHub",
            Self::Monokai => "Monokai",
            Self::Aura => "Aura",
        }
    }

    fn theme(self) -> embedded_graphics_terminal::Theme {
        match self {
            Self::Default => embedded_graphics_terminal::Theme::DEFAULT,
            Self::Sequoia => embedded_graphics_terminal::Theme::SEQUOIA_MOONLIGHT,
            Self::SolarizedDark => embedded_graphics_terminal::Theme::SOLARIZED_DARK,
            Self::SolarizedLight => embedded_graphics_terminal::Theme::SOLARIZED_LIGHT,
            Self::Dracula => embedded_graphics_terminal::Theme::DRACULA,
            Self::GithubDark => embedded_graphics_terminal::Theme::GITHUB_DARK,
            Self::Monokai => embedded_graphics_terminal::Theme::MONOKAI,
            Self::Aura => embedded_graphics_terminal::Theme::AURA,
        }
    }
}

#[derive(Clone)]
pub struct ListItem {
    pub rect: Rectangle,
    pub text: String,
    pub fg_color: Option<UiColor>,
    pub border_color: Option<UiColor>,
}

impl ListItem {
    pub fn new(
        rect: Rectangle,
        text: impl Into<String>,
        border_color: Option<UiColor>,
        fg_color: Option<UiColor>,
    ) -> Self {
        Self {
            rect,
            text: text.into(),
            fg_color,
            border_color,
        }
    }
}

pub fn menu_item_rect(index: usize) -> Option<Rectangle> {
    menu_item_rect_for_count(index, MENU_COLUMNS)
}

fn menu_item_rect_for_count(index: usize, item_count: usize) -> Option<Rectangle> {
    if item_count == 1 {
        let item_top = MENU_START_Y as i32;
        return Some(Rectangle::new(
            Point::new(0, item_top),
            Size::new(DISPLAY_WIDTH as u32, MENU_ITEM_H as u32),
        ));
    }

    let col = index % MENU_COLUMNS;
    let row = index / MENU_COLUMNS;
    let total_gap = MENU_COLUMN_GAP * (MENU_COLUMNS as i32 - 1);
    let item_width = (DISPLAY_WIDTH as i32 - total_gap) / MENU_COLUMNS as i32;
    let item_left = col as i32 * (item_width + MENU_COLUMN_GAP);
    let item_top = MENU_START_Y as i32 + row as i32 * MENU_ITEM_H as i32;
    if item_top + MENU_ITEM_H as i32 > DISPLAY_HEIGHT as i32 - MENU_FOOTER_H {
        return None;
    }

    Some(Rectangle::new(
        Point::new(item_left, item_top),
        Size::new(item_width as u32, MENU_ITEM_H as u32),
    ))
}

fn list_display_text(text: &str, width: u32) -> String {
    const HORIZONTAL_PADDING: u32 = 24;
    let max_width = width.saturating_sub(HORIZONTAL_PADDING);
    let mut used = 0;
    let mut out = String::new();
    for ch in text.chars() {
        let ch_width = if ch.is_ascii() { 8 } else { 16 };
        if used + ch_width > max_width {
            if out.len() < text.len() {
                out.push_str("...");
            }
            return out;
        }
        used += ch_width;
        out.push(ch);
    }
    out
}

pub enum MainMenuSelection {
    Clock,
    Remote,
    Setting,
}

pub enum SettingMenuSelection {
    Ota,
    SyncTime,
    Ble,
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
    terminal: TerminalState,
    jpeg_screen: Option<crate::new_jpg::JpegBufferu16>,
}

const DISPLAY_WIDTH: usize = crate::lcd::LCD_WIDTH as usize;
const DISPLAY_HEIGHT: usize = crate::lcd::LCD_HEIGHT as usize;
const CLOCK_BACKLIGHT_NORMAL: u8 = 30;
const CLOCK_IDLE_OFF_DELAY: std::time::Duration = std::time::Duration::from_secs(30);
static CLOCK_UTC_OFFSET_SECS: AtomicI32 = AtomicI32::new(8 * 60 * 60);

pub fn set_clock_utc_offset_secs(offset_secs: i32) {
    CLOCK_UTC_OFFSET_SECS.store(offset_secs, Ordering::Relaxed);
}

pub async fn clock_screen(
    gui: &mut UI,
    touch: &mut crate::touch::TouchInput,
    boot_button: &mut crate::boot::BootButton,
) -> anyhow::Result<()> {
    set_clock_screen_on(true)?;
    gui.show_clock().await?;
    let mut next_tick = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    let mut idle_off_at = tokio::time::Instant::now() + CLOCK_IDLE_OFF_DELAY;
    let mut screen_on = true;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(next_tick) => {
                next_tick += std::time::Duration::from_secs(1);
                if screen_on {
                    gui.show_clock().await?;
                }
            }
            _ = tokio::time::sleep_until(idle_off_at), if screen_on => {
                log::info!("Clock screen idle timeout, turning screen off");
                set_clock_screen_on(false)?;
                screen_on = false;
            }
            _ = crate::boot::wait_boot_press(boot_button), if screen_on => {
                log::info!("BOOT button pressed from clock screen, turning screen off");
                set_clock_screen_on(false)?;
                screen_on = false;
            }
            gesture = touch.next_gesture() => {
                let Some(gesture) = gesture else {
                    return Err(anyhow::anyhow!("touch event source closed"));
                };
                idle_off_at = tokio::time::Instant::now() + CLOCK_IDLE_OFF_DELAY;
                if !screen_on {
                    log::info!("Touch while clock screen is off, restoring clock");
                    set_clock_screen_on(true)?;
                    screen_on = true;
                    touch.cancel_active_gesture();
                    gui.show_clock().await?;
                    continue;
                }
                if matches!(gesture, crate::touch::TouchGesture::Click { .. }) {
                    return Ok(());
                }
            }
        }
    }
}

fn set_clock_screen_on(on: bool) -> anyhow::Result<()> {
    if on {
        crate::power::hold_light_sleep_lock()?;
        if let Err(e) = crate::audio::init() {
            log::warn!("Failed to reopen audio after clock screen on: {e:?}");
        }
        crate::lcd::set_backlight(CLOCK_BACKLIGHT_NORMAL)?;
    } else {
        crate::lcd::set_backlight(0)?;
        if let Err(e) = crate::audio::close() {
            log::warn!("Failed to close audio before clock light sleep: {e:?}");
        }
        crate::power::release_light_sleep_lock()?;
    }
    Ok(())
}

pub async fn main_menu(
    gui: &mut UI,
    touch: &mut crate::touch::TouchInput,
) -> anyhow::Result<MainMenuSelection> {
    let items = vec![
        ("Remote".to_string(), false),
        ("Setting".to_string(), false),
    ];
    let mut title = main_menu_title();
    let item_rects = gui.display_menu_list(&title, &items).await?;
    log::info!("{title}: waiting for touch selection");

    let mut next_title_refresh = tokio::time::Instant::now() + MENU_TITLE_REFRESH_DELAY;
    let index = loop {
        tokio::select! {
            _ = tokio::time::sleep_until(next_title_refresh) => {
                next_title_refresh = tokio::time::Instant::now() + MENU_TITLE_REFRESH_DELAY;
                let next_title = main_menu_title();
                if next_title != title {
                    gui.refresh_list_title(&next_title).await?;
                    title = next_title;
                }
            }
            gesture = touch.next_gesture() => {
                let Some(gesture) = gesture else {
                    return Err(anyhow::anyhow!("touch event source closed"));
                };
                if let crate::touch::TouchGesture::Click { start, end } = gesture {
                    let press_index = list_touch_index(start, &item_rects);
                    let release_index = list_touch_index(end, &item_rects);
                    if press_index.is_some() && press_index == release_index {
                        let index = press_index.unwrap();
                        log::info!("{title}: selected item {index}");
                        break index;
                    }
                    log::info!(
                        "{title}: ignored touch, press={:?} release={:?}",
                        press_index,
                        release_index
                    );
                } else if matches!(
                    gesture,
                    crate::touch::TouchGesture::Swipe {
                        direction: crate::touch::SwipeDirection::Right,
                        ..
                    }
                ) {
                    log::info!("{title}: right swipe detected, returning to clock");
                    return Ok(MainMenuSelection::Clock);
                }
            }
        }
    };
    Ok(match index {
        0 => MainMenuSelection::Remote,
        1 => MainMenuSelection::Setting,
        _ => unreachable!(),
    })
}

fn main_menu_title() -> String {
    match crate::power::battery_percent() {
        Some(percent) => format!("Main: Battery {percent}%"),
        None => "Main: Battery --".to_string(),
    }
}

fn list_title_color(title: &str) -> ColorFormat {
    let Some(rest) = title.split("Battery ").nth(1) else {
        return TEXT_LIGHT;
    };
    let Some(percent_text) = rest.split('%').next() else {
        return TEXT_LIGHT;
    };
    let Ok(percent) = percent_text.parse::<u8>() else {
        return TEXT_LIGHT;
    };

    match percent {
        0..=20 => ColorFormat::CSS_RED,
        21..=60 => ColorFormat::CSS_DARK_ORANGE,
        _ => ColorFormat::CSS_DARK_GREEN,
    }
}

pub async fn setting_menu(
    gui: &mut UI,
    touch: &mut crate::touch::TouchInput,
) -> anyhow::Result<SettingMenuSelection> {
    let items = vec![
        ("OTA Update".to_string(), false),
        ("Sync Time".to_string(), false),
        ("Enable BLE".to_string(), false),
        ("Back".to_string(), false),
    ];
    let index = select_menu_item(gui, touch, "Setting", &items).await?;
    Ok(match index {
        0 => SettingMenuSelection::Ota,
        1 => SettingMenuSelection::SyncTime,
        2 => SettingMenuSelection::Ble,
        3 => SettingMenuSelection::Back,
        _ => unreachable!(),
    })
}

pub async fn select_menu_item(
    gui: &mut UI,
    touch: &mut crate::touch::TouchInput,
    title: &str,
    items: &[(String, bool)],
) -> anyhow::Result<usize> {
    let item_rects = gui.display_menu_list(title, items).await?;
    log::info!("{title}: waiting for touch selection");

    loop {
        match touch.next_gesture().await {
            Some(crate::touch::TouchGesture::Click { start, end }) => {
                let press_index = list_touch_index(start, &item_rects);
                let release_index = list_touch_index(end, &item_rects);
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
            }
            Some(_) => {}
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
                    .stroke_color(ColorFormat::CSS_STEEL_BLUE)
                    .stroke_width(1)
                    .fill_color(ColorFormat::CSS_STEEL_BLUE)
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
            terminal: TerminalState::new(),
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

fn current_clock_parts() -> (i64, u64, u64, u64, u64) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0) as i64;
    let offset = CLOCK_UTC_OFFSET_SECS.load(Ordering::Relaxed) as i64;
    let local_secs = secs + offset;
    let days = local_secs.div_euclid(24 * 60 * 60);
    let day_secs = local_secs.rem_euclid(24 * 60 * 60) as u64;
    let (year, month, day) = civil_from_days(days);
    (year, month, day, day_secs / 3600, day_secs / 60 % 60)
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i64, u64, u64) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month as u64, day as u64)
}

impl UI {
    pub async fn show_clock(&mut self) -> anyhow::Result<()> {
        let (year, month, day, hours, minutes) = current_clock_parts();
        self.display.clear(ColorFormat::CSS_BLACK)?;

        let time_color = ColorFormat::CSS_DARK_GREEN;
        let date_style =
            shifted_text_style(u8g2_fonts::fonts::u8g2_font_logisoso24_tr, time_color, 0);
        Text::with_alignment(
            &format!("{year:04}-{month:02}-{day:02}"),
            Point::new(DISPLAY_WIDTH as i32 / 2, DISPLAY_HEIGHT as i32 / 4),
            date_style,
            Alignment::Center,
        )
        .draw(self.display.as_mut())?;

        let style = shifted_text_style(u8g2_fonts::fonts::u8g2_font_logisoso78_tn, time_color, 0);
        let baseline_y = (DISPLAY_HEIGHT as i32 / 2) + 38;
        Text::with_alignment(
            &format!("{hours:02}"),
            Point::new(DISPLAY_WIDTH as i32 / 2 - 72, baseline_y),
            style.clone(),
            Alignment::Center,
        )
        .draw(self.display.as_mut())?;
        Text::with_alignment(
            &format!("{minutes:02}"),
            Point::new(DISPLAY_WIDTH as i32 / 2 + 72, baseline_y),
            style,
            Alignment::Center,
        )
        .draw(self.display.as_mut())?;

        let colon_x = DISPLAY_WIDTH as i32 / 2 - 5;
        for y in [baseline_y - 52, baseline_y - 18] {
            Rectangle::new(Point::new(colon_x, y), Size::new(10, 10))
                .into_styled(PrimitiveStyleBuilder::new().fill_color(time_color).build())
                .draw(self.display.as_mut())?;
        }

        self.flush_terminal_full().await?;
        Ok(())
    }

    pub fn set_terminal_theme(&mut self, index: usize) -> &'static str {
        let theme = TerminalTheme::from_index(index);
        TERMINAL_THEME_INDEX.store(index % TerminalTheme::ALL.len(), Ordering::Relaxed);
        self.terminal.set_theme();
        theme.label()
    }

    pub async fn show_status(
        &mut self,
        state: impl Into<String>,
        text: impl Into<String>,
    ) -> anyhow::Result<()> {
        self.state = state.into();
        self.text = text.into();
        self.display_flush().await
    }

    /// ASR text editor, adapted from vibekeys_firmware's black TUI-style editor.
    pub async fn show_asr_editor(&mut self, text: &str, hint: &str) -> anyhow::Result<()> {
        let display = self.display.as_mut();
        display.clear(ColorFormat::CSS_BLACK)?;
        let record_color = match hint {
            "Connecting..." => ColorFormat::CSS_YELLOW,
            "Listening..." => ColorFormat::CSS_GREEN,
            _ => ColorFormat::CSS_WHEAT,
        };

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
                    .stroke_color(record_color)
                    .stroke_width(3)
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
                    .stroke_width(3)
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
            shifted_text_style(u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312, TEXT_LIGHT, 3),
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
                    .stroke_color(record_color)
                    .stroke_width(3)
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
                record_color,
                3,
            ),
            hint_style,
        )
        .draw(display)?;

        let e = crate::lcd::async_flush_display(
            self.display.data(),
            0,
            0,
            DISPLAY_WIDTH as i32,
            DISPLAY_HEIGHT as i32,
        )
        .await;
        if e == 0 {
            Ok(())
        } else {
            Err(anyhow::anyhow!("flush asr editor failed: {e}"))
        }
    }

    async fn flush_terminal_dirty(&self, rect: Rectangle) -> anyhow::Result<Option<i64>> {
        let Some((data, rect)) = self.display.rect_data(rect) else {
            return Ok(None);
        };

        let flush_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        let e = crate::lcd::async_flush_display(
            &data,
            rect.top_left.x,
            rect.top_left.y,
            rect.top_left.x + rect.size.width as i32,
            rect.top_left.y + rect.size.height as i32,
        )
        .await;
        if e == 0 {
            let flush_elapsed_us =
                unsafe { esp_idf_svc::sys::esp_timer_get_time() } - flush_start_us;
            Ok(Some(flush_elapsed_us))
        } else {
            Err(anyhow::anyhow!("flush terminal dirty rect failed: {e}"))
        }
    }

    async fn flush_terminal_full(&self) -> anyhow::Result<i64> {
        let flush_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        let e = crate::lcd::async_flush_display(
            self.display.data(),
            0,
            0,
            DISPLAY_WIDTH as i32,
            DISPLAY_HEIGHT as i32,
        )
        .await;
        if e == 0 {
            Ok(unsafe { esp_idf_svc::sys::esp_timer_get_time() } - flush_start_us)
        } else {
            Err(anyhow::anyhow!("flush terminal full frame failed: {e}"))
        }
    }

    pub async fn show_terminal_text_frame(&mut self, payload: &[u8]) -> anyhow::Result<()> {
        let Some((&tag, bytes)) = payload.split_first() else {
            log::warn!("empty screen_text frame");
            return Ok(());
        };
        let full_frame = tag == 0x00;
        match tag {
            0x00 => {
                log::info!("screen_text full frame: {}B", bytes.len());
                self.terminal.reset_session();
                self.terminal.append_render_deadline = None;
            }
            0x01 => {
                log::debug!("screen_text delta frame: {}B", bytes.len());
                if self.terminal.session.is_none() {
                    log::warn!("screen_text delta before full frame; creating blank terminal");
                    self.terminal.reset_session();
                    self.terminal.ensure_session().renderer.invalidate();
                }
            }
            other => {
                log::warn!("unknown screen_text tag: {other}");
                return Ok(());
            }
        }

        let parse_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        self.terminal.ensure_session().parser.process(bytes);
        let parse_elapsed_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() } - parse_start_us;
        let now_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };

        let since_last_render = if self.terminal.last_render_us > 0 {
            Some(std::time::Duration::from_micros(
                (now_us - self.terminal.last_render_us) as u64,
            ))
        } else {
            None
        };
        if !full_frame
            && since_last_render.is_some_and(|elapsed| elapsed < TERMINAL_APPEND_RENDER_TIMEOUT)
        {
            let elapsed = since_last_render.unwrap();
            log::debug!(
                "screen_text append delayed render: bytes={} parse={:.2}ms since_render={:.2}ms",
                bytes.len(),
                parse_elapsed_us as f32 / 1000.0,
                elapsed.as_micros() as f32 / 1000.0
            );
            tokio::time::sleep(TERMINAL_APPEND_RENDER_TIMEOUT - elapsed).await;
        }

        self.render_terminal_frame(tag, bytes.len(), parse_elapsed_us, full_frame)
            .await
    }

    pub async fn buffer_terminal_text_frame(&mut self, payload: &[u8]) -> anyhow::Result<()> {
        let Some((&tag, bytes)) = payload.split_first() else {
            log::warn!("empty screen_text frame");
            return Ok(());
        };
        if tag != 0x01 {
            return self.show_terminal_text_frame(payload).await;
        }

        log::debug!("screen_text delta frame: {}B", bytes.len());
        if self.terminal.session.is_none() {
            log::warn!("screen_text delta before full frame; creating blank terminal");
            self.terminal.reset_session();
            self.terminal.ensure_session().renderer.invalidate();
        }

        let parse_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        self.terminal.ensure_session().parser.process(bytes);
        let parse_elapsed_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() } - parse_start_us;

        if self.terminal.append_render_deadline.is_none() {
            self.terminal.append_render_deadline =
                Some(tokio::time::Instant::now() + TERMINAL_APPEND_RENDER_TIMEOUT);
        }
        log::debug!(
            "screen_text append buffered: bytes={} parse={:.2}ms deadline_set={}",
            bytes.len(),
            parse_elapsed_us as f32 / 1000.0,
            self.terminal.append_render_deadline.is_some()
        );
        Ok(())
    }

    async fn render_terminal_frame(
        &mut self,
        tag: u8,
        byte_len: usize,
        parse_elapsed_us: i64,
        full_frame: bool,
    ) -> anyhow::Result<()> {
        let render_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        let dirty = {
            let session = self.terminal.ensure_session();
            if full_frame {
                self.display.clear(ColorFormat::CSS_BLACK)?;
                session
                    .renderer
                    .render(session.parser.screen(), self.display.as_mut())?;
                session.renderer.invalidate();
                Some(self.display.bounding_box())
            } else {
                session
                    .renderer
                    .render_diff(session.parser.screen(), self.display.as_mut())?
            }
        };
        let render_elapsed_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() } - render_start_us;
        let cache_len = self.terminal.ensure_session().renderer.cache_len();

        let flush_elapsed_us = match (full_frame, dirty) {
            (true, Some(_)) => self.flush_terminal_full().await?,
            (false, Some(rect)) => self.flush_terminal_dirty(rect).await?.unwrap_or(0),
            (_, None) => 0,
        };
        self.terminal.last_render_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        log::info!(
            "screen_text frame tag=0x{tag:02x} bytes={} parse={:.2}ms render={:.2}ms flush={:.2}ms cache_len={} dirty={:?}",
            byte_len,
            parse_elapsed_us as f32 / 1000.0,
            render_elapsed_us as f32 / 1000.0,
            flush_elapsed_us as f32 / 1000.0,
            cache_len,
            dirty
        );
        Ok(())
    }

    pub fn terminal_append_render_deadline(&self) -> Option<tokio::time::Instant> {
        self.terminal.append_render_deadline
    }

    pub fn cancel_pending_terminal_append(&mut self) {
        self.terminal.append_render_deadline = None;
    }

    pub async fn render_pending_terminal_append(&mut self) -> anyhow::Result<bool> {
        if self.terminal.append_render_deadline.is_none() {
            return Ok(false);
        }
        self.terminal.append_render_deadline = None;

        let Some(session) = self.terminal.session.as_mut() else {
            return Ok(false);
        };
        let render_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        let dirty = session
            .renderer
            .render_diff(session.parser.screen(), self.display.as_mut())?;
        let render_elapsed_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() } - render_start_us;
        let cache_len = session.renderer.cache_len();

        let flush_elapsed_us = match dirty {
            Some(rect) => self.flush_terminal_dirty(rect).await?.unwrap_or(0),
            None => 0,
        };
        self.terminal.last_render_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        log::info!(
            "screen_text append render: render={:.2}ms flush={:.2}ms cache_len={} dirty={:?}",
            render_elapsed_us as f32 / 1000.0,
            flush_elapsed_us as f32 / 1000.0,
            cache_len,
            dirty
        );
        Ok(dirty.is_some())
    }

    pub async fn redraw_cached_terminal_text(&mut self) -> anyhow::Result<bool> {
        let Some(session) = self.terminal.session.as_mut() else {
            return Ok(false);
        };
        self.terminal.append_render_deadline = None;

        let render_start_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() };
        self.display.clear(ColorFormat::CSS_BLACK)?;
        session
            .renderer
            .render(session.parser.screen(), self.display.as_mut())?;
        session.renderer.invalidate();
        let render_elapsed_us = unsafe { esp_idf_svc::sys::esp_timer_get_time() } - render_start_us;
        let cache_len = session.renderer.cache_len();

        let flush_elapsed_us = self.flush_terminal_full().await?;
        log::info!(
            "redraw cached terminal text render={:.2}ms flush={:.2}ms cache_len={}",
            render_elapsed_us as f32 / 1000.0,
            flush_elapsed_us as f32 / 1000.0,
            cache_len
        );
        Ok(true)
    }

    pub async fn show_jpeg_screen(
        &mut self,
        screen: crate::new_jpg::JpegBufferu16,
    ) -> anyhow::Result<()> {
        screen.flush_to_lcd_async().await?;
        self.jpeg_screen = Some(screen);
        Ok(())
    }

    pub async fn redraw_cached_jpeg_screen(&self) -> anyhow::Result<bool> {
        let Some(screen) = self.jpeg_screen.as_ref() else {
            return Ok(false);
        };
        screen.flush_to_lcd_async().await?;
        Ok(true)
    }

    pub async fn show_loading_modal(&mut self) -> anyhow::Result<()> {
        let modal_w = (DISPLAY_WIDTH as u32).saturating_sub(80).min(240);
        let modal_h = 88u32;
        let modal_rect = Rectangle::new(
            Point::new(
                ((DISPLAY_WIDTH as u32).saturating_sub(modal_w) / 2) as i32,
                ((DISPLAY_HEIGHT as u32).saturating_sub(modal_h) / 2) as i32,
            ),
            Size::new(modal_w, modal_h),
        );
        let display = self.display.as_mut();
        modal_rect
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .stroke_color(ColorFormat::CSS_WHEAT)
                    .stroke_width(4)
                    .fill_color(ColorFormat::CSS_BLACK)
                    .build(),
            )
            .draw(display)?;
        Text::with_alignment(
            "Loading...",
            modal_rect.center() + Point::new(0, 6),
            shifted_text_style(
                u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312,
                ColorFormat::CSS_LIGHT_CYAN,
                3,
            ),
            Alignment::Center,
        )
        .draw(display)?;

        let _ = self.flush_terminal_dirty(modal_rect).await?;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Ok(())
    }

    pub async fn show_session_backspace_overlay(&mut self) -> anyhow::Result<()> {
        let (rect, box_rect) = self.draw_session_top_overlay_box(0)?;
        let icon_style = PrimitiveStyleBuilder::new()
            .stroke_color(ColorFormat::CSS_WHEAT)
            .stroke_width(4)
            .build();
        let x0 = box_rect.top_left.x + 18;
        let x1 = box_rect.top_left.x + 42;
        let x2 = box_rect.top_left.x + box_rect.size.width as i32 - 18;
        let y0 = box_rect.top_left.y + 16;
        let y1 = box_rect.top_left.y + box_rect.size.height as i32 / 2;
        let y2 = box_rect.top_left.y + box_rect.size.height as i32 - 16;
        for line in [
            Line::new(Point::new(x0, y1), Point::new(x1, y0)),
            Line::new(Point::new(x1, y0), Point::new(x2, y0)),
            Line::new(Point::new(x2, y0), Point::new(x2, y2)),
            Line::new(Point::new(x2, y2), Point::new(x1, y2)),
            Line::new(Point::new(x1, y2), Point::new(x0, y1)),
            Line::new(Point::new(x1 + 24, y0 + 14), Point::new(x2 - 20, y2 - 14)),
            Line::new(Point::new(x2 - 20, y0 + 14), Point::new(x1 + 24, y2 - 14)),
        ] {
            line.into_styled(icon_style).draw(self.display.as_mut())?;
        }

        let _ = self.flush_terminal_dirty(rect).await?;
        Ok(())
    }

    pub async fn show_session_menu_overlay(&mut self) -> anyhow::Result<()> {
        let (rect, box_rect) = self.draw_session_top_overlay_box(2)?;
        let icon_style = PrimitiveStyleBuilder::new()
            .stroke_color(ColorFormat::CSS_WHEAT)
            .stroke_width(4)
            .build();
        let x0 = box_rect.top_left.x + 28;
        let x1 = box_rect.top_left.x + box_rect.size.width as i32 - 28;
        let center_y = box_rect.top_left.y + box_rect.size.height as i32 / 2;
        for y in [center_y - 14, center_y, center_y + 14] {
            Line::new(Point::new(x0, y), Point::new(x1, y))
                .into_styled(icon_style)
                .draw(self.display.as_mut())?;
        }

        let _ = self.flush_terminal_dirty(rect).await?;
        Ok(())
    }

    fn draw_session_top_overlay_box(
        &mut self,
        third_index: usize,
    ) -> anyhow::Result<(Rectangle, Rectangle)> {
        let third_w = DISPLAY_WIDTH / 3;
        let x = (third_w * third_index) as i32;
        let w = if third_index == 2 {
            DISPLAY_WIDTH - third_w * 2
        } else {
            third_w
        };
        let rect = Rectangle::new(Point::new(x, 0), Size::new(w as u32, 80));
        let display = self.display.as_mut();
        rect.into_styled(
            PrimitiveStyleBuilder::new()
                .fill_color(ColorFormat::CSS_BLACK)
                .build(),
        )
        .draw(display)?;

        let box_rect = Rectangle::new(
            Point::new(x + 8, 8),
            Size::new(w.saturating_sub(16) as u32, 64),
        );
        box_rect
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .stroke_color(ColorFormat::CSS_WHEAT)
                    .stroke_width(3)
                    .fill_color(ColorFormat::CSS_BLACK)
                    .build(),
            )
            .draw(display)?;
        Ok((rect, box_rect))
    }

    // 横向42个字符
    async fn display_flush(&mut self) -> anyhow::Result<()> {
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
            let e = crate::lcd::async_flush_display(
                self.display.data(),
                0,
                0,
                DISPLAY_WIDTH as _,
                DISPLAY_HEIGHT as _,
            )
            .await;
            if e == 0 {
                break;
            }
            log::warn!("flush_display error: {} retry {i}", e);
        }
        Ok(())
    }

    /// Render generic list items. Each item owns its geometry and optional colors.
    pub async fn display_list(
        &mut self,
        title: &str,
        items: &[ListItem],
    ) -> anyhow::Result<Vec<Rectangle>> {
        let display = self.display.as_mut();
        display.clear(ColorFormat::CSS_BLACK)?;

        Text::with_alignment(
            title,
            Point::new((DISPLAY_WIDTH / 2) as i32, 18),
            U8g2TextStyle::new(
                u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312,
                list_title_color(title),
            ),
            Alignment::Center,
        )
        .draw(display)?;

        Text::with_alignment(
            build_version_label(),
            Point::new((DISPLAY_WIDTH / 2) as i32, DISPLAY_HEIGHT as i32 - 6),
            U8g2TextStyle::new(
                u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312,
                ColorFormat::CSS_GRAY,
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
            let style = PrimitiveStyleBuilder::new()
                .stroke_color(item.border_color.unwrap_or(ColorFormat::CSS_BLACK))
                .stroke_width(8)
                .fill_color(ColorFormat::CSS_BLACK);
            draw_rect.into_styled(style.build()).draw(display)?;
            if let Some(fg_color) = item.fg_color {
                let text_y =
                    draw_rect.top_left.y + (draw_rect.size.height as i32 + MENU_FONT_H as i32) / 2;
                let text = list_display_text(&item.text, draw_rect.size.width);
                Text::with_alignment(
                    &text,
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
        let e = crate::lcd::async_flush_display(
            self.display.data(),
            0,
            0,
            DISPLAY_WIDTH as i32,
            DISPLAY_HEIGHT as i32,
        )
        .await;
        if e == 0 {
            log::info!("display_list flush ok");
            Ok(item_rects)
        } else {
            Err(anyhow::anyhow!("flush list failed: {e}"))
        }
    }

    pub async fn refresh_list_title(&mut self, title: &str) -> anyhow::Result<()> {
        let title_rect = Rectangle::new(
            Point::zero(),
            Size::new(DISPLAY_WIDTH as u32, MENU_START_Y as u32),
        );
        title_rect
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .fill_color(ColorFormat::CSS_BLACK)
                    .build(),
            )
            .draw(self.display.as_mut())?;

        Text::with_alignment(
            title,
            Point::new((DISPLAY_WIDTH / 2) as i32, 18),
            U8g2TextStyle::new(
                u8g2_fonts::fonts::u8g2_font_wqy16_t_gb2312,
                list_title_color(title),
            ),
            Alignment::Center,
        )
        .draw(self.display.as_mut())?;

        let Some((data, rect)) = self.display.rect_data(title_rect) else {
            return Ok(());
        };
        let e = crate::lcd::async_flush_display(
            &data,
            rect.top_left.x,
            rect.top_left.y,
            rect.top_left.x + rect.size.width as i32,
            rect.top_left.y + rect.size.height as i32,
        )
        .await;
        if e == 0 {
            Ok(())
        } else {
            Err(anyhow::anyhow!("flush list title failed: {e}"))
        }
    }

    /// Compatibility helper for the existing menu/session list layout.
    pub async fn display_menu_list(
        &mut self,
        title: &str,
        items: &[(String, bool)],
    ) -> anyhow::Result<Vec<Rectangle>> {
        let list_items: Vec<ListItem> = items
            .iter()
            .enumerate()
            .filter_map(|(i, (label, is_working))| {
                let rect = menu_item_rect_for_count(i, items.len())?;
                let border_color = if *is_working {
                    ColorFormat::CSS_STEEL_BLUE
                } else {
                    ColorFormat::CSS_DARK_ORANGE
                };
                Some(ListItem::new(
                    rect,
                    label.clone(),
                    Some(border_color),
                    Some(TEXT_LIGHT),
                ))
            })
            .collect();

        self.display_list(title, &list_items).await
    }
}
