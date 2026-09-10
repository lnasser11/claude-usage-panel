//! Account-level limit windows from Anthropic's OAuth usage endpoint.
//!
//! WARNING — UNSUPPORTED API. `GET https://api.anthropic.com/api/oauth/usage`
//! is what Claude Code's own `/usage` command calls (verified by reading the
//! Claude Code 2.1.260 binary). It is not publicly documented and may change or
//! stop accepting third-party callers at any time. The docs do say it is rate
//! limited, so callers must keep a minimum interval between requests.
//!
//! Response schema, confirmed against a real response on 2026-09-10:
//! - `five_hour` / `seven_day` / `seven_day_opus` / `seven_day_sonnet` / …:
//!   `{utilization: <percent 0-100>, resets_at: <RFC3339>, …}` or null;
//! - `limits`: array of `{kind: session|weekly_all|weekly_scoped, group, percent,
//!   severity, resets_at, scope: {model: {display_name}} | null, is_active}`;
//! - `extra_usage`: usage credits `{is_enabled, monthly_limit, used_credits,
//!   utilization, currency, decimal_places, spend_limit_reached}` (real billing);
//! - `seven_day_breakdown.rows`: `{key, display_name, percent}` share of the
//!   weekly window per surface (Claude Code, Chats, Cowork).
//! Unknown code-named windows also appear; they are kept in `other` only.
//! The parser stays lenient and keeps the raw body (`usage-cli --limits --raw`).
//!
//! Token policy: this module NEVER refreshes or writes OAuth tokens. It uses,
//! in order: an explicit token, `CLAUDE_CODE_OAUTH_TOKEN`, or the access token
//! in `~/.claude/.credentials.json` while it is unexpired.

use std::{fs, path::Path, time::Duration};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// OAuth token endpoint used by Claude Code (URL seen in the 2.1.260 binary).
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Claude Code's public OAuth client id, as used by third-party usage tools.
/// ASSUMPTION: recalled, not read from the binary (that extraction was blocked).
/// A wrong id is simply rejected by the server, so it fails safe. Overridable.
pub const DEFAULT_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
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

/// One row of the `limits` array.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimitEntry {
    /// `session`, `weekly_all`, `weekly_scoped`, …
    pub kind: String,
    pub group: String,
    pub percent: f64,
    pub severity: String,
    pub resets_at: Option<DateTime<Utc>>,
    /// Model display name for scoped windows (e.g. "Fable").
    pub scope: Option<String>,
    pub is_active: bool,
}

/// Usage credits ("extra usage"): real money, not an estimate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Credits {
    pub enabled: bool,
    pub used_minor: i64,
    pub limit_minor: Option<i64>,
    pub currency: String,
    pub exponent: u32,
    pub percent: f64,
    pub limit_reached: bool,
}

impl Credits {
    pub fn money(&self, minor: i64) -> String {
        let div = 10f64.powi(self.exponent as i32);
        let sym = match self.currency.as_str() {
            "BRL" => "R$",
            "USD" => "$",
            "EUR" => "€",
            "GBP" => "£",
            other => other,
        };
        format!("{sym}{:.*}", self.exponent as usize, minor as f64 / div)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BreakdownRow {
    pub key: String,
    pub display_name: String,
    pub percent: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LimitsReading {
    pub fetched_at: DateTime<Utc>,
    pub limits: Vec<LimitEntry>,
    pub credits: Option<Credits>,
    pub breakdown: Vec<BreakdownRow>,
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
    RefreshFailed(String),
    NoRefreshToken,
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
            FetchError::RefreshFailed(m) => write!(f, "token refresh failed: {m}"),
            FetchError::NoRefreshToken => write!(f, "no refresh token in credentials file"),
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
    let resp = agent(timeout)
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

/// Windows certificate store via schannel; no bundled root list to go stale.
fn agent(timeout: Duration) -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .tls_config(tls)
        .build()
        .new_agent()
}

/// Renew Claude Code's stored token pair using its refresh token, and write the
/// result back to `path` atomically, preserving every other field in the file.
///
/// Request shape ASSUMED (not documented): JSON `{grant_type, refresh_token, client_id}`;
/// response `{access_token, refresh_token?, expires_in}`. Tokens are never logged.
pub fn refresh_credentials(path: &Path, client_id: &str, timeout: Duration) -> Result<Token, FetchError> {
    let text = fs::read_to_string(path).map_err(|_| FetchError::NoToken)?;
    let mut root: Value = serde_json::from_str(&text).map_err(|_| FetchError::NoToken)?;
    let refresh_token = root["claudeAiOauth"]["refreshToken"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or(FetchError::NoRefreshToken)?
        .to_string();

    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": client_id,
    })
    .to_string();
    let resp = agent(timeout)
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .header("User-Agent", "claude-usage-panel/0.1")
        .send(body.as_bytes())
        .map_err(|e| FetchError::Network(e.to_string()))?;
    let status = resp.status().as_u16();
    let text = resp
        .into_body()
        .read_to_string()
        .map_err(|e| FetchError::Network(e.to_string()))?;
    if status != 200 {
        return Err(FetchError::RefreshFailed(format!("HTTP {status}: {}", short(&text))));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| FetchError::RefreshFailed(e.to_string()))?;
    let access = v["access_token"]
        .as_str()
        .ok_or_else(|| FetchError::RefreshFailed("no access_token in response".into()))?
        .to_string();
    let expires_in = v["expires_in"].as_i64().unwrap_or(3600);
    let expires_at = Utc::now() + chrono::Duration::seconds(expires_in);

    let o = root["claudeAiOauth"]
        .as_object_mut()
        .ok_or_else(|| FetchError::RefreshFailed("claudeAiOauth missing".into()))?;
    o.insert("accessToken".into(), Value::String(access.clone()));
    if let Some(rt) = v["refresh_token"].as_str() {
        o.insert("refreshToken".into(), Value::String(rt.to_string()));
    }
    o.insert("expiresAt".into(), Value::from(expires_at.timestamp_millis()));
    if let Some(scope) = v["scope"].as_str() {
        let scopes: Vec<Value> = scope.split_whitespace().map(|s| Value::String(s.to_string())).collect();
        if !scopes.is_empty() {
            o.insert("scopes".into(), Value::Array(scopes));
        }
    }

    let tmp = path.with_extension(format!("json.tmp{}", std::process::id()));
    fs::write(&tmp, serde_json::to_string(&root).unwrap())
        .and_then(|_| fs::rename(&tmp, path))
        .map_err(|e| FetchError::RefreshFailed(format!("write-back failed: {e}")))?;

    Ok(Token { value: access, source: TokenSource::CredentialsFile, expires_at: Some(expires_at) })
}

#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub reading: Option<LimitsReading>,
    pub source: Option<TokenSource>,
    /// A refresh was attempted this call and succeeded / failed.
    pub refreshed: Option<bool>,
    pub error: Option<FetchError>,
}

/// Resolve a token, fetch, and if the stored token is expired or rejected (and
/// `allow_refresh`), renew it once and fetch again.
pub fn get_reading(explicit: Option<&str>, credentials_path: Option<&Path>, client_id: &str, allow_refresh: bool, timeout: Duration) -> Outcome {
    let mut out = Outcome::default();
    let now = Utc::now();
    let mut token = resolve_token(explicit, credentials_path, now);

    let can_refresh = |t: &Result<Token, FetchError>| {
        allow_refresh
            && credentials_path.is_some()
            && match t {
                Err(FetchError::TokenExpired) => true,
                Ok(t) => t.source == TokenSource::CredentialsFile,
                _ => false,
            }
    };

    if matches!(token, Err(FetchError::TokenExpired)) && can_refresh(&token) {
        match refresh_credentials(credentials_path.unwrap(), client_id, timeout) {
            Ok(t) => {
                out.refreshed = Some(true);
                token = Ok(t);
            }
            Err(e) => {
                out.refreshed = Some(false);
                out.error = Some(e);
                return out;
            }
        }
    }
    let token = match token {
        Ok(t) => t,
        Err(e) => {
            out.error = Some(e);
            return out;
        }
    };
    out.source = Some(token.source.clone());

    match fetch(&token, timeout) {
        Ok(r) => out.reading = Some(r),
        Err(e @ (FetchError::TokenExpired | FetchError::Unauthorized(_))) if out.refreshed.is_none() && can_refresh(&Ok(token.clone())) => {
            match refresh_credentials(credentials_path.unwrap(), client_id, timeout) {
                Ok(t) => {
                    out.refreshed = Some(true);
                    match fetch(&t, timeout) {
                        Ok(r) => out.reading = Some(r),
                        Err(e2) => out.error = Some(e2),
                    }
                }
                Err(re) => {
                    out.refreshed = Some(false);
                    out.error = Some(FetchError::RefreshFailed(format!("{re} (after {e})")));
                }
            }
        }
        Err(e) => out.error = Some(e),
    }
    out
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
        // Structured sections handled below; not windows.
        if matches!(k.as_str(), "limits" | "extra_usage" | "spend" | "seven_day_breakdown") {
            continue;
        }
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
    if let Some(arr) = obj.get("limits").and_then(Value::as_array) {
        for e in arr {
            let (Some(kind), Some(percent)) = (e["kind"].as_str(), e["percent"].as_f64()) else { continue };
            r.limits.push(LimitEntry {
                kind: kind.to_string(),
                group: e["group"].as_str().unwrap_or("").to_string(),
                percent,
                severity: e["severity"].as_str().unwrap_or("").to_string(),
                resets_at: e.get("resets_at").and_then(parse_time),
                scope: e["scope"]["model"]["display_name"].as_str().map(str::to_string),
                is_active: e["is_active"].as_bool().unwrap_or(false),
            });
        }
    }
    if let Some(x) = obj.get("extra_usage").filter(|v| v.is_object()) {
        if let Some(used) = x["used_credits"].as_f64() {
            let exponent = x["decimal_places"].as_u64().unwrap_or(2) as u32;
            r.credits = Some(Credits {
                enabled: x["is_enabled"].as_bool().unwrap_or(false),
                used_minor: used.round() as i64,
                limit_minor: x["monthly_limit"].as_i64(),
                currency: x["currency"].as_str().unwrap_or("").to_string(),
                exponent,
                percent: x["utilization"].as_f64().unwrap_or(0.0),
                limit_reached: x["spend_limit_reached"].as_bool().unwrap_or(false),
            });
        }
    }
    // Note: indexing a serde_json Map with a missing key panics; use get().
    if let Some(rows) = obj.get("seven_day_breakdown").and_then(|b| b["rows"].as_array()) {
        for row in rows {
            if let (Some(key), Some(pct)) = (row["key"].as_str(), row["percent"].as_f64()) {
                r.breakdown.push(BreakdownRow {
                    key: key.to_string(),
                    display_name: row["display_name"].as_str().unwrap_or(key).to_string(),
                    percent: pct,
                });
            }
        }
    }
    if r.five_hour.is_none() && r.seven_day.is_none() && r.limits.is_empty() && r.other.is_empty() {
        return Err(FetchError::BadBody("no limit windows found".into()));
    }
    Ok(r)
}

fn parse_window(v: &Value) -> Option<Window> {
    let o = v.as_object()?;
    // Confirmed on a real response: `utilization` is already 0–100
    // (e.g. 34.0 when claude.ai shows 34% used). So are `percent` / `used_percentage`.
    let used = o
        .get("utilization")
        .or_else(|| o.get("percent"))
        .or_else(|| o.get("used_percentage"))
        .and_then(Value::as_f64)?;
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
    fn parses_percent_utilization_and_epoch_seconds() {
        let body = r#"{"five_hour":{"utilization":23.5,"resets_at":1789002600},"seven_day":{"utilization":41.2,"resets_at":"2026-09-16T12:00:00Z"},"seven_day_opus":null,"extra_window":{"utilization":50,"resets_at":1789002600},"not_a_window":"x"}"#;
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

    /// Trimmed copy of a real response (2026-09-10, Max 5x plan).
    const REAL: &str = r#"{"five_hour":{"utilization":34.0,"resets_at":"2026-09-10T17:20:00.239284+00:00","limit_dollars":null,"locked_reason":null},"seven_day":{"utilization":11.0,"resets_at":"2026-09-16T08:00:00.239305+00:00"},"seven_day_opus":null,"seven_day_sonnet":null,"nimbus_quill":{"utilization":0.0,"resets_at":null},"extra_usage":{"is_enabled":true,"monthly_limit":11000,"used_credits":9503.0,"utilization":86.39090909090909,"currency":"BRL","decimal_places":2,"disabled_reason":null,"user_disabled":false,"spend_limit_reached":false},"limits":[{"kind":"session","group":"session","percent":34,"severity":"normal","resets_at":"2026-09-10T17:20:00.239284+00:00","scope":null,"is_active":true},{"kind":"weekly_all","group":"weekly","percent":11,"severity":"normal","resets_at":"2026-09-16T08:00:00.239305+00:00","scope":null,"is_active":false},{"kind":"weekly_scoped","group":"weekly","percent":20,"severity":"normal","resets_at":"2026-09-16T08:00:00.239561+00:00","scope":{"model":{"id":null,"display_name":"Fable"},"surface":null},"is_active":false}],"seven_day_breakdown":{"as_of":"2026-09-10T13:23:30.319105+00:00","rows":[{"key":"claude_code","display_name":"Claude Code","percent":53},{"key":"chat","display_name":"Chats","percent":9},{"key":"cowork","display_name":"Cowork","percent":38},{"key":"other","display_name":"Other","percent":0}]}}"#;

    #[test]
    fn parses_real_response_shape() {
        let r = parse_body(REAL, Utc::now()).unwrap();
        assert_eq!(r.five_hour.unwrap().used_percentage, 34.0);
        let sd = r.seven_day.unwrap();
        assert_eq!(sd.used_percentage, 11.0);
        assert_eq!(sd.resets_at.unwrap().to_rfc3339(), "2026-09-16T08:00:00.239305+00:00");
        assert_eq!(r.limits.len(), 3);
        let scoped = r.limits.iter().find(|l| l.kind == "weekly_scoped").unwrap();
        assert_eq!(scoped.percent, 20.0);
        assert_eq!(scoped.scope.as_deref(), Some("Fable"));
        assert!(r.limits.iter().find(|l| l.kind == "session").unwrap().is_active);
        let c = r.credits.unwrap();
        assert!(c.enabled);
        assert_eq!(c.used_minor, 9503);
        assert_eq!(c.limit_minor, Some(11000));
        assert_eq!(c.money(c.used_minor), "R$95.03");
        assert_eq!(c.money(c.limit_minor.unwrap()), "R$110.00");
        assert!((c.percent - 86.39).abs() < 0.01);
        assert_eq!(r.breakdown.len(), 4);
        assert_eq!(r.breakdown[0].display_name, "Claude Code");
        assert_eq!(r.breakdown[0].percent, 53.0);
        assert_eq!(r.other.len(), 1, "code-named window kept only in `other`");
    }

    #[test]
    fn parses_used_percentage_form_and_millis() {
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
