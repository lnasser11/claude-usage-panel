//! Claude Code `statusLine` command.
//!
//! Reads the documented status-line JSON from stdin, prints a short line for the
//! terminal, and — when `rate_limits` is present — writes the interesting fields
//! plus `captured_at` to `~/.claude/usage-snapshot.json` (atomic replace).
//! Sessions without `rate_limits` (API-key auth, before first response) never
//! overwrite a good snapshot.

use std::{fs, io::Read, process};

use chrono::{DateTime, Local, Utc};
use serde_json::{json, Value};

fn main() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        process::exit(0);
    }
    let v: Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(_) => process::exit(0),
    };

    let model = v["model"]["display_name"]
        .as_str()
        .or_else(|| v["model"]["id"].as_str())
        .unwrap_or("?");
    let ctx = v["context_window"]["used_percentage"].as_f64();
    let limits = &v["rate_limits"];
    let five = window(&limits["five_hour"]);
    let seven = window(&limits["seven_day"]);

    let mut parts: Vec<String> = Vec::new();
    if let Some((pct, reset)) = five {
        parts.push(format!(
            "5h {pct:.0}% (resets {})",
            reset.with_timezone(&Local).format("%H:%M")
        ));
    }
    if let Some((pct, reset)) = seven {
        parts.push(format!(
            "7d {pct:.0}% (resets {})",
            reset.with_timezone(&Local).format("%a %H:%M")
        ));
    }
    if let Some(c) = ctx {
        parts.push(format!("ctx {c:.0}%"));
    }
    parts.push(model.to_string());
    println!("{}", parts.join(" | "));

    if five.is_some() || seven.is_some() {
        if let Some(path) = usage_core::discovery::default_snapshot_path() {
            let out = json!({
                "captured_at": Utc::now().timestamp(),
                "session_id": v["session_id"],
                "model": v["model"],
                "context_window": v["context_window"],
                "rate_limits": limits,
                "cost": v["cost"],
                "version": v["version"],
            });
            let tmp = path.with_extension(format!("json.tmp{}", process::id()));
            if fs::write(&tmp, out.to_string()).is_ok() {
                let _ = fs::rename(&tmp, &path);
            }
        }
    }
}

fn window(w: &Value) -> Option<(f64, DateTime<Utc>)> {
    let pct = w["used_percentage"].as_f64()?;
    let reset = DateTime::from_timestamp(w["resets_at"].as_i64()?, 0)?;
    Some((pct, reset))
}
