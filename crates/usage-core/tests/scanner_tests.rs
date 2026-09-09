use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use usage_core::{aggregate, PricingTable, Scanner, TokenUsage};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
        .join("projects")
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for e in fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            fs::copy(e.path(), &to).unwrap();
            // fs::copy preserves the source mtime on Windows; make the copy look
            // freshly written so the scanner treats a partial tail as in-progress.
            let f = fs::OpenOptions::new().write(true).open(&to).unwrap();
            f.set_modified(SystemTime::now()).unwrap();
        }
    }
}

fn ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}

#[test]
fn empty_projects_dir() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Scanner::new(dir.path());
    let r = s.scan().unwrap();
    assert!(r.root_exists);
    assert_eq!(r.files_seen, 0);
    assert_eq!(s.store.len(), 0);
    let t = aggregate::today(&s.store, &PricingTable::builtin(), &Utc, Utc::now());
    assert_eq!(t.requests, 0);
    approx(t.cost_usd, 0.0);
}

#[test]
fn missing_projects_dir_is_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Scanner::new(dir.path().join("does-not-exist"));
    let r = s.scan().unwrap();
    assert!(!r.root_exists);
    assert_eq!(s.store.len(), 0);
}

#[test]
fn basic_fixture_counts_and_costs() {
    let mut s = Scanner::new(fixture("basic"));
    let r = s.scan().unwrap();
    assert_eq!(r.files_seen, 2, "main transcript + subagent transcript");
    assert_eq!(r.events_added, 4, "msg_A, msg_B, msg_UNKNOWN, msg_C");
    assert_eq!(r.events_updated, 1, "second msg_A record");
    assert_eq!(r.lines_malformed, 1);
    assert_eq!(r.lines_ignored, 6, "3 user + 1 synthetic + 1 system + 1 ai-title");
    assert_eq!(r.partial_lines, 0);
    assert_eq!(s.store.len(), 4);

    let pricing = PricingTable::builtin();
    let t = aggregate::totals(
        &s.store,
        &pricing,
        ts("2026-09-09T00:00:00Z"),
        ts("2026-09-10T00:00:00Z"),
    );
    assert_eq!(t.requests, 4);
    assert_eq!(
        t.usage,
        TokenUsage {
            input: 2 + 2 + 1000 + 10,
            output: 250 + 100 + 1000 + 20,
            cache_creation: 30556 + 800,
            cache_creation_5m: 0,
            cache_creation_1h: 30556 + 800,
            cache_read: 30556 + 5000,
            thinking: 80,
        }
    );
    // opus-5:    2*5 + 250*25 + 30556*10 (1h write)     = 0.31182
    // fable-5-1: 2*10 + 100*50 + 800*20 + 30556*0.25    = 0.028659
    // sonnet-5:  10*2 + 20*10 + 5000*0.2                = 0.00122
    approx(t.cost_usd, 0.31182 + 0.028659 + 0.00122);
    assert_eq!(t.unpriced_tokens, 2000, "unknown model tokens are counted but not priced");
}

#[test]
fn duplicate_message_id_last_record_wins() {
    let mut s = Scanner::new(fixture("basic"));
    s.scan().unwrap();
    let a = s.store.iter().find(|e| e.message_id == "msg_A").unwrap();
    assert_eq!(a.usage.output, 250, "not 3 (first record) and not 253 (sum)");
    assert_eq!(a.usage.thinking, 80);
}

#[test]
fn synthetic_and_malformed_are_excluded() {
    let mut s = Scanner::new(fixture("basic"));
    s.scan().unwrap();
    assert!(s.store.iter().all(|e| e.model != "<synthetic>"));
    assert!(s.store.iter().all(|e| e.message_id != "msg_BROKEN"));
}

#[test]
fn subagent_transcripts_are_discovered_and_flagged() {
    let mut s = Scanner::new(fixture("basic"));
    s.scan().unwrap();
    let c = s.store.iter().find(|e| e.message_id == "msg_C").unwrap();
    assert!(c.is_sidechain);
    assert_eq!(c.session, "sess-1");
    assert_eq!(c.model, "claude-sonnet-5");
}

#[test]
fn by_model_and_by_session() {
    let mut s = Scanner::new(fixture("basic"));
    s.scan().unwrap();
    let p = PricingTable::builtin();
    let (a, b) = (ts("2026-09-09T00:00:00Z"), ts("2026-09-10T00:00:00Z"));
    let models: Vec<String> = aggregate::by_model(&s.store, &p, a, b)
        .into_iter()
        .map(|m| m.model)
        .collect();
    assert_eq!(
        models,
        ["claude-fable-5-1", "claude-opus-5", "claude-sonnet-5", "claude-unicorn-9"]
    );
    let sessions = aggregate::by_session(&s.store, &p, a, b);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session, "sess-1");
    assert_eq!(
        sessions[0].first,
        ts("2026-09-09T12:00:07Z"),
        "timestamp of the last record for msg_A"
    );
    assert_eq!(sessions[0].last, ts("2026-09-09T12:03:00Z"));
    assert_eq!(sessions[0].totals.requests, 4);
}

#[test]
fn unchanged_files_are_not_reread() {
    let mut s = Scanner::new(fixture("basic"));
    let r1 = s.scan().unwrap();
    assert!(r1.bytes_read > 0);
    let r2 = s.scan().unwrap();
    assert_eq!(r2.files_read, 0);
    assert_eq!(r2.bytes_read, 0);
    assert_eq!(r2.events_added, 0);
    assert_eq!(s.store.len(), 4);
}

#[test]
fn truncated_final_line_waits_then_completes() {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&fixture("truncated"), dir.path());
    let file = dir.path().join("p/s.jsonl");

    let mut s = Scanner::new(dir.path());
    let r1 = s.scan().unwrap();
    assert_eq!(r1.events_added, 2);
    assert_eq!(r1.partial_lines, 1);
    assert_eq!(r1.lines_malformed, 0, "an in-progress line is not malformed");
    assert_eq!(s.store.len(), 2);

    // Nothing changed, tail not settled: no read at all.
    let r2 = s.scan().unwrap();
    assert_eq!(r2.bytes_read, 0);

    // The writer finishes the line.
    let rest = br#"put_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
    let mut f = fs::OpenOptions::new().append(true).open(&file).unwrap();
    f.write_all(rest).unwrap();
    f.write_all(b"\n").unwrap();
    drop(f);

    let r3 = s.scan().unwrap();
    assert_eq!(r3.events_added, 1);
    assert_eq!(r3.partial_lines, 0);
    assert_eq!(s.store.len(), 3);
    let t3 = s.store.iter().find(|e| e.message_id == "msg_T3").unwrap();
    assert_eq!(t3.usage.input, 3);
    assert_eq!(t3.usage.output, 3);
}

#[test]
fn settled_tail_that_never_completes_is_malformed_once() {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&fixture("truncated"), dir.path());
    let file = dir.path().join("p/s.jsonl");

    let mut s = Scanner::new(dir.path()).with_settle_after(Duration::from_secs(60));
    let r1 = s.scan().unwrap();
    assert_eq!(r1.partial_lines, 1);

    // Pretend the file has been quiet for two minutes.
    let f = fs::OpenOptions::new().write(true).open(&file).unwrap();
    f.set_modified(SystemTime::now() - Duration::from_secs(120))
        .unwrap();
    drop(f);

    let r2 = s.scan().unwrap();
    assert_eq!(r2.partial_lines, 0);
    assert_eq!(r2.lines_malformed, 1);
    assert_eq!(s.store.len(), 2);

    let r3 = s.scan().unwrap();
    assert_eq!(r3.bytes_read, 0, "tail is consumed, not re-read forever");
}

#[test]
fn shrunk_file_is_reset_and_rescanned() {
    let dir = tempfile::tempdir().unwrap();
    copy_dir(&fixture("basic"), dir.path());
    let file = dir.path().join("proj-a/sess-1.jsonl");

    let mut s = Scanner::new(dir.path());
    s.scan().unwrap();
    assert_eq!(s.store.len(), 4);

    // Keep only the first three lines (user + both msg_A records).
    let content = fs::read_to_string(&file).unwrap();
    let head: String = content.lines().take(3).map(|l| format!("{l}\n")).collect();
    fs::write(&file, head).unwrap();

    let r = s.scan().unwrap();
    assert_eq!(r.files_reset, 1);
    let ids: Vec<&str> = {
        let mut v: Vec<&str> = s.store.iter().map(|e| e.message_id).collect();
        v.sort();
        v
    };
    assert_eq!(
        ids,
        ["msg_A", "msg_C"],
        "events from the rewritten file were dropped; subagent file untouched"
    );
}

#[test]
fn session_spanning_local_midnight_is_split_by_local_day() {
    let mut s = Scanner::new(fixture("midnight"));
    s.scan().unwrap();
    let p = PricingTable::builtin();
    let sao_paulo = FixedOffset::west_opt(3 * 3600).unwrap();
    let d10 = NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();

    let local = aggregate::daily(&s.store, &p, &sao_paulo, d10, 2);
    assert_eq!(local[0].date, NaiveDate::from_ymd_opt(2026, 9, 9).unwrap());
    assert_eq!(local[0].totals.usage.input, 100, "23:50 local on Sep 9");
    assert_eq!(local[1].totals.usage.input, 100, "00:10 local on Sep 10");

    let utc = aggregate::daily(&s.store, &p, &Utc, d10, 2);
    assert_eq!(utc[0].totals.usage.input, 0);
    assert_eq!(utc[1].totals.usage.input, 200, "both at 02:50 / 03:10 UTC on Sep 10");

    // `today` picks the local date from a UTC instant.
    let now = ts("2026-09-10T02:55:00Z"); // 23:55 in Sao Paulo
    assert_eq!(aggregate::today(&s.store, &p, &sao_paulo, now).usage.input, 100);
    assert_eq!(aggregate::today(&s.store, &p, &Utc, now).usage.input, 200);
}

#[test]
fn retention_prunes_old_events() {
    let mut s = Scanner::new(fixture("basic"));
    s.scan().unwrap();
    let removed = s.store.prune_before(ts("2026-09-09T12:01:30Z"));
    assert_eq!(removed, 2, "msg_A and msg_B are older");
    assert_eq!(s.store.len(), 2);
}

#[test]
fn trailing_window_counts_tokens_only() {
    let mut s = Scanner::new(fixture("basic"));
    s.scan().unwrap();
    let p = PricingTable::builtin();
    let now = ts("2026-09-09T12:02:30Z");
    let t = aggregate::trailing(&s.store, &p, now, chrono::Duration::minutes(1));
    assert_eq!(t.requests, 1, "only msg_UNKNOWN at 12:02:00 is within the last minute");
}
