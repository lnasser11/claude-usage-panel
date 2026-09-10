//! Hover state machine. Pure logic, unit-tested without a window.
//!
//! Hidden --(cursor in entry zone)--> Arming --(dwell elapsed)--> Visible
//! Visible --(cursor outside exit zone & panel)--> Leaving --(hide delay)--> Hidden
//! Leaving --(cursor back inside)--> Visible
//!
//! The exit zone is larger than the entry zone and the panel itself counts as
//! inside, which is the hysteresis that stops flicker at the boundary.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Hidden,
    Arming { since: Instant },
    Visible,
    Leaving { since: Instant },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Show,
    Hide,
}

#[derive(Debug, Clone, Copy)]
pub struct Input {
    pub cursor: (i32, i32),
    pub entry: Rect,
    pub exit: Rect,
    /// Current on-screen panel rectangle, if any part of it is visible.
    pub panel: Option<Rect>,
    /// True while a fullscreen app has focus or a drag is in progress: never show.
    pub suppressed: bool,
    /// True while the user has pinned the panel (expanded view): never auto-hide.
    pub pinned: bool,
}

pub struct Tracker {
    pub state: State,
    pub dwell: Duration,
    pub hide_delay: Duration,
}

impl Tracker {
    pub fn new(dwell: Duration, hide_delay: Duration) -> Self {
        Tracker { state: State::Hidden, dwell, hide_delay }
    }

    pub fn is_visible(&self) -> bool {
        matches!(self.state, State::Visible | State::Leaving { .. })
    }

    pub fn tick(&mut self, now: Instant, input: &Input) -> Action {
        let (x, y) = input.cursor;
        let in_entry = input.entry.contains(x, y);
        let inside = input.exit.contains(x, y) || input.panel.map_or(false, |p| p.contains(x, y));

        match self.state {
            State::Hidden => {
                if in_entry && !input.suppressed {
                    self.state = State::Arming { since: now };
                }
                Action::None
            }
            State::Arming { since } => {
                if !in_entry || input.suppressed {
                    self.state = State::Hidden;
                    Action::None
                } else if now.duration_since(since) >= self.dwell {
                    self.state = State::Visible;
                    Action::Show
                } else {
                    Action::None
                }
            }
            State::Visible => {
                if input.pinned {
                    return Action::None;
                }
                if !inside || input.suppressed {
                    self.state = State::Leaving { since: now };
                }
                Action::None
            }
            State::Leaving { since } => {
                if input.pinned || (inside && !input.suppressed) {
                    self.state = State::Visible;
                    Action::None
                } else if now.duration_since(since) >= self.hide_delay {
                    self.state = State::Hidden;
                    Action::Hide
                } else {
                    Action::None
                }
            }
        }
    }

    /// Force-hide (e.g. display change or explicit dismiss).
    pub fn reset(&mut self) {
        self.state = State::Hidden;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zones() -> (Rect, Rect) {
        let entry = Rect { left: 800, top: 0, right: 1120, bottom: 4 };
        let exit = Rect { left: 700, top: 0, right: 1220, bottom: 160 };
        (entry, exit)
    }

    fn input(cursor: (i32, i32), panel: Option<Rect>) -> Input {
        let (entry, exit) = zones();
        Input { cursor, entry, exit, panel, suppressed: false, pinned: false }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn passing_through_does_not_show() {
        let mut t = Tracker::new(ms(200), ms(400));
        let t0 = Instant::now();
        assert_eq!(t.tick(t0, &input((900, 1), None)), Action::None);
        assert_eq!(t.tick(t0 + ms(100), &input((900, 300), None)), Action::None);
        assert_eq!(t.state, State::Hidden);
    }

    #[test]
    fn dwell_then_show() {
        let mut t = Tracker::new(ms(200), ms(400));
        let t0 = Instant::now();
        t.tick(t0, &input((900, 1), None));
        assert_eq!(t.tick(t0 + ms(150), &input((905, 2), None)), Action::None);
        assert_eq!(t.tick(t0 + ms(210), &input((905, 2), None)), Action::Show);
        assert!(t.is_visible());
    }

    #[test]
    fn hysteresis_keeps_panel_open_in_exit_zone_and_over_panel() {
        let mut t = Tracker::new(ms(200), ms(400));
        let t0 = Instant::now();
        t.tick(t0, &input((900, 1), None));
        t.tick(t0 + ms(250), &input((900, 1), None));
        let panel = Rect { left: 790, top: 0, right: 1130, bottom: 120 };
        // Inside exit zone but outside entry zone: still visible.
        assert_eq!(t.tick(t0 + ms(300), &input((720, 150), Some(panel))), Action::None);
        assert_eq!(t.state, State::Visible);
        // Over the panel body: still visible.
        assert_eq!(t.tick(t0 + ms(350), &input((1000, 100), Some(panel))), Action::None);
        assert_eq!(t.state, State::Visible);
    }

    #[test]
    fn leave_then_hide_after_delay_and_reentry_cancels() {
        let mut t = Tracker::new(ms(200), ms(400));
        let t0 = Instant::now();
        t.tick(t0, &input((900, 1), None));
        t.tick(t0 + ms(250), &input((900, 1), None));
        // Move far away.
        assert_eq!(t.tick(t0 + ms(300), &input((100, 900), None)), Action::None);
        assert!(matches!(t.state, State::Leaving { .. }));
        // Come back before the delay: cancel.
        assert_eq!(t.tick(t0 + ms(500), &input((900, 50), None)), Action::None);
        assert_eq!(t.state, State::Visible);
        // Leave for good.
        t.tick(t0 + ms(600), &input((100, 900), None));
        assert_eq!(t.tick(t0 + ms(900), &input((100, 900), None)), Action::None);
        assert_eq!(t.tick(t0 + ms(1001), &input((100, 900), None)), Action::Hide);
        assert_eq!(t.state, State::Hidden);
    }

    #[test]
    fn suppressed_never_shows_and_hides_visible() {
        let mut t = Tracker::new(ms(200), ms(400));
        let t0 = Instant::now();
        let mut i = input((900, 1), None);
        i.suppressed = true;
        t.tick(t0, &i);
        t.tick(t0 + ms(500), &i);
        assert_eq!(t.state, State::Hidden);

        let mut t = Tracker::new(ms(200), ms(400));
        t.tick(t0, &input((900, 1), None));
        t.tick(t0 + ms(250), &input((900, 1), None));
        assert!(t.is_visible());
        let mut i = input((900, 1), None);
        i.suppressed = true;
        t.tick(t0 + ms(300), &i);
        assert!(matches!(t.state, State::Leaving { .. }));
        assert_eq!(t.tick(t0 + ms(800), &i), Action::Hide);
    }

    #[test]
    fn pinned_never_auto_hides() {
        let mut t = Tracker::new(ms(200), ms(400));
        let t0 = Instant::now();
        t.tick(t0, &input((900, 1), None));
        t.tick(t0 + ms(250), &input((900, 1), None));
        let mut i = input((100, 900), None);
        i.pinned = true;
        for k in 0..50 {
            assert_eq!(t.tick(t0 + ms(300 + k * 100), &i), Action::None);
        }
        assert_eq!(t.state, State::Visible);
        i.pinned = false;
        t.tick(t0 + ms(6000), &i);
        assert_eq!(t.tick(t0 + ms(6500), &i), Action::Hide);
    }
}
