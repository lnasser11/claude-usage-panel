//! Account-level limit windows from Anthropic's OAuth usage endpoint.
//!
//! WARNING — UNSUPPORTED API. `GET https://api.anthropic.com/api/oauth/usage`
//! is what Claude Code's own `/usage` command calls (verified by reading the
//! Claude Code 2.1.260 binary). It is not publicly documented and may change or
//! stop accepting third-party callers at any time. The docs do say it is rate
//! limited, so callers must keep a minimum interval between requests.
//!
//! Response schema: field names `five_hour`, `seven_day`, `seven_day_opus`,
//! `seven_day_sonnet`, `seven_day_overage_included`, `overage`, `spend_limit`,
//! `utilization`, `resets_at` were seen in the binary. The parser below is
//! deliberately lenient and keeps the raw body so the exact shape can be
//! confirmed against a real response (see `usage-cli --limits --raw`).
//!
//! Token policy: this module NEVER refreshes or writes OAuth tokens. It uses,
//! in order: an explicit token, `CLAUDE_CODE_OAUTH_TOKEN`, or the access token
//! in `~/.claude/.credentials.json` while it is unexpired.

use std::{fs, path::Path, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// Claude Code sends this beta header with OAuth bearer tokens (seen in the binary).
pub const OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenSource {
    Explicit,
    Env,
    CredentialsFile,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub value: String,
    pub source: TokenSource,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// 0–100.
    pub used_percentage: f64,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LimitsReading {
    pub fetched_at: DateTime<Utc>,
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
    pub seven_day_opus: Option<Window>,
    pub seven_day_sonnet: Option<Window>,
    pub seven_day_overage_included: Option<Window>,
    pub overage: Option<Window>,
    pub spend_limit: Option<Window>,
    /// Any other top-level windows we did not anticipate, keyed by field name.
    pub other: Vec<(String, Window)>,
    /// Raw response body, for schema confirmation.
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FetchError {
    NoToken,
    TokenExpired,
    Unauthorized(String),
    RateLimited,
    Http(u16, String),
    Network(String),
    BadBody(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::NoToken => write!(f, "no OAuth token available"),
            FetchError::TokenExpired => write!(f, "stored access token has expired"),
            FetchError::Unauthorized(m) => write!(f, "unauthorized: {m}"),
            FetchError::RateLimited => write!(f, "usage endpoint rate limited"),
            FetchError::Http(c, m) => write!(f, "HTTP {c}: {m}"),
            FetchError::Network(m) => write!(f, "network: {m}"),
            FetchError::BadBody(m) => write!(f, "unexpected response: {m}"),
        }
    }
}

/// Resolve a token without ever refreshing one.
pub fn resolve_token(explicit: Option<&str>, credentials_path: Option<&Path>, now: DateTime<Utc>) -> Result<Token, FetchError> {
    if let Some(t) = explicit.map(str::trim).filter(|t| !t.is_empty()) {
        return Ok(Token { value: t.to_string(), source: TokenSource::Explicit, expires_at: None });
    }
    if let Ok(t) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        if !t.trim().is_empty() {
            return Ok(Token { value: t.trim().to_string(), source: TokenSource::Env, expires_at: None });
        }
    }
    let path = credentials_path.ok_or(FetchError::NoToken)?;
    let text = fs::read_to_string(path).map_err(|_| FetchError::NoToken)?;
    token_from_credentials_json(&text, now)
}

/// Parse `~/.claude/.credentials.json` (`claudeAiOauth.accessToken` / `expiresAt` in ms).
pub fn token_from_credentials_json(text: &str, now: DateTime<Utc>) -> Result<Token, FetchError> {
    let v: Value = serde_json::from_str(text).map_err(|_| FetchError::NoToken)?;
    let o = &v["claudeAiOauth"];
    let access = o["accessToken"].as_str().ok_or(FetchError::NoToken)?;
    let expires_at = o["expiresAt"].as_i64().and_then(|ms| DateTime::from_timestamp_millis(ms));
    if let Some(e) = expires_at {
        if now >= e {
            return Err(FetchError::TokenExpired);
        }
    }
    Ok(Token { value: access.to_string(), source: TokenSource::CredentialsFile, expires_at })
}

/// One GET to the usage endpoint. Blocking; call from a worker thread.
pub fn fetch(token: &Token, timeout: Duration) -> Result<LimitsReading, FetchError> {
    // Windows certificate store via schannel; no bundled root list to go stale.
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .tls_config(tls)
        .build()
        .new_agent();
    let resp = agent
        .get(USAGE_URL)
        .header("Authorization", &format!("Bearer {}", token.value))
        .header("anthropic-beta", OAUTH_BETA_HEADER)
        .header("Accept", "application/json")
        .header("User-Agent", "claude-usage-panel/0.1")
        .call()
        .map_err(|e| FetchError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let body = resp
        .into_body()
        .read_to_string()
        .map_err(|e| FetchError::Network(e.to_string()))?;
    match status {
        200 => parse_body(&body, Utc::now()),
        401 => Err(if body.contains("expired") { FetchError::TokenExpired } else { FetchError::Unauthorized(short(&body)) }),
        429 => Err(FetchError::RateLimited),
        c => Err(FetchError::Http(c, short(&body))),
    }
}

fn short(s: &str) -> String {
    s.chars().take(200).collect()
}

/// Lenient parse of the response body.
pub fn parse_body(body: &str, fetched_at: DateTime<Utc>) -> Result<LimitsReading, FetchError> {
    let v: Value = serde_json::from_str(body).map_err(|e| FetchError::BadBody(e.to_string()))?;
    let obj = v.as_object().ok_or_else(|| FetchError::BadBody("top level is not an object".into()))?;
    let mut r = LimitsReading { fetched_at, raw: body.to_string(), ..Default::default() };
    for (k, val) in obj {
        let w = match parse_window(val) {
            Some(w) => w,
            None => continue,
        };
        match k.as_str() {
            "five_hour" => r.five_hour = Some(w),
            "seven_day" => r.seven_day = Some(w),
            "seven_day_opus" => r.seven_day_opus = Some(w),
            "seven_day_sonnet" => r.seven_day_sonnet = Some(w),
            "seven_day_overage_included" => r.seven_day_overage_included = Some(w),
            "overage" => r.overage = Some(w),
            "spend_limit" => r.spend_limit = Some(w),
            other => r.other.push((other.to_string(), w)),
        }
    }
    if r.five_hour.is_none() && r.seven_day.is_none() && r.other.is_empty() {
        return Err(FetchError::BadBody("no limit windows found".into()));
    }
    Ok(r)
}

fn parse_window(v: &Value) -> Option<Window> {
    let o = v.as_object()?;
    // `utilization` is a 0–1 fraction in Claude Code (it multiplies by 100);
    // `percent` / `used_percentage` are already 0–100.
    let used = if let Some(u) = o.get("utilization").and_then(Value::as_f64) {
        u * 100.0
    } else if let Some(p) = o.get("percent").or_else(|| o.get("used_percentage")).and_then(Value::as_f64) {
        p
    } else {
        return None;
    };
    let resets_at = o.get("resets_at").and_then(parse_time);
    Some(Window { used_percentage: used, resets_at })
}

fn parse_time(v: &Value) -> Option<DateTime<Utc>> {
    if let Some(n) = v.as_f64() {
        let secs = if n > 1e12 { n / 1000.0 } else { n };
        return DateTime::from_timestamp(secs as i64, 0);
    }
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_token_and_expiry() {
        let text = r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat01-xyz","expiresAt":1789000000000,"subscriptionType":"max"}}"#;
        let before = DateTime::from_timestamp(1788999000, 0).unwrap();
        let t = token_from_credentials_json(text, before).unwrap();
        assert_eq!(t.value, "sk-ant-oat01-xyz");
        assert_eq!(t.source, TokenSource::CredentialsFile);
        let after = DateTime::from_timestamp(1789000001, 0).unwrap();
        assert_eq!(token_from_credentials_json(text, after).unwrap_err(), FetchError::TokenExpired);
        assert_eq!(token_from_credentials_json("{}", after).unwrap_err(), FetchError::NoToken);
    }

    #[test]
    fn explicit_token_wins() {
        let t = resolve_token(Some(" abc "), None, Utc::now()).unwrap();
        assert_eq!(t.value, "abc");
        assert_eq!(t.source, TokenSource::Explicit);
    }

    #[test]
    fn parses_fraction_utilization_and_epoch_seconds() {
        let body = r#"{"five_hour":{"utilization":0.235,"resets_at":1789002600},"seven_day":{"utilization":0.412,"resets_at":"2026-09-16T12:00:00Z"},"seven_day_opus":null,"extra_window":{"utilization":0.5,"resets_at":1789002600},"not_a_window":"x"}"#;
        let r = parse_body(body, Utc::now()).unwrap();
        let fh = r.five_hour.unwrap();
        assert!((fh.used_percentage - 23.5).abs() < 1e-9);
        assert_eq!(fh.resets_at.unwrap().timestamp(), 1789002600);
        let sd = r.seven_day.unwrap();
        assert!((sd.used_percentage - 41.2).abs() < 1e-9);
        assert_eq!(sd.resets_at.unwrap().to_rfc3339(), "2026-09-16T12:00:00+00:00");
        assert!(r.seven_day_opus.is_none());
        assert_eq!(r.other.len(), 1);
        assert_eq!(r.other[0].0, "extra_window");
    }

    #[test]
    fn parses_percent_form_and_millis() {
        let body = r#"{"five_hour":{"used_percentage":50,"resets_at":1789002600000}}"#;
        let r = parse_body(body, Utc::now()).unwrap();
        let fh = r.five_hour.unwrap();
        assert_eq!(fh.used_percentage, 50.0);
        assert_eq!(fh.resets_at.unwrap().timestamp(), 1789002600);
    }

    #[test]
    fn rejects_bodies_without_windows() {
        assert!(matches!(parse_body(r#"{"ok":true}"#, Utc::now()), Err(FetchError::BadBody(_))));
        assert!(matches!(parse_body("nope", Utc::now()), Err(FetchError::BadBody(_))));
    }
}
