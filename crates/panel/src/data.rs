//! Background data thread: transcript scanning and limit fetching.
//!
//! The UI thread never blocks on I/O. It reads `Shared` under a short lock and
//! is told about updates via `PostMessageW(WM_APP_DATA)`. `Shared` also holds
//! the live settings: the window and the settings page update them, the data
//! thread reads them on every loop.

use std::{
    sync::{Arc, Condvar, Mutex, RwLock},
    thread,
    time::{Duration, Instant},
};

use chrono::{Local, Utc};
use usage_core::{aggregate, discovery, limits, DayTotals, LimitsReading, ModelTotals, PricingTable, Scanner, Totals};

use crate::settings::{log, Settings};

#[derive(Debug, Clone, Default)]
pub struct ScanView {
    pub loading: bool,
    pub progress: (usize, usize),
    pub today: Totals,
    pub daily: Vec<DayTotals>,
    pub by_model: Vec<ModelTotals>,
    pub responses: usize,
    pub root_exists: bool,
    pub last_scan: Option<Instant>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LimitsView {
    pub reading: Option<LimitsReading>,
    pub last_attempt: Option<Instant>,
    pub last_success: Option<Instant>,
    pub error: Option<String>,
    pub token_source: Option<String>,
    pub fetching: bool,
    pub last_refresh_failure: Option<Instant>,
}

pub struct Shared {
    pub scan: Mutex<ScanView>,
    pub limits: Mutex<LimitsView>,
    settings: RwLock<(u64, Settings)>,
    wake: Mutex<WakeState>,
    cv: Condvar,
}

#[derive(Default)]
struct WakeState {
    visible: bool,
    refresh_now: bool,
    quit: bool,
}

impl Shared {
    pub fn new(settings: Settings) -> Self {
        Shared {
            scan: Mutex::new(ScanView::default()),
            limits: Mutex::new(LimitsView::default()),
            settings: RwLock::new((0, settings)),
            wake: Mutex::new(WakeState::default()),
            cv: Condvar::new(),
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap().1.clone()
    }

    pub fn settings_version(&self) -> u64 {
        self.settings.read().unwrap().0
    }

    /// Replace the live settings; bumps the version so the window applies them.
    pub fn update_settings(&self, s: Settings) {
        let mut g = self.settings.write().unwrap();
        g.0 += 1;
        g.1 = s;
        drop(g);
        self.cv.notify_all();
    }

    pub fn set_visible(&self, visible: bool) {
        let mut w = self.wake.lock().unwrap();
        if w.visible != visible {
            w.visible = visible;
            if visible {
                w.refresh_now = true;
            }
            self.cv.notify_all();
        }
    }

    pub fn refresh_now(&self) {
        let mut w = self.wake.lock().unwrap();
        w.refresh_now = true;
        self.cv.notify_all();
    }

    pub fn quit(&self) {
        let mut w = self.wake.lock().unwrap();
        w.quit = true;
        self.cv.notify_all();
    }
}

pub trait Notifier: Send + 'static {
    fn notify(&self);
}

pub fn spawn(shared: Arc<Shared>, notifier: Box<dyn Notifier>) {
    thread::Builder::new()
        .name("data".into())
        .spawn(move || run(shared, notifier))
        .expect("spawn data thread");
}

fn run(shared: Arc<Shared>, notifier: Box<dyn Notifier>) {
    let pricing = PricingTable::builtin();
    let root = discovery::default_projects_dir().unwrap_or_default();
    let mut scanner = Scanner::new(&root).with_retention_days(shared.settings().retain_days);

    {
        let mut s = shared.scan.lock().unwrap();
        s.loading = true;
    }
    notifier.notify();

    // Cold scan with progress so the panel can show a loading state.
    let mut last_progress = Instant::now();
    let report = scanner.scan_with_progress(|done, total| {
        if last_progress.elapsed() > Duration::from_millis(120) {
            last_progress = Instant::now();
            if let Ok(mut s) = shared.scan.lock() {
                s.progress = (done, total);
            }
            notifier.notify();
        }
    });
    match report {
        Ok(r) => log(&format!(
            "cold scan: {} files, {} responses, {} malformed, {} bytes",
            r.files_seen,
            scanner.store.len(),
            r.lines_malformed,
            r.bytes_read
        )),
        Err(e) => log(&format!("cold scan error: {e}")),
    }
    publish_scan(&shared, &scanner, &pricing, false, None);
    notifier.notify();

    let mut last_limits_attempt: Option<Instant> = None;
    loop {
        let settings = shared.settings();
        let visible = shared.wake.lock().unwrap().visible;
        let limits_period = Duration::from_secs(if visible { settings.limits_visible_secs } else { settings.limits_hidden_secs });
        let min_gap = Duration::from_secs(settings.limits_min_gap_secs);
        let due = match last_limits_attempt {
            None => true,
            Some(t) => t.elapsed() >= limits_period.max(min_gap),
        };
        if due {
            last_limits_attempt = Some(Instant::now());
            fetch_limits(&settings, &shared);
            notifier.notify();
        }

        // Wait for the next rescan period, a visibility change, or an explicit refresh.
        let rescan_period = Duration::from_secs(if visible { settings.rescan_visible_secs } else { settings.rescan_hidden_secs });
        let mut w = shared.wake.lock().unwrap();
        if !w.refresh_now && !w.quit {
            let (guard, _) = shared.cv.wait_timeout(w, rescan_period).unwrap();
            w = guard;
        }
        if w.quit {
            return;
        }
        let forced = w.refresh_now;
        w.refresh_now = false;
        drop(w);

        match scanner.scan() {
            Ok(r) => {
                if r.events_added > 0 || r.events_updated > 0 || r.files_reset > 0 || forced {
                    publish_scan(&shared, &scanner, &pricing, false, None);
                    notifier.notify();
                }
            }
            Err(e) => {
                publish_scan(&shared, &scanner, &pricing, false, Some(e.to_string()));
                notifier.notify();
            }
        }
        if forced && last_limits_attempt.map_or(true, |t| t.elapsed() >= min_gap) {
            // A forced refresh (panel just opened) may fetch limits early, but never
            // more often than the minimum gap.
            last_limits_attempt = Some(Instant::now());
            fetch_limits(&settings, &shared);
            notifier.notify();
        }
    }
}

fn publish_scan(shared: &Shared, scanner: &Scanner, pricing: &PricingTable, loading: bool, error: Option<String>) {
    let now = Utc::now();
    let today = aggregate::today(&scanner.store, pricing, &Local, now);
    let end_date = aggregate::local_date(&Local, now);
    let daily = aggregate::daily(&scanner.store, pricing, &Local, end_date, 7);
    let (ws, _) = aggregate::day_bounds(&Local, end_date - chrono::Duration::days(6));
    let (_, we) = aggregate::day_bounds(&Local, end_date);
    let by_model = aggregate::by_model(&scanner.store, pricing, ws, we);
    let mut s = shared.scan.lock().unwrap();
    s.loading = loading;
    s.today = today;
    s.daily = daily;
    s.by_model = by_model;
    s.responses = scanner.store.len();
    s.root_exists = scanner.root().is_dir();
    s.last_scan = Some(Instant::now());
    s.error = error;
}

fn fetch_limits(settings: &Settings, shared: &Shared) {
    let allow_refresh = {
        let mut l = shared.limits.lock().unwrap();
        l.fetching = true;
        l.last_attempt = Some(Instant::now());
        settings.auto_refresh_token
            && l.last_refresh_failure.map_or(true, |t| t.elapsed() >= Duration::from_secs(settings.refresh_retry_secs))
    };
    let creds = discovery::claude_config_dir().map(|d| d.join(".credentials.json"));
    let out = limits::get_reading(
        settings.oauth_token.as_deref(),
        creds.as_deref(),
        &settings.oauth_client_id,
        allow_refresh,
        Duration::from_secs(8),
    );
    match out.refreshed {
        Some(true) => log("token refreshed and written back to credentials file"),
        Some(false) => log("token refresh failed; will retry after the configured delay"),
        None => {}
    }
    let mut l = shared.limits.lock().unwrap();
    l.fetching = false;
    if out.refreshed == Some(false) {
        l.last_refresh_failure = Some(Instant::now());
    }
    if let Some(src) = out.source {
        l.token_source = Some(format!("{src:?}"));
    }
    match (out.reading, out.error) {
        (Some(reading), _) => {
            l.reading = Some(reading);
            l.last_success = Some(Instant::now());
            l.error = None;
        }
        (None, Some(e)) => {
            log(&format!("limits fetch failed: {e}"));
            l.error = Some(e.to_string());
        }
        (None, None) => l.error = Some("no reading".into()),
    }
}
