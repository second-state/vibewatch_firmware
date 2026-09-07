use embedded_graphics::{
    draw_target::DrawTarget,
    geometry::{Point, Size},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{
        Circle, Line, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle, Triangle,
    },
    text::{Alignment, Text},
};
use embedded_text::TextBox;
use u8g2_fonts::U8g2TextStyle;

pub type Color = Rgb565;

pub struct Palette;

impl Palette {
    pub const BG: Color = rgb(0x06, 0x07, 0x0a);
    pub const PANEL: Color = rgb(0x0b, 0x0d, 0x10);
    pub const PANEL_2: Color = rgb(0x10, 0x12, 0x15);
    pub const SURFACE: Color = rgb(0x16, 0x18, 0x1c);
    pub const SURFACE_2: Color = rgb(0x1c, 0x1f, 0x24);
    pub const TEXT: Color = rgb(0xf5, 0xf5, 0xf0);
    pub const MUTED: Color = rgb(0x8a, 0x8f, 0x98);
    pub const DIM: Color = rgb(0x5b, 0x60, 0x68);
    pub const FAINT: Color = rgb(0x4b, 0x4f, 0x56);
    pub const AMBER: Color = rgb(0xff, 0xb1, 0x3d);
    pub const AMBER_DIM: Color = rgb(0x54, 0x3c, 0x19);
    pub const AMBER_SURFACE: Color = rgb(0x2b, 0x21, 0x13);
    pub const PURPLE: Color = rgb(0x9b, 0x8c, 0xff);
    pub const GREEN: Color = rgb(0xb9, 0xf0, 0x4a);
    pub const RED: Color = rgb(0xff, 0x6b, 0x5e);
}

const DESIGN_WIDTH: u32 = 264;
const DESIGN_HEIGHT: u32 = 328;

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::new(r >> 3, g >> 2, b >> 3)
}

#[derive(Clone, Copy)]
struct Scale {
    origin: Point,
    size: Size,
}

impl Scale {
    const fn new(frame: Rectangle) -> Self {
        Self {
            origin: frame.top_left,
            size: frame.size,
        }
    }

    fn sx(&self, v: i32) -> i32 {
        self.origin.x + (v * self.size.width as i32) / DESIGN_WIDTH as i32
    }

    fn sy(&self, v: i32) -> i32 {
        self.origin.y + (v * self.size.height as i32) / DESIGN_HEIGHT as i32
    }

    fn sw(&self, v: u32) -> u32 {
        (v * self.size.width) / DESIGN_WIDTH
    }

    fn sh(&self, v: u32) -> u32 {
        (v * self.size.height) / DESIGN_HEIGHT
    }

    fn sr(&self, v: u32) -> u32 {
        ((v * self.size.width) / DESIGN_WIDTH).max(1)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Back,
    Submit,
    Left,
    Right,
    Agent,
    Settings,
}

pub struct ClockData<'a> {
    pub year: u64,
    pub month: u64,
    pub day: u64,
    pub hours: u64,
    pub minutes: u64,
    pub battery: Option<u8>,
    pub status: &'a str,
}

/// Read-only notice screen used for connection, sync, OTA, and error messages.
pub struct NoticeData<'a> {
    pub title: &'a str,
    pub text: &'a str,
}

pub struct MenuTile<'a> {
    pub label: &'a str,
    pub icon: Icon,
    pub accent: Color,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainMenuHit {
    Back,
    Top,
    Bottom,
}

#[derive(Debug, Clone)]
pub struct MainMenuHitRegions {
    back: Rectangle,
    top: Rectangle,
    bottom: Rectangle,
}

impl MainMenuHitRegions {
    pub fn hit(&self, touch: crate::lcd::TouchPoint) -> Option<MainMenuHit> {
        if contains_touch(self.back, touch) {
            Some(MainMenuHit::Back)
        } else if contains_touch(self.top, touch) {
            Some(MainMenuHit::Top)
        } else if contains_touch(self.bottom, touch) {
            Some(MainMenuHit::Bottom)
        } else {
            None
        }
    }

    pub fn hit_pair(
        &self,
        start: crate::lcd::TouchPoint,
        end: crate::lcd::TouchPoint,
    ) -> Option<MainMenuHit> {
        let start_hit = self.hit(start)?;
        (Some(start_hit) == self.hit(end)).then_some(start_hit)
    }
}

impl Default for MainMenuHitRegions {
    fn default() -> Self {
        Self {
            back: Rectangle::zero(),
            top: Rectangle::zero(),
            bottom: Rectangle::zero(),
        }
    }
}

pub struct SessionRow<'a> {
    pub label: &'a str,
    pub active: bool,
    pub working: bool,
}

pub struct SessionListData<'a> {
    pub title: &'a str,
    pub battery: Option<u8>,
    pub rows: &'a [SessionRow<'a>],
    pub footer: &'a str,
}

#[derive(Debug, Clone)]
pub struct SessionListHitRegions {
    back: Rectangle,
    rows: Vec<Rectangle>,
}

impl Default for SessionListHitRegions {
    fn default() -> Self {
        Self {
            back: top_left_scaled_hit_rect(64),
            rows: Vec::new(),
        }
    }
}

impl SessionListHitRegions {
    pub fn clear(&mut self) {
        self.rows.clear();
    }

    pub fn back_hit(&self, touch: crate::lcd::TouchPoint) -> bool {
        contains_touch(self.back, touch)
    }

    pub fn back_hit_pair(
        &self,
        start: crate::lcd::TouchPoint,
        end: crate::lcd::TouchPoint,
    ) -> bool {
        self.back_hit(start) && self.back_hit(end)
    }

    pub fn visible_count(&self) -> usize {
        self.rows.len()
    }

    pub fn hit_index(&self, touch: crate::lcd::TouchPoint) -> Option<usize> {
        self.rows
            .iter()
            .position(|rect| contains_touch(*rect, touch))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveSessionHit {
    Back,
    PrevAction,
    NextAction,
    RunAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveSessionSwipe {
    Back,
    ScrollUp,
    ScrollDown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentTuiAction {
    #[default]
    Speak,
    Accept,
    Next,
    Yolo,
    Del,
    Esc,
}

impl AgentTuiAction {
    pub const ALL: [Self; 6] = [
        Self::Speak,
        Self::Accept,
        Self::Next,
        Self::Yolo,
        Self::Del,
        Self::Esc,
    ];

    pub fn from_index(index: usize) -> Self {
        Self::ALL[index % Self::ALL.len()]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Speak => "* Speak",
            Self::Accept => "+ Accept",
            Self::Next => "> Next",
            Self::Yolo => "* Yolo",
            Self::Del => "< Del",
            Self::Esc => "x Esc",
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Speak | Self::Yolo => Palette::AMBER,
            Self::Accept => Palette::GREEN,
            Self::Next => Palette::PURPLE,
            Self::Del => Palette::RED,
            Self::Esc => Palette::RED,
        }
    }

    fn fill(self) -> Color {
        match self {
            Self::Speak | Self::Yolo => Palette::AMBER_SURFACE,
            Self::Accept => rgb(0x1f, 0x2a, 0x15),
            Self::Next => rgb(0x22, 0x1f, 0x34),
            Self::Del => rgb(0x2b, 0x15, 0x17),
            Self::Esc => rgb(0x2b, 0x15, 0x17),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActiveSessionHitRegions {
    back: Rectangle,
    prev_action: Rectangle,
    next_action: Rectangle,
    run_action: Rectangle,
}

impl Default for ActiveSessionHitRegions {
    fn default() -> Self {
        Self {
            back: top_left_scaled_hit_rect(72),
            prev_action: Rectangle::zero(),
            next_action: Rectangle::zero(),
            run_action: Rectangle::zero(),
        }
    }
}

impl ActiveSessionHitRegions {
    pub fn hit(&self, touch: crate::lcd::TouchPoint) -> Option<ActiveSessionHit> {
        if contains_touch(self.back, touch) {
            Some(ActiveSessionHit::Back)
        } else if contains_touch(self.prev_action, touch) {
            Some(ActiveSessionHit::PrevAction)
        } else if contains_touch(self.next_action, touch) {
            Some(ActiveSessionHit::NextAction)
        } else if contains_touch(self.run_action, touch) {
            Some(ActiveSessionHit::RunAction)
        } else {
            None
        }
    }

    pub fn hit_pair(
        &self,
        start: crate::lcd::TouchPoint,
        end: crate::lcd::TouchPoint,
    ) -> Option<ActiveSessionHit> {
        let start_hit = self.hit(start)?;
        (Some(start_hit) == self.hit(end)).then_some(start_hit)
    }

    pub fn swipe(
        &self,
        start: crate::lcd::TouchPoint,
        end: crate::lcd::TouchPoint,
    ) -> Option<ActiveSessionSwipe> {
        let dx = end.x as i32 - start.x as i32;
        let dy = end.y as i32 - start.y as i32;
        if dx >= crate::touch::DEFAULT_SWIPE_THRESHOLD_PX
            && dy.abs() <= crate::touch::DEFAULT_SWIPE_THRESHOLD_PX
        {
            return Some(ActiveSessionSwipe::Back);
        }
        if dx.abs() > crate::touch::DEFAULT_SWIPE_THRESHOLD_PX
            || dy.abs() < crate::touch::DEFAULT_SWIPE_THRESHOLD_PX
        {
            return None;
        }
        if dy < 0 {
            Some(ActiveSessionSwipe::ScrollDown)
        } else {
            Some(ActiveSessionSwipe::ScrollUp)
        }
    }

    pub fn swipe_preview_offset(
        &self,
        start: crate::lcd::TouchPoint,
        current: crate::lcd::TouchPoint,
        direction: Option<crate::touch::SwipeDirection>,
        dy: i32,
    ) -> i32 {
        if matches!(
            direction,
            Some(crate::touch::SwipeDirection::Up | crate::touch::SwipeDirection::Down)
        ) && matches!(
            self.swipe(start, current),
            Some(ActiveSessionSwipe::ScrollUp | ActiveSessionSwipe::ScrollDown)
        ) {
            dy
        } else {
            0
        }
    }
}

pub fn active_session_controls_rect(frame: Rectangle) -> Rectangle {
    let s = Scale::new(frame);
    Rectangle::new(
        Point::new(frame.top_left.x, s.sy(278)),
        Size::new(
            frame.size.width,
            frame.size.height.saturating_sub(s.sh(278)),
        ),
    )
}

pub fn active_session_controls_trigger_hit_pair(
    start: crate::lcd::TouchPoint,
    end: crate::lcd::TouchPoint,
) -> bool {
    let frame = Rectangle::new(
        Point::zero(),
        Size::new(crate::lcd::LCD_WIDTH as u32, crate::lcd::LCD_HEIGHT as u32),
    );
    let rect = active_session_controls_rect(frame);
    contains_touch(rect, start) && contains_touch(rect, end)
}

pub fn render_active_session_controls<D>(
    target: &mut D,
    action: AgentTuiAction,
) -> Result<ActiveSessionHitRegions, D::Error>
where
    D: DrawTarget<Color = Color>,
{
    let frame = target.bounding_box();
    let s = Scale::new(frame);
    let controls = active_session_controls_rect(frame);
    controls
        .into_styled(PrimitiveStyle::with_fill(Palette::BG))
        .draw(target)?;

    let prev_action = Rectangle::new(
        Point::new(s.sx(20), s.sy(286)),
        Size::new(s.sw(36), s.sh(36)),
    );
    let next_action = Rectangle::new(
        Point::new(s.sx(210), s.sy(286)),
        Size::new(s.sw(36), s.sh(36)),
    );
    let run_action = Rectangle::new(
        Point::new(s.sx(67), s.sy(290)),
        Size::new(s.sw(130), s.sh(28)),
    );

    for (center, icon) in [
        (Point::new(s.sx(36), s.sy(304)), Icon::Left),
        (Point::new(s.sx(228), s.sy(304)), Icon::Right),
    ] {
        Circle::with_center(center, s.sr(24))
            .into_styled(PrimitiveStyle::with_fill(Palette::SURFACE_2))
            .draw(target)?;
        draw_icon(target, s, center, icon, Palette::MUTED)?;
    }
    round_rect(target, run_action, s.sr(14), action.fill(), None)?;
    draw_label(
        target,
        action.label(),
        run_action.center() + Point::new(0, 5),
        action.color(),
        Alignment::Center,
    )?;

    Ok(ActiveSessionHitRegions {
        back: Rectangle::zero(),
        prev_action,
        next_action,
        run_action,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VoiceState {
    Idle,
    Connecting,
    Listening,
    Error,
}

pub struct VoiceInputData<'a> {
    pub text: &'a str,
    pub state: VoiceState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceInputHit {
    Back,
    Submit,
    Delete,
    Left,
    Right,
    Record,
}

#[derive(Debug, Clone)]
pub struct VoiceInputHitRegions {
    back: Rectangle,
    submit: Rectangle,
    left: Rectangle,
    right: Rectangle,
    delete: Rectangle,
    record: Rectangle,
}

impl Default for VoiceInputHitRegions {
    fn default() -> Self {
        Self::new(Rectangle::new(
            Point::zero(),
            Size::new(crate::lcd::LCD_WIDTH as u32, crate::lcd::LCD_HEIGHT as u32),
        ))
    }
}

impl VoiceInputHitRegions {
    pub fn new(frame: Rectangle) -> Self {
        let s = Scale::new(frame);
        let button_y = s.sy(270);
        let button_w = s.sw(50);
        let button_h = s.sh(40);
        let gap = s.sw(8) as i32;
        let bottom_button = |index: i32| {
            Rectangle::new(
                Point::new(s.sx(20) + index * (button_w as i32 + gap), button_y),
                Size::new(button_w, button_h),
            )
        };
        Self {
            back: top_left_scaled_hit_rect(72),
            submit: Rectangle::new(
                Point::new(s.sx(211), s.sy(13)),
                Size::new(s.sw(40), s.sh(40)),
            ),
            left: bottom_button(0),
            right: bottom_button(1),
            delete: bottom_button(2),
            record: bottom_button(3),
        }
    }

    pub fn hit(&self, touch: crate::lcd::TouchPoint) -> Option<VoiceInputHit> {
        for (hit, rect) in [
            (VoiceInputHit::Back, self.back),
            (VoiceInputHit::Submit, self.submit),
            (VoiceInputHit::Left, self.left),
            (VoiceInputHit::Right, self.right),
            (VoiceInputHit::Delete, self.delete),
            (VoiceInputHit::Record, self.record),
        ] {
            if contains_touch(rect, touch) {
                return Some(hit);
            }
        }
        None
    }
}

pub fn render_clock<D>(target: &mut D, data: &ClockData<'_>) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Color>,
{
    let frame = target.bounding_box();
    target.clear(Palette::BG)?;
    let s = Scale::new(frame);
    let center_x = frame.center().x;

    draw_meta(
        target,
        "WATCH",
        Point::new(s.sx(24), s.sy(28)),
        Palette::DIM,
        Alignment::Left,
    )?;
    if let Some(percent) = data.battery {
        let color = battery_color(percent);
        draw_meta(
            target,
            &format!("* {percent}%"),
            Point::new(s.sx(240), s.sy(28)),
            color,
            Alignment::Right,
        )?;
    }

    let center = Point::new(center_x, s.sy(116));
    for (diameter, color) in [(s.sr(96), Palette::AMBER_DIM), (s.sr(68), Palette::AMBER)] {
        Circle::with_center(center, diameter)
            .into_styled(PrimitiveStyle::with_stroke(color, 2))
            .draw(target)?;
    }
    Circle::with_center(center, s.sr(12))
        .into_styled(PrimitiveStyle::with_fill(Palette::AMBER))
        .draw(target)?;

    let baseline_y = s.sy(211);
    Text::with_alignment(
        &format!("{:02}", data.hours),
        Point::new(center_x - s.sw(44) as i32, baseline_y),
        time_style(Palette::TEXT),
        Alignment::Center,
    )
    .draw(target)?;
    Text::with_alignment(
        &format!("{:02}", data.minutes),
        Point::new(center_x + s.sw(44) as i32, baseline_y),
        time_style(Palette::TEXT),
        Alignment::Center,
    )
    .draw(target)?;
    let colon_size = s.sr(6).max(8);
    let colon_x = center_x - colon_size as i32 / 2;
    for y in [baseline_y - s.sh(53) as i32, baseline_y - s.sh(20) as i32] {
        Rectangle::new(Point::new(colon_x, y), Size::new(colon_size, colon_size))
            .into_styled(PrimitiveStyle::with_fill(Palette::TEXT))
            .draw(target)?;
    }
    draw_meta(
        target,
        &format!("{:04}-{:02}-{:02}", data.year, data.month, data.day),
        Point::new(center_x, s.sy(244)),
        Palette::MUTED,
        Alignment::Center,
    )?;
    draw_meta(
        target,
        data.status,
        Point::new(center_x, s.sy(286)),
        Palette::PURPLE,
        Alignment::Center,
    )?;
    Ok(())
}

/// Render a non-interactive notice using the same visual language as the other watch screens.
pub fn render_notice<D>(target: &mut D, data: &NoticeData<'_>) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Color>,
{
    let frame = target.bounding_box();
    target.clear(Palette::BG)?;
    let s = Scale::new(frame);
    let panel = Rectangle::new(
        Point::new(s.sx(20), s.sy(66)),
        Size::new(s.sw(224), s.sh(196)),
    );
    round_rect(target, panel, s.sr(16), Palette::PANEL_2, None)?;
    let title_y = if data.text.is_empty() {
        panel.center().y + 5
    } else {
        panel.top_left.y + s.sh(54) as i32
    };
    draw_label(
        target,
        data.title,
        Point::new(panel.center().x, title_y),
        Palette::TEXT,
        Alignment::Center,
    )?;
    if !data.text.is_empty() {
        let text_rect = Rectangle::new(
            Point::new(
                panel.top_left.x + s.sw(16) as i32,
                panel.top_left.y + s.sh(82) as i32,
            ),
            Size::new(
                panel.size.width.saturating_sub(s.sw(32)),
                panel.size.height.saturating_sub(s.sh(98)),
            ),
        );
        let text_box_style = embedded_text::style::TextBoxStyleBuilder::new()
            .height_mode(embedded_text::style::HeightMode::FitToText)
            .alignment(embedded_text::alignment::HorizontalAlignment::Center)
            .line_height(embedded_graphics::text::LineHeight::Pixels(22))
            .paragraph_spacing(10)
            .build();
        TextBox::with_textbox_style(
            data.text,
            text_rect,
            terminal_style(Palette::MUTED),
            text_box_style,
        )
        .draw(target)?;
    }
    Ok(())
}

pub fn render_main_menu<D>(
    target: &mut D,
    top: &MenuTile<'_>,
    bottom: &MenuTile<'_>,
) -> Result<MainMenuHitRegions, D::Error>
where
    D: DrawTarget<Color = Color>,
{
    let frame = target.bounding_box();
    target.clear(Palette::BG)?;
    let s = Scale::new(frame);
    draw_icon_button(
        target,
        s,
        Point::new(s.sx(33), s.sy(33)),
        Icon::Back,
        Palette::MUTED,
        Palette::SURFACE_2,
    )?;
    let first = Rectangle::new(
        Point::new(s.sx(22), s.sy(66)),
        Size::new(s.sw(220), s.sh(113)),
    );
    let second = Rectangle::new(
        Point::new(s.sx(22), s.sy(193)),
        Size::new(s.sw(220), s.sh(113)),
    );
    draw_tile(target, s, first, top, true)?;
    draw_tile(target, s, second, bottom, false)?;
    Ok(MainMenuHitRegions {
        back: top_left_scaled_hit_rect(64),
        top: first,
        bottom: second,
    })
}

pub fn render_session_list<D>(
    target: &mut D,
    data: &SessionListData<'_>,
) -> Result<SessionListHitRegions, D::Error>
where
    D: DrawTarget<Color = Color>,
{
    let frame = target.bounding_box();
    target.clear(Palette::BG)?;
    let s = Scale::new(frame);
    draw_icon_button(
        target,
        s,
        Point::new(s.sx(33), s.sy(33)),
        Icon::Back,
        Palette::MUTED,
        Palette::SURFACE_2,
    )?;
    draw_label(
        target,
        data.title,
        Point::new(s.sx(58), s.sy(37)),
        Palette::TEXT,
        Alignment::Left,
    )?;
    if let Some(percent) = data.battery {
        draw_meta(
            target,
            &format!("* {percent}%"),
            Point::new(s.sx(244), s.sy(35)),
            battery_color(percent),
            Alignment::Right,
        )?;
    }

    let row_x = s.sx(20);
    let row_w = s.sw(224);
    let row_h = s.sh(34).max(34);
    let row_gap = s.sh(8) as i32;
    let mut hits = SessionListHitRegions::default();
    for (i, row) in data.rows.iter().enumerate() {
        let rect = Rectangle::new(
            Point::new(row_x, s.sy(58) + i as i32 * (row_h as i32 + row_gap)),
            Size::new(row_w, row_h),
        );
        if rect.top_left.y + rect.size.height as i32 > frame.size.height as i32 - s.sh(24) as i32 {
            break;
        }
        hits.rows.push(rect);
        let fill = if row.active {
            Palette::AMBER_SURFACE
        } else {
            Palette::SURFACE
        };
        let stroke = if row.active {
            Some(Palette::AMBER)
        } else {
            None
        };
        round_rect(target, rect, s.sr(14), fill, stroke)?;
        let dot = if row.working {
            Palette::AMBER
        } else {
            Palette::GREEN
        };
        Circle::with_center(
            rect.top_left + Point::new(s.sw(16) as i32, row_h as i32 / 2),
            s.sr(8),
        )
        .into_styled(PrimitiveStyle::with_fill(dot))
        .draw(target)?;
        draw_label(
            target,
            row.label,
            rect.top_left + Point::new(s.sw(34) as i32, row_h as i32 / 2 + 6),
            Palette::TEXT,
            Alignment::Left,
        )?;
        draw_text(
            target,
            ">",
            Point::new(
                rect.top_left.x + rect.size.width as i32 - s.sw(14) as i32,
                rect.center().y + 5,
            ),
            Palette::DIM,
            Alignment::Center,
        )?;
    }
    if !data.footer.is_empty() {
        draw_meta(
            target,
            data.footer,
            Point::new(frame.center().x, frame.size.height as i32 - 8),
            Palette::FAINT,
            Alignment::Center,
        )?;
    }
    Ok(hits)
}

pub fn render_voice_input<D>(target: &mut D, data: &VoiceInputData<'_>) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Color>,
{
    let frame = target.bounding_box();
    target.clear(Palette::BG)?;
    let s = Scale::new(frame);
    let state_color = match data.state {
        VoiceState::Idle => Palette::AMBER,
        VoiceState::Connecting => Color::CSS_YELLOW,
        VoiceState::Listening => Palette::GREEN,
        VoiceState::Error => Palette::RED,
    };

    draw_icon_button(
        target,
        s,
        Point::new(s.sx(33), s.sy(33)),
        Icon::Back,
        Palette::MUTED,
        Palette::SURFACE_2,
    )?;
    draw_meta(
        target,
        "Edit task",
        Point::new(frame.center().x, s.sy(36)),
        Palette::DIM,
        Alignment::Center,
    )?;
    draw_icon_button(
        target,
        s,
        Point::new(s.sx(231), s.sy(33)),
        Icon::Submit,
        Palette::PANEL,
        Palette::AMBER,
    )?;

    let edit = Rectangle::new(
        Point::new(s.sx(20), s.sy(58)),
        Size::new(s.sw(224), s.sh(194)),
    );
    round_rect(target, edit, s.sr(16), Palette::PANEL_2, None)?;
    let text_rect = Rectangle::new(
        edit.top_left + Point::new(s.sw(12) as i32, s.sh(28) as i32),
        Size::new(
            edit.size.width.saturating_sub(s.sw(24)),
            edit.size.height.saturating_sub(s.sh(40)),
        ),
    );
    let text_style = embedded_text::style::TextBoxStyleBuilder::new()
        .height_mode(embedded_text::style::HeightMode::FitToText)
        .alignment(embedded_text::alignment::HorizontalAlignment::Left)
        .line_height(embedded_graphics::text::LineHeight::Pixels(24))
        .paragraph_spacing(12)
        .build();
    TextBox::with_textbox_style(
        data.text,
        text_rect,
        terminal_style(Palette::TEXT),
        text_style,
    )
    .draw(target)?;

    let button_y = s.sy(270);
    let button_w = s.sw(50);
    let button_h = s.sh(40);
    let gap = s.sw(8) as i32;
    for (i, (label, icon, color, fill)) in [
        ("", Some(Icon::Left), Palette::TEXT, Palette::SURFACE_2),
        ("", Some(Icon::Right), Palette::TEXT, Palette::SURFACE_2),
        ("Del", None, Palette::RED, Palette::SURFACE_2),
        ("Rec", None, state_color, Palette::AMBER_SURFACE),
    ]
    .iter()
    .enumerate()
    {
        let rect = Rectangle::new(
            Point::new(s.sx(20) + i as i32 * (button_w as i32 + gap), button_y),
            Size::new(button_w, button_h),
        );
        round_rect(target, rect, s.sr(14), *fill, None)?;
        if let Some(icon) = icon {
            draw_icon(target, s, rect.center(), *icon, *color)?;
        } else {
            draw_text(
                target,
                label,
                rect.center() + Point::new(0, 5),
                *color,
                Alignment::Center,
            )?;
        }
    }
    Ok(())
}

fn draw_tile<D>(
    target: &mut D,
    s: Scale,
    rect: Rectangle,
    tile: &MenuTile<'_>,
    selected: bool,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Color>,
{
    let fill = if selected {
        Palette::AMBER_SURFACE
    } else {
        Palette::SURFACE
    };
    let stroke = if selected {
        Some(Palette::AMBER_DIM)
    } else {
        Some(Palette::SURFACE_2)
    };
    round_rect(target, rect, s.sr(24), fill, stroke)?;
    let icon_center = Point::new(rect.center().x, rect.center().y - s.sh(14) as i32);
    Circle::with_center(icon_center, s.sr(34))
        .into_styled(PrimitiveStyle::with_fill(tile.accent))
        .draw(target)?;
    draw_icon(target, s, icon_center, tile.icon, Palette::PANEL)?;
    draw_label(
        target,
        tile.label,
        Point::new(rect.center().x, rect.center().y + s.sh(22) as i32),
        Palette::TEXT,
        Alignment::Center,
    )
}

fn draw_icon_button<D>(
    target: &mut D,
    s: Scale,
    center: Point,
    icon: Icon,
    color: Color,
    fill: Color,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Color>,
{
    Circle::with_center(center, s.sr(26))
        .into_styled(PrimitiveStyle::with_fill(fill))
        .draw(target)?;
    draw_icon(target, s, center, icon, color)
}

fn draw_icon<D>(
    target: &mut D,
    s: Scale,
    center: Point,
    icon: Icon,
    color: Color,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Color>,
{
    match icon {
        Icon::Back => draw_text(
            target,
            "<",
            center + Point::new(0, 5),
            color,
            Alignment::Center,
        ),
        Icon::Left => draw_triangle(target, center, crate::touch::SwipeDirection::Left, color),
        Icon::Right => draw_triangle(target, center, crate::touch::SwipeDirection::Right, color),
        Icon::Submit => draw_check(target, center, color),
        Icon::Agent | Icon::Settings => {
            Circle::with_center(center, s.sr(6))
                .into_styled(PrimitiveStyle::with_fill(color))
                .draw(target)?;
            Ok(())
        }
    }
}

fn draw_triangle<D>(
    target: &mut D,
    center: Point,
    direction: crate::touch::SwipeDirection,
    color: Color,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Color>,
{
    let style = PrimitiveStyle::with_fill(color);
    let triangle = match direction {
        crate::touch::SwipeDirection::Left => Triangle::new(
            center + Point::new(-14, 0),
            center + Point::new(10, -14),
            center + Point::new(10, 14),
        ),
        crate::touch::SwipeDirection::Right => Triangle::new(
            center + Point::new(14, 0),
            center + Point::new(-10, -14),
            center + Point::new(-10, 14),
        ),
        crate::touch::SwipeDirection::Up => Triangle::new(
            center + Point::new(0, -14),
            center + Point::new(-14, 10),
            center + Point::new(14, 10),
        ),
        crate::touch::SwipeDirection::Down => Triangle::new(
            center + Point::new(0, 14),
            center + Point::new(-14, -10),
            center + Point::new(14, -10),
        ),
    };
    triangle.into_styled(style).draw(target)
}

fn draw_check<D>(target: &mut D, center: Point, color: Color) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Color>,
{
    let style = PrimitiveStyle::with_stroke(color, 3);
    Line::new(center + Point::new(-9, 0), center + Point::new(-2, 8))
        .into_styled(style)
        .draw(target)?;
    Line::new(center + Point::new(-2, 8), center + Point::new(11, -10))
        .into_styled(style)
        .draw(target)
}

fn round_rect<D>(
    target: &mut D,
    rect: Rectangle,
    radius: u32,
    fill: Color,
    stroke: Option<Color>,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Color>,
{
    let mut builder = PrimitiveStyleBuilder::new().fill_color(fill);
    if let Some(stroke) = stroke {
        builder = builder.stroke_color(stroke).stroke_width(1);
    }
    RoundedRectangle::with_equal_corners(rect, Size::new(radius, radius))
        .into_styled(builder.build())
        .draw(target)
}

fn draw_text<D>(
    target: &mut D,
    text: &str,
    point: Point,
    color: Color,
    alignment: Alignment,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Color>,
{
    Text::with_alignment(text, point, text_style(color), alignment)
        .draw(target)
        .map(|_| ())
}

fn draw_meta<D>(
    target: &mut D,
    text: &str,
    point: Point,
    color: Color,
    alignment: Alignment,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Color>,
{
    Text::with_alignment(text, point, meta_style(color), alignment)
        .draw(target)
        .map(|_| ())
}

fn draw_label<D>(
    target: &mut D,
    text: &str,
    point: Point,
    color: Color,
    alignment: Alignment,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Color>,
{
    Text::with_alignment(text, point, label_style(color), alignment)
        .draw(target)
        .map(|_| ())
}

fn text_style(color: Color) -> U8g2TextStyle<Color> {
    U8g2TextStyle::new(u8g2_fonts::fonts::u8g2_font_wqy12_t_gb2312a, color)
}

fn meta_style(color: Color) -> U8g2TextStyle<Color> {
    U8g2TextStyle::new(u8g2_fonts::fonts::u8g2_font_6x13_tf, color)
}

fn label_style(color: Color) -> U8g2TextStyle<Color> {
    U8g2TextStyle::new(u8g2_fonts::fonts::u8g2_font_helvB12_tf, color)
}

fn terminal_style(color: Color) -> U8g2TextStyle<Color> {
    U8g2TextStyle::new(u8g2_fonts::fonts::u8g2_font_unifont_t_gb2312, color)
}

fn time_style(color: Color) -> U8g2TextStyle<Color> {
    U8g2TextStyle::new(u8g2_fonts::fonts::u8g2_font_logisoso78_tn, color)
}

fn battery_color(percent: u8) -> Color {
    match percent {
        0..=20 => Palette::RED,
        21..=60 => Palette::AMBER,
        _ => Palette::GREEN,
    }
}

fn contains_touch(rect: Rectangle, touch: crate::lcd::TouchPoint) -> bool {
    let x = touch.x as i32;
    let y = touch.y as i32;
    let left = rect.top_left.x;
    let top = rect.top_left.y;
    let right = left + rect.size.width as i32;
    let bottom = top + rect.size.height as i32;
    x >= left && x < right && y >= top && y < bottom
}

fn top_left_scaled_hit_rect(size: u32) -> Rectangle {
    let frame = Rectangle::new(
        Point::zero(),
        Size::new(crate::lcd::LCD_WIDTH as u32, crate::lcd::LCD_HEIGHT as u32),
    );
    let s = Scale::new(frame);
    Rectangle::new(Point::zero(), Size::new(s.sw(size), s.sh(size)))
}
