//! The rate-limit snapshot written by the statusline hook.
//!
//! Source of truth: Claude Code's documented statusLine stdin JSON
//! (`rate_limits.five_hour` / `rate_limits.seven_day`, each with
//! `used_percentage` 0–100 and `resets_at` in Unix epoch seconds). Present only
//! for Pro/Max subscribers and only after the first API response of a session.
//! The hook adds `captured_at` (epoch seconds) and passes the rest through.

use std::{fs, io, path::Path};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LimitWindow {
    pub used_percentage: f64,
    pub resets_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimitSnapshot {
    pub captured_at: DateTime<Utc>,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub context_used_percentage: Option<f64>,
    pub five_hour: Option<LimitWindow>,
    pub seven_day: Option<LimitWindow>,
}

/// How much to trust a window reading right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    /// Captured recently; the window has not reset since.
    Fresh,
    /// Older than the caller's freshness threshold, but the window has not reset yet.
    /// Usage may have grown since capture.
    Stale,
    /// `resets_at` has passed since capture. Nothing is known about the new window.
    Expired,
}

#[derive(Deserialize)]
struct RawFile {
    captured_at: i64,
    session_id: Option<String>,
    model: Option<RawModel>,
    context_window: Option<RawContext>,
    rate_limits: Option<RawLimits>,
}
#[derive(Deserialize)]
struct RawModel {
    id: Option<String>,
    display_name: Option<String>,
}
#[derive(Deserialize)]
struct RawContext {
    used_percentage: Option<f64>,
}
#[derive(Deserialize)]
struct RawLimits {
    five_hour: Option<RawWindow>,
    seven_day: Option<RawWindow>,
}
#[derive(Deserialize)]
struct RawWindow {
    used_percentage: Option<f64>,
    resets_at: Option<i64>,
}

fn window(w: Option<RawWindow>) -> Option<LimitWindow> {
    let w = w?;
    Some(LimitWindow {
        used_percentage: w.used_percentage?,
        resets_at: DateTime::from_timestamp(w.resets_at?, 0)?,
    })
}

impl LimitSnapshot {
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let raw: RawFile = serde_json::from_str(json)?;
        let (five_hour, seven_day) = match raw.rate_limits {
            Some(l) => (window(l.five_hour), window(l.seven_day)),
            None => (None, None),
        };
        Ok(LimitSnapshot {
            captured_at: DateTime::from_timestamp(raw.captured_at, 0).unwrap_or_default(),
            session_id: raw.session_id,
            model: raw.model.and_then(|m| m.display_name.or(m.id)),
            context_used_percentage: raw.context_window.and_then(|c| c.used_percentage),
            five_hour,
            seven_day,
        })
    }

    /// `Ok(None)` when the file does not exist yet.
    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        match fs::read_to_string(path) {
            Ok(s) => Self::from_json(&s)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn age(&self, now: DateTime<Utc>) -> Duration {
        now - self.captured_at
    }

    pub fn state(&self, w: &LimitWindow, now: DateTime<Utc>, fresh_for: Duration) -> WindowState {
        if now >= w.resets_at {
            WindowState::Expired
        } else if self.age(now) > fresh_for {
            WindowState::Stale
        } else {
            WindowState::Fresh
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"captured_at":1788999000,"session_id":"abc","model":{"id":"claude-opus-5","display_name":"Opus 5"},"context_window":{"used_percentage":12.5},"rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":1789002600},"seven_day":{"used_percentage":41.2,"resets_at":1789434000}}}"#;

    #[test]
    fn parses_hook_output() {
        let s = LimitSnapshot::from_json(SAMPLE).unwrap();
        assert_eq!(s.session_id.as_deref(), Some("abc"));
        assert_eq!(s.model.as_deref(), Some("Opus 5"));
        assert_eq!(s.context_used_percentage, Some(12.5));
        let fh = s.five_hour.unwrap();
        assert_eq!(fh.used_percentage, 23.5);
        assert_eq!(fh.resets_at.timestamp(), 1789002600);
        assert_eq!(s.seven_day.unwrap().used_percentage, 41.2);
    }

    #[test]
    fn missing_windows_are_none() {
        let s = LimitSnapshot::from_json(
            r#"{"captured_at":1,"rate_limits":{"five_hour":{"used_percentage":1,"resets_at":2}}}"#,
        )
        .unwrap();
        assert!(s.five_hour.is_some());
        assert!(s.seven_day.is_none());
        let s = LimitSnapshot::from_json(r#"{"captured_at":1}"#).unwrap();
        assert!(s.five_hour.is_none());
    }

    #[test]
    fn window_states() {
        let s = LimitSnapshot::from_json(SAMPLE).unwrap();
        let w = s.five_hour.unwrap();
        let fresh = Duration::minutes(10);
        let t0 = DateTime::from_timestamp(1788999000 + 60, 0).unwrap();
        assert_eq!(s.state(&w, t0, fresh), WindowState::Fresh);
        let t1 = DateTime::from_timestamp(1788999000 + 1800, 0).unwrap();
        assert_eq!(s.state(&w, t1, fresh), WindowState::Stale);
        let t2 = DateTime::from_timestamp(1789002600, 0).unwrap();
        assert_eq!(s.state(&w, t2, fresh), WindowState::Expired);
    }

    #[test]
    fn missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(LimitSnapshot::read(&dir.path().join("nope.json"))
            .unwrap()
            .is_none());
    }
}
