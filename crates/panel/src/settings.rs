//! Hand-editable settings at `%APPDATA%\claude-usage-panel\settings.json`.
//! Missing fields take defaults, so a partial file is fine.

use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Width of the entry hot zone as a fraction of the display width (centered).
    pub hot_zone_width_fraction: f64,
    /// Height of the entry hot zone in logical pixels (DPI-scaled).
    pub hot_zone_height_px: u32,
    /// Extra width on each side for the exit zone, as a fraction of display width.
    pub exit_margin_fraction: f64,
    /// Extra height below the panel for the exit zone, logical pixels.
    pub exit_margin_px: u32,
    /// Cursor must stay in the hot zone this long before the panel shows.
    pub dwell_ms: u64,
    /// Cursor must stay outside the exit zone this long before the panel hides.
    pub hide_delay_ms: u64,
    /// Cursor polling period.
    pub poll_ms: u64,
    /// Slide animation duration.
    pub animation_ms: u64,
    /// "internal" (laptop panel), "primary", or a GDI device name like "\\\\.\\DISPLAY2".
    pub display: String,
    /// JSONL rescan period while the panel is visible / hidden.
    pub rescan_visible_secs: u64,
    pub rescan_hidden_secs: u64,
    /// Limit-endpoint polling period while visible / hidden, and the hard minimum gap.
    pub limits_visible_secs: u64,
    pub limits_hidden_secs: u64,
    pub limits_min_gap_secs: u64,
    /// Readings older than this are drawn as stale.
    pub stale_after_secs: u64,
    /// Long-lived OAuth token from `claude setup-token`. Leave null to fall back to
    /// CLAUDE_CODE_OAUTH_TOKEN, then to Claude Code's stored access token.
    pub oauth_token: Option<String>,
    /// Renew Claude Code's stored token with its refresh token when it has expired
    /// (writes ~/.claude/.credentials.json). Unsupported flow; see README.
    pub auto_refresh_token: bool,
    /// OAuth client id used for the refresh. Default is Claude Code's public id.
    pub oauth_client_id: String,
    /// After a failed refresh, wait this long before trying again.
    pub refresh_retry_secs: u64,
    /// Only look at transcripts modified in the last N days.
    pub retain_days: i64,
    /// Register in HKCU\...\Run so the panel starts at login.
    pub run_at_login: bool,
    /// Panel width in logical pixels.
    pub panel_width_px: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            hot_zone_width_fraction: 0.18,
            hot_zone_height_px: 3,
            exit_margin_fraction: 0.06,
            exit_margin_px: 40,
            dwell_ms: 200,
            hide_delay_ms: 450,
            poll_ms: 40,
            animation_ms: 180,
            display: "internal".into(),
            rescan_visible_secs: 15,
            rescan_hidden_secs: 300,
            limits_visible_secs: 60,
            limits_hidden_secs: 300,
            limits_min_gap_secs: 60,
            stale_after_secs: 900,
            oauth_token: None,
            auto_refresh_token: true,
            oauth_client_id: usage_core::limits::DEFAULT_CLIENT_ID.to_string(),
            refresh_retry_secs: 3600,
            retain_days: 60,
            run_at_login: false,
            panel_width_px: 340,
        }
    }
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| usage_core::discovery::home_dir())
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("claude-usage-panel")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn log_path() -> PathBuf {
    config_dir().join("panel.log")
}

/// Load settings, writing a default file on first run so it can be hand-edited.
pub fn load_or_create() -> (Settings, Option<String>) {
    let path = settings_path();
    match fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Settings>(&text) {
            Ok(s) => (s, None),
            Err(e) => (Settings::default(), Some(format!("settings.json invalid ({e}); using defaults"))),
        },
        Err(_) => {
            let s = Settings::default();
            let _ = fs::create_dir_all(config_dir());
            let _ = fs::write(&path, serde_json::to_string_pretty(&s).unwrap());
            (s, None)
        }
    }
}

pub fn log(msg: &str) {
    use std::io::Write;
    let _ = fs::create_dir_all(config_dir());
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = writeln!(f, "{} {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_takes_defaults() {
        let s: Settings = serde_json::from_str(r#"{"dwell_ms": 500}"#).unwrap();
        assert_eq!(s.dwell_ms, 500);
        assert_eq!(s.hide_delay_ms, Settings::default().hide_delay_ms);
        assert!(s.oauth_token.is_none());
    }
}
