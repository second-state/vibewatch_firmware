#![allow(dead_code)]

use crate::lcd::{TouchEvent, TouchPoint};

pub const DEFAULT_SWIPE_THRESHOLD_PX: i32 = 40;
pub const DEFAULT_LONG_PRESS_DELAY: std::time::Duration = std::time::Duration::from_millis(200);
pub const DEFAULT_LONG_PRESS_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchGesture {
    Press {
        point: TouchPoint,
    },
    Click {
        start: TouchPoint,
        end: TouchPoint,
    },
    LongPress {
        start: TouchPoint,
        end: TouchPoint,
        duration: std::time::Duration,
    },
    Swipe {
        start: TouchPoint,
        end: TouchPoint,
        direction: SwipeDirection,
        dx: i32,
        dy: i32,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct TouchGestureConfig {
    pub swipe_threshold_px: i32,
    pub long_press_delay: std::time::Duration,
    pub long_press_interval: std::time::Duration,
}

impl Default for TouchGestureConfig {
    fn default() -> Self {
        Self {
            swipe_threshold_px: DEFAULT_SWIPE_THRESHOLD_PX,
            long_press_delay: DEFAULT_LONG_PRESS_DELAY,
            long_press_interval: DEFAULT_LONG_PRESS_INTERVAL,
        }
    }
}

pub struct TouchInput {
    rx: tokio::sync::mpsc::Receiver<TouchEvent>,
    config: TouchGestureConfig,
    active: Option<TouchGestureInProgress>,
}

#[derive(Debug, Clone, Copy)]
struct TouchGestureInProgress {
    start: TouchPoint,
    started_at: tokio::time::Instant,
    next_long_press_at: tokio::time::Instant,
    last_press: TouchPoint,
    swipe_candidate: Option<TouchPoint>,
    press_emitted: bool,
}

impl TouchInput {
    pub fn new(rx: tokio::sync::mpsc::Receiver<TouchEvent>) -> Self {
        Self::with_config(rx, TouchGestureConfig::default())
    }

    pub fn with_config(
        rx: tokio::sync::mpsc::Receiver<TouchEvent>,
        config: TouchGestureConfig,
    ) -> Self {
        Self {
            rx,
            config,
            active: None,
        }
    }

    pub fn cancel_active_gesture(&mut self) {
        self.active = None;
    }

    pub async fn next_gesture(&mut self) -> Option<TouchGesture> {
        loop {
            if self.active.is_none() {
                let TouchEvent::Press(start) = self.rx.recv().await? else {
                    continue;
                };
                let started_at = tokio::time::Instant::now();
                self.active = Some(TouchGestureInProgress {
                    start,
                    started_at,
                    next_long_press_at: started_at + self.config.long_press_delay,
                    last_press: start,
                    swipe_candidate: None,
                    press_emitted: false,
                });
            }

            if let Some(active) = self.active.as_mut() {
                if !active.press_emitted {
                    active.press_emitted = true;
                    return Some(TouchGesture::Press {
                        point: active.start,
                    });
                }
            }

            loop {
                let active = self.active.as_ref()?;
                let next_long_press_at = active.next_long_press_at;
                let can_long_press = active.swipe_candidate.is_none();
                tokio::select! {
                    _ = tokio::time::sleep_until(next_long_press_at), if can_long_press => {
                        let active = self.active.as_mut()?;
                        let now = tokio::time::Instant::now();
                        active.next_long_press_at = now + self.config.long_press_interval;
                        return Some(TouchGesture::LongPress {
                            start: active.start,
                            end: active.last_press,
                            duration: now.duration_since(active.started_at),
                        });
                    }
                    event = self.rx.recv() => {
                        match event? {
                            TouchEvent::Press(touch) => {
                                let threshold = self.config.swipe_threshold_px;
                                let active = self.active.as_mut()?;
                                active.last_press = touch;
                                let dx = touch.x as i32 - active.start.x as i32;
                                let dy = touch.y as i32 - active.start.y as i32;
                                if swipe_direction(threshold, dx, dy).is_some() {
                                    active.swipe_candidate = Some(touch);
                                }
                            }
                            TouchEvent::Release(end) => {
                                let active = self.active.take()?;
                                return Some(self.classify_release(
                                    active.start,
                                    end,
                                    active.swipe_candidate,
                                ));
                            }
                        }
                    }
                }
            }
        }
    }

    pub async fn wait_release(&mut self) -> Option<TouchPoint> {
        loop {
            match self.rx.recv().await? {
                TouchEvent::Press(_) => {}
                TouchEvent::Release(touch) => return Some(touch),
            }
        }
    }

    fn classify_release(
        &self,
        start: TouchPoint,
        end: TouchPoint,
        swipe_candidate: Option<TouchPoint>,
    ) -> TouchGesture {
        if let Some(candidate) = swipe_candidate {
            return self.classify_swipe(start, end).unwrap_or_else(|| {
                self.classify_swipe(start, candidate)
                    .unwrap_or(TouchGesture::Click { start, end })
            });
        }

        self.classify_click_or_swipe(start, end)
    }

    fn classify_click_or_swipe(&self, start: TouchPoint, end: TouchPoint) -> TouchGesture {
        self.classify_swipe(start, end)
            .unwrap_or(TouchGesture::Click { start, end })
    }

    fn classify_swipe(&self, start: TouchPoint, end: TouchPoint) -> Option<TouchGesture> {
        let dx = end.x as i32 - start.x as i32;
        let dy = end.y as i32 - start.y as i32;
        self.swipe_direction(dx, dy)
            .map(|direction| TouchGesture::Swipe {
                start,
                end,
                direction,
                dx,
                dy,
            })
    }

    fn swipe_direction(&self, dx: i32, dy: i32) -> Option<SwipeDirection> {
        swipe_direction(self.config.swipe_threshold_px, dx, dy)
    }
}

fn swipe_direction(threshold_px: i32, dx: i32, dy: i32) -> Option<SwipeDirection> {
    let abs_dx = dx.abs();
    let abs_dy = dy.abs();
    if abs_dx < threshold_px && abs_dy < threshold_px {
        return None;
    }

    if abs_dx >= abs_dy {
        if dx > 0 {
            Some(SwipeDirection::Right)
        } else {
            Some(SwipeDirection::Left)
        }
    } else if dy > 0 {
        Some(SwipeDirection::Down)
    } else {
        Some(SwipeDirection::Up)
    }
}
