//! Builds what the panel shows from the shared data. Pure logic.

use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use usage_core::{limits::Window, Totals};

use crate::{
    data::{LimitsView, ScanView},
    settings::Settings,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarState {
    Fresh,
    Stale,
    Expired,
    Missing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BarRow {
    pub label: String,
    pub right: String,
    pub pct: Option<f64>,
    pub state: BarState,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewModel {
    pub bars: Vec<BarRow>,
    pub today: String,
    pub today_sub: String,
    pub footer: String,
    pub footer_warn: bool,
    /// Expanded-only rows: (left, right). Empty left = section header in right.
    pub detail: Vec<(String, String)>,
    pub expanded: bool,
}

pub const ROW_BAR: f32 = 44.0;
pub const ROW_TEXT: f32 = 17.0;
pub const PAD: f32 = 16.0;

impl ViewModel {
    /// Panel height in logical pixels.
    pub fn height(&self) -> f32 {
        let mut h = PAD + 4.0;
        h += self.bars.len() as f32 * ROW_BAR;
        h += 8.0 + ROW_TEXT * 2.0; // today + sub
        if self.expanded {
            h += 6.0 + self.detail.len() as f32 * ROW_TEXT;
        }
        h += 6.0 + ROW_TEXT; // footer
        h + PAD - 4.0
    }
}

pub fn fmt_tokens(n: u64) -> String {
    if n >= 10_000_000 {
        format!("{:.0}M", n as f64 / 1e6)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 10_000 {
        format!("{:.0}k", n as f64 / 1e3)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

pub fn fmt_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    if secs >= 86_400 {
        format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3600)
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else {
        format!("{m}m")
    }
}

fn fmt_age(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else {
        format!("{}h {}m ago", s / 3600, (s % 3600) / 60)
    }
}

fn bar(label: &str, w: Option<&Window>, age: Option<Duration>, stale_after: Duration, now: DateTime<Utc>, short_reset: bool) -> BarRow {
    let w = match w {
        Some(w) => w,
        None => {
            return BarRow { label: label.into(), right: "no data".into(), pct: None, state: BarState::Missing };
        }
    };
    let expired = w.resets_at.map_or(false, |r| now >= r);
    let stale = age.map_or(true, |a| a > stale_after);
    if expired {
        return BarRow {
            label: label.into(),
            right: "window reset - no newer reading".into(),
            pct: None,
            state: BarState::Expired,
        };
    }
    let right = match w.resets_at {
        Some(r) if short_reset => format!("resets in {}", fmt_duration(r - now)),
        Some(r) => format!("resets {}", r.with_timezone(&Local).format("%a %H:%M")),
        None => String::new(),
    };
    BarRow {
        label: label.into(),
        right,
        pct: Some(w.used_percentage.clamp(0.0, 100.0)),
        state: if stale { BarState::Stale } else { BarState::Fresh },
    }
}

fn totals_line(t: &Totals) -> String {
    let mut s = format!("{} tokens - est. ${:.2}", fmt_tokens(t.usage.total()), t.cost_usd);
    if t.unpriced_tokens > 0 {
        s.push_str(&format!(" (+{} unpriced)", fmt_tokens(t.unpriced_tokens)));
    }
    s
}

pub fn build(scan: &ScanView, limits: &LimitsView, settings: &Settings, now: DateTime<Utc>, now_i: Instant, expanded: bool) -> ViewModel {
    let stale_after = Duration::from_secs(settings.stale_after_secs);
    let age = limits.last_success.map(|t| now_i.duration_since(t));
    let r = limits.reading.as_ref();

    let stale = age.map_or(true, |a| a > stale_after);
    let mut bars = vec![
        bar("Session (5 h)", r.and_then(|r| r.five_hour.as_ref()), age, stale_after, now, true),
        bar("Weekly (7 d)", r.and_then(|r| r.seven_day.as_ref()), age, stale_after, now, false),
    ];
    if let Some(r) = r {
        // Per-model weekly windows from the `limits` array (e.g. "Fable").
        for l in r.limits.iter().filter(|l| l.kind == "weekly_scoped") {
            let label = format!("Weekly {}", l.scope.as_deref().unwrap_or("model"));
            let w = Window { used_percentage: l.percent, resets_at: l.resets_at };
            bars.push(bar(&label, Some(&w), age, stale_after, now, false));
        }
        // Usage credits are real money, so they earn a place on the collapsed face.
        if let Some(c) = r.credits.as_ref().filter(|c| c.enabled) {
            let right = match c.limit_minor {
                Some(lim) => format!("{} of {} this month", c.money(c.used_minor), c.money(lim)),
                None => format!("{} this month", c.money(c.used_minor)),
            };
            bars.push(BarRow {
                label: "Usage credits (billed)".into(),
                right,
                pct: Some(c.percent.clamp(0.0, 100.0)),
                state: if stale { BarState::Stale } else { BarState::Fresh },
            });
        }
        if expanded {
            for (label, w) in [
                ("Weekly Opus", r.seven_day_opus.as_ref()),
                ("Weekly Sonnet", r.seven_day_sonnet.as_ref()),
                ("Weekly (credits incl.)", r.seven_day_overage_included.as_ref()),
            ] {
                if w.is_some() {
                    bars.push(bar(label, w, age, stale_after, now, false));
                }
            }
        }
    }

    let (today, today_sub) = if scan.loading {
        let (d, t) = scan.progress;
        ("Scanning transcripts...".to_string(), if t > 0 { format!("{d} / {t} files") } else { String::new() })
    } else if !scan.root_exists {
        ("No Claude Code transcripts found".to_string(), String::new())
    } else {
        (format!("Today: {}", totals_line(&scan.today)), "Claude Code on this PC - list-price estimate, not billing".to_string())
    };

    let (footer, footer_warn) = match (&limits.error, age) {
        (None, Some(a)) => (format!("limits fetched {}", fmt_age(a)), a > stale_after),
        (Some(e), Some(a)) => (format!("last fetch failed ({e}); showing reading from {}", fmt_age(a)), true),
        (Some(e), None) => (format!("limits unavailable: {e}"), true),
        (None, None) => (if limits.fetching { "fetching limits...".into() } else { "limits not fetched yet".into() }, false),
    };

    let mut detail = Vec::new();
    if expanded {
        if let Some(r) = r {
            if !r.breakdown.is_empty() {
                detail.push((String::new(), "Weekly window by surface".into()));
                for b in r.breakdown.iter().filter(|b| b.percent > 0.0) {
                    detail.push((b.display_name.clone(), format!("{:.0}% of this week's use", b.percent)));
                }
            }
        }
        if !scan.by_model.is_empty() {
            detail.push((String::new(), "By model - last 7 days".into()));
            for m in scan.by_model.iter().take(5) {
                detail.push((m.model.clone(), totals_line(&m.totals)));
            }
        }
        if !scan.daily.is_empty() {
            detail.push((String::new(), "Last 7 days".into()));
            for d in &scan.daily {
                detail.push((d.date.format("%a %d %b").to_string(), totals_line(&d.totals)));
            }
        }
        detail.push((String::new(), "click to collapse - right-click for menu".into()));
    }

    ViewModel { bars, today, today_sub, footer, footer_warn, detail, expanded }
}

#[cfg(test)]
mod tests {
    use super::*;
    use usage_core::LimitsReading;

    fn reading(now: DateTime<Utc>) -> LimitsReading {
        LimitsReading {
            fetched_at: now,
            five_hour: Some(Window { used_percentage: 23.5, resets_at: Some(now + chrono::Duration::minutes(133)) }),
            seven_day: Some(Window { used_percentage: 41.0, resets_at: Some(now + chrono::Duration::days(3)) }),
            ..Default::default()
        }
    }

    #[test]
    fn fresh_reading_renders_bars_and_countdown() {
        let now = Utc::now();
        let now_i = Instant::now();
        let limits = LimitsView { reading: Some(reading(now)), last_success: Some(now_i), ..Default::default() };
        let vm = build(&ScanView::default(), &limits, &Settings::default(), now, now_i, false);
        assert_eq!(vm.bars.len(), 2);
        assert_eq!(vm.bars[0].state, BarState::Fresh);
        assert_eq!(vm.bars[0].pct, Some(23.5));
        assert_eq!(vm.bars[0].right, "resets in 2h 13m");
        assert!(vm.bars[1].right.starts_with("resets "));
        assert!(!vm.footer_warn);
    }

    #[test]
    fn old_reading_is_stale_and_passed_reset_is_expired() {
        let now = Utc::now();
        let now_i = Instant::now();
        let mut r = reading(now);
        r.seven_day = Some(Window { used_percentage: 41.0, resets_at: Some(now - chrono::Duration::minutes(1)) });
        let limits = LimitsView { reading: Some(r), last_success: Some(now_i - Duration::from_secs(3600)), ..Default::default() };
        let vm = build(&ScanView::default(), &limits, &Settings::default(), now, now_i, false);
        assert_eq!(vm.bars[0].state, BarState::Stale);
        assert_eq!(vm.bars[1].state, BarState::Expired);
        assert_eq!(vm.bars[1].pct, None);
        assert!(vm.footer_warn);
    }

    #[test]
    fn no_reading_and_error_footer() {
        let now = Utc::now();
        let now_i = Instant::now();
        let limits = LimitsView { error: Some("no OAuth token available".into()), ..Default::default() };
        let vm = build(&ScanView::default(), &limits, &Settings::default(), now, now_i, false);
        assert_eq!(vm.bars[0].state, BarState::Missing);
        assert!(vm.footer.contains("no OAuth token"));
        assert!(vm.footer_warn);
    }

    #[test]
    fn loading_state_and_expanded_height() {
        let now = Utc::now();
        let now_i = Instant::now();
        let scan = ScanView { loading: true, progress: (3, 26), ..Default::default() };
        let vm = build(&scan, &LimitsView::default(), &Settings::default(), now, now_i, false);
        assert_eq!(vm.today, "Scanning transcripts...");
        assert_eq!(vm.today_sub, "3 / 26 files");
        let collapsed = vm.height();
        let vm2 = build(&scan, &LimitsView::default(), &Settings::default(), now, now_i, true);
        assert!(vm2.height() > collapsed);
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_500), "1.5k");
        assert_eq!(fmt_tokens(25_000), "25k");
        assert_eq!(fmt_tokens(3_260_000), "3.3M");
        assert_eq!(fmt_duration(chrono::Duration::seconds(7980)), "2h 13m");
        assert_eq!(fmt_duration(chrono::Duration::seconds(90_000)), "1d 1h");
        assert_eq!(fmt_duration(chrono::Duration::seconds(-5)), "0m");
    }
}
