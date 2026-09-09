//! Aggregation by time range, day, model and session.
//!
//! Day boundaries are computed in the caller's time zone (`chrono::Local` in
//! the app, a `FixedOffset` in tests) so a session spanning local midnight is
//! split correctly even though transcript timestamps are UTC.

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use serde::Serialize;

use crate::{
    model::TokenUsage,
    pricing::PricingTable,
    store::{EventRef, UsageStore},
};

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Totals {
    pub usage: TokenUsage,
    /// Number of API responses.
    pub requests: u64,
    /// Estimated cost at list price (USD). Estimate, not billing data.
    pub cost_usd: f64,
    /// Tokens from models missing in the pricing table (excluded from `cost_usd`).
    pub unpriced_tokens: u64,
}

impl Totals {
    pub fn add_event(&mut self, ev: &EventRef<'_>, pricing: &PricingTable) {
        self.usage.add(&ev.usage);
        self.requests += 1;
        match pricing.cost(ev.model, &ev.usage, ev.fast) {
            Some(c) => self.cost_usd += c,
            None => self.unpriced_tokens += ev.usage.total(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelTotals {
    pub model: String,
    pub totals: Totals,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionTotals {
    pub session: String,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    pub totals: Totals,
}

#[derive(Debug, Clone, Serialize)]
pub struct DayTotals {
    pub date: NaiveDate,
    pub totals: Totals,
}

pub fn totals(
    store: &UsageStore,
    pricing: &PricingTable,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Totals {
    let mut t = Totals::default();
    for ev in store.between(start, end) {
        t.add_event(&ev, pricing);
    }
    t
}

/// Per-model totals, largest token count first.
pub fn by_model(
    store: &UsageStore,
    pricing: &PricingTable,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<ModelTotals> {
    let mut map: std::collections::BTreeMap<&str, Totals> = Default::default();
    for ev in store.between(start, end) {
        map.entry(ev.model).or_default().add_event(&ev, pricing);
    }
    let mut v: Vec<ModelTotals> = map
        .into_iter()
        .map(|(m, t)| ModelTotals {
            model: m.to_string(),
            totals: t,
        })
        .collect();
    v.sort_by(|a, b| b.totals.usage.total().cmp(&a.totals.usage.total()));
    v
}

/// Per-session totals, most recently active first.
pub fn by_session(
    store: &UsageStore,
    pricing: &PricingTable,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<SessionTotals> {
    let mut map: std::collections::BTreeMap<&str, SessionTotals> = Default::default();
    for ev in store.between(start, end) {
        let e = map.entry(ev.session).or_insert_with(|| SessionTotals {
            session: ev.session.to_string(),
            first: ev.timestamp,
            last: ev.timestamp,
            totals: Totals::default(),
        });
        e.first = e.first.min(ev.timestamp);
        e.last = e.last.max(ev.timestamp);
        e.totals.add_event(&ev, pricing);
    }
    let mut v: Vec<SessionTotals> = map.into_values().collect();
    v.sort_by(|a, b| b.last.cmp(&a.last));
    v
}

/// `[start, end)` in UTC for the local calendar day `date` in `tz`.
pub fn day_bounds<Tz: TimeZone>(tz: &Tz, date: NaiveDate) -> (DateTime<Utc>, DateTime<Utc>) {
    (
        local_midnight(tz, date),
        local_midnight(tz, date + Duration::days(1)),
    )
}

fn local_midnight<Tz: TimeZone>(tz: &Tz, date: NaiveDate) -> DateTime<Utc> {
    // DST gaps can make 00:00 non-existent; walk forward until a valid local time.
    for minutes in [0i64, 30, 60, 90, 120] {
        let naive = date.and_hms_opt(0, 0, 0).unwrap() + Duration::minutes(minutes);
        if let Some(dt) = tz.from_local_datetime(&naive).earliest() {
            return dt.with_timezone(&Utc);
        }
    }
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}

pub fn local_date<Tz: TimeZone>(tz: &Tz, now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(tz).date_naive()
}

/// Totals for the local calendar day containing `now`.
pub fn today<Tz: TimeZone>(
    store: &UsageStore,
    pricing: &PricingTable,
    tz: &Tz,
    now: DateTime<Utc>,
) -> Totals {
    let (s, e) = day_bounds(tz, local_date(tz, now));
    totals(store, pricing, s, e)
}

/// The last `days` local calendar days ending with `end_date` (inclusive), oldest first.
pub fn daily<Tz: TimeZone>(
    store: &UsageStore,
    pricing: &PricingTable,
    tz: &Tz,
    end_date: NaiveDate,
    days: usize,
) -> Vec<DayTotals> {
    (0..days)
        .rev()
        .map(|back| {
            let date = end_date - Duration::days(back as i64);
            let (s, e) = day_bounds(tz, date);
            DayTotals {
                date,
                totals: totals(store, pricing, s, e),
            }
        })
        .collect()
}

/// Totals for the trailing window `[now - window, now)`. Token counts only —
/// this is NOT the subscription limit, whose size is not published.
pub fn trailing(
    store: &UsageStore,
    pricing: &PricingTable,
    now: DateTime<Utc>,
    window: Duration,
) -> Totals {
    totals(store, pricing, now - window, now)
}
