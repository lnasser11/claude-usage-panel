use std::time::Instant;

use chrono::{Duration, Local, Utc};
use usage_core::{aggregate, discovery, LimitSnapshot, PricingTable, Scanner, Totals, WindowState};

fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

fn line(t: &Totals) -> String {
    let u = &t.usage;
    let mut s = format!(
        "{:>8} tok  (in {:>7} | out {:>7} | cache write {:>7} | cache read {:>7})  {:>4} req  est ${:>7.2}",
        fmt_tokens(u.total()),
        fmt_tokens(u.input),
        fmt_tokens(u.output),
        fmt_tokens(u.cache_creation),
        fmt_tokens(u.cache_read),
        t.requests,
        t.cost_usd
    );
    if t.unpriced_tokens > 0 {
        s.push_str(&format!("  (+{} unpriced)", fmt_tokens(t.unpriced_tokens)));
    }
    s
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let days: usize = args
        .iter()
        .position(|a| a == "--days")
        .and_then(|i| args.get(i + 1))
        .and_then(|d| d.parse().ok())
        .unwrap_or(7);
    let json = args.iter().any(|a| a == "--json");
    if args.iter().any(|a| a == "--limits") {
        return limits_mode(args.iter().any(|a| a == "--raw"), !args.iter().any(|a| a == "--no-refresh"));
    }

    let root = discovery::default_projects_dir().expect("home directory");
    let pricing = PricingTable::builtin();
    let mut scanner = Scanner::new(&root);

    let t0 = Instant::now();
    let report = scanner.scan().expect("scan");
    let elapsed = t0.elapsed();
    let t1 = Instant::now();
    let rescan = scanner.scan().expect("rescan");
    let re_elapsed = t1.elapsed();

    let now = Utc::now();
    let today = aggregate::today(&scanner.store, &pricing, &Local, now);
    let end_date = aggregate::local_date(&Local, now);
    let daily = aggregate::daily(&scanner.store, &pricing, &Local, end_date, days);
    let ws = aggregate::day_bounds(&Local, end_date - Duration::days(days as i64 - 1)).0;
    let we = aggregate::day_bounds(&Local, end_date).1;
    let models = aggregate::by_model(&scanner.store, &pricing, ws, we);
    let sessions = aggregate::by_session(&scanner.store, &pricing, ws, we);
    let snapshot = discovery::default_snapshot_path()
        .and_then(|p| LimitSnapshot::read(&p).ok().flatten());

    if json {
        let out = serde_json::json!({
            "scan": {
                "files_seen": report.files_seen,
                "events": scanner.store.len(),
                "malformed": report.lines_malformed,
                "bytes_read": report.bytes_read,
                "ms": elapsed.as_millis(),
            },
            "today": today,
            "daily": daily,
            "by_model": models,
            "sessions": sessions,
            "snapshot": snapshot,
            "pricing_source": pricing.source,
            "pricing_fetched": pricing.fetched,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        return;
    }

    println!("Claude Code usage  -  {}  (local time)", end_date);
    println!("Projects dir: {}", root.display());
    println!(
        "Scan: {} files, {} API responses ({} extra block records collapsed), {} malformed lines, {:.1} MB in {:?}; re-scan {:?} ({} bytes)",
        report.files_seen,
        scanner.store.len(),
        report.events_updated,
        report.lines_malformed,
        report.bytes_read as f64 / 1e6,
        elapsed,
        re_elapsed,
        rescan.bytes_read
    );
    println!();
    println!("Today:      {}", line(&today));
    println!();
    println!("Limit windows (from statusLine snapshot):");
    match snapshot {
        None => println!("  no snapshot yet - run an interactive `claude` session after installing the hook"),
        Some(s) => {
            let age = s.age(now);
            println!(
                "  captured {} min ago (session {}, {})",
                age.num_minutes(),
                s.session_id.as_deref().unwrap_or("?"),
                s.model.as_deref().unwrap_or("?")
            );
            for (name, w) in [("5-hour", s.five_hour), ("7-day", s.seven_day)] {
                match w {
                    None => println!("  {name}: absent"),
                    Some(w) => {
                        let state = match s.state(&w, now, Duration::minutes(15)) {
                            WindowState::Fresh => "fresh",
                            WindowState::Stale => "STALE",
                            WindowState::Expired => "EXPIRED (window reset since capture)",
                        };
                        println!(
                            "  {name}: {:.0}% used, resets {} - {state}",
                            w.used_percentage,
                            w.resets_at.with_timezone(&Local).format("%a %d %b %H:%M")
                        );
                    }
                }
            }
        }
    }
    println!();
    println!("Last {days} days:");
    for d in &daily {
        println!("  {}  {}", d.date, line(&d.totals));
    }
    println!();
    println!("By model (last {days} days):");
    for m in &models {
        println!("  {:<28} {}", m.model, line(&m.totals));
    }
    println!();
    println!("Sessions (last {days} days, most recent first, top 10):");
    for s in sessions.iter().take(10) {
        println!(
            "  {}  {} -> {}  {}",
            &s.session[..8.min(s.session.len())],
            s.first.with_timezone(&Local).format("%m-%d %H:%M"),
            s.last.with_timezone(&Local).format("%m-%d %H:%M"),
            line(&s.totals)
        );
    }
    println!();
    println!(
        "Costs are estimates at Anthropic list price ({}, fetched {}). Not billing data.",
        pricing.source, pricing.fetched
    );
}

/// Fetch the account limit windows once (unsupported endpoint; see usage_core::limits).
fn limits_mode(raw: bool, allow_refresh: bool) {
    use std::time::Duration;
    use usage_core::limits;
    let creds = discovery::claude_config_dir().map(|d| d.join(".credentials.json"));
    let out = limits::get_reading(None, creds.as_deref(), limits::DEFAULT_CLIENT_ID, allow_refresh, Duration::from_secs(10));
    match out.refreshed {
        Some(true) => println!("stored token was expired: refreshed and written back"),
        Some(false) => println!("stored token was expired: refresh FAILED"),
        None => {}
    }
    if let Some(src) = &out.source {
        println!("token source: {src:?}");
    }
    match (out.reading, out.error) {
        (Some(r), _) => {
            for (name, w) in [
                ("five_hour", &r.five_hour),
                ("seven_day", &r.seven_day),
                ("seven_day_opus", &r.seven_day_opus),
                ("seven_day_sonnet", &r.seven_day_sonnet),
                ("seven_day_overage_included", &r.seven_day_overage_included),
                ("overage", &r.overage),
                ("spend_limit", &r.spend_limit),
            ] {
                match w {
                    Some(w) => println!("{name:<28} {:6.1}%  resets {}", w.used_percentage, w.resets_at.map(|t| t.with_timezone(&Local).to_string()).unwrap_or_else(|| "?".into())),
                    None => println!("{name:<28} absent"),
                }
            }
            for (name, w) in &r.other {
                println!("{name:<28} {:6.1}%  (unlisted window)", w.used_percentage);
            }
            println!("--- limits array ---");
            for l in &r.limits {
                println!(
                    "{:<14} {:<8} {:>5.1}%  {:<8} {}{}",
                    l.kind,
                    l.group,
                    l.percent,
                    l.severity,
                    l.scope.as_deref().unwrap_or("-"),
                    if l.is_active { "  (active)" } else { "" }
                );
            }
            if let Some(c) = &r.credits {
                println!(
                    "--- usage credits (billed) --- enabled={} {} of {} ({:.1}%){}",
                    c.enabled,
                    c.money(c.used_minor),
                    c.limit_minor.map(|l| c.money(l)).unwrap_or_else(|| "no limit".into()),
                    c.percent,
                    if c.limit_reached { "  LIMIT REACHED" } else { "" }
                );
            }
            if !r.breakdown.is_empty() {
                println!("--- weekly window by surface ---");
                for b in &r.breakdown {
                    println!("{:<14} {:>3.0}%", b.display_name, b.percent);
                }
            }
            if raw {
                println!("--- raw body ---\n{}", r.raw);
            }
        }
        (None, Some(e)) => {
            eprintln!("fetch failed: {e}");
            std::process::exit(1);
        }
        (None, None) => std::process::exit(1),
    }
}
