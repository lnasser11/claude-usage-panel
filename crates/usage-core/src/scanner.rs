//! Incremental JSONL scanner.
//!
//! Tracks (path → len, mtime, consumed byte offset). A re-scan only reads bytes
//! appended since the last consumed newline, so it is cheap to run often.
//! Partially written trailing lines are left unconsumed until the next scan,
//! unless the file has been quiet for `settle_after`, in which case the tail is
//! parsed as-is (and counted as malformed if it never completed).

use std::{
    collections::HashMap,
    fs, io,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Utc};

use crate::{
    parse::{parse_line, ParsedLine},
    store::{Upsert, UsageStore},
};

#[derive(Debug, Clone)]
struct FileState {
    id: u32,
    len: u64,
    mtime: SystemTime,
    /// Bytes consumed so far (always ends right after a newline, or at EOF once settled).
    offset: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub root_exists: bool,
    pub files_seen: usize,
    pub files_read: usize,
    pub files_skipped_old: usize,
    /// Files that shrank (rewritten/truncated) and were re-read from the start.
    pub files_reset: usize,
    pub bytes_read: u64,
    pub events_added: usize,
    pub events_updated: usize,
    pub lines_ignored: usize,
    pub lines_malformed: usize,
    /// Trailing lines without a newline that were left for the next scan.
    pub partial_lines: usize,
    pub events_pruned: usize,
}

pub struct Scanner {
    root: PathBuf,
    files: HashMap<PathBuf, FileState>,
    next_file_id: u32,
    pub store: UsageStore,
    retain: Option<chrono::Duration>,
    settle_after: Duration,
}

impl Scanner {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Scanner {
            root: root.into(),
            files: HashMap::new(),
            next_file_id: 0,
            store: UsageStore::new(),
            retain: None,
            settle_after: Duration::from_secs(30),
        }
    }

    /// Skip files not modified in the last `days` and prune older events.
    pub fn with_retention_days(mut self, days: i64) -> Self {
        self.retain = Some(chrono::Duration::days(days));
        self
    }

    /// How long a file must be quiet before a newline-less tail is parsed as final.
    pub fn with_settle_after(mut self, d: Duration) -> Self {
        self.settle_after = d;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn tracked_files(&self) -> usize {
        self.files.len()
    }

    pub fn scan(&mut self) -> io::Result<ScanReport> {
        self.scan_with_progress(|_, _| {})
    }

    /// `progress(done, total)` is called before each file and once at the end.
    pub fn scan_with_progress(
        &mut self,
        mut progress: impl FnMut(usize, usize),
    ) -> io::Result<ScanReport> {
        let mut report = ScanReport::default();
        if !self.root.is_dir() {
            return Ok(report);
        }
        report.root_exists = true;

        let mut paths = Vec::new();
        discover(&self.root, &mut paths)?;
        paths.sort();
        report.files_seen = paths.len();

        let now = SystemTime::now();
        let cutoff = self.retain.map(|d| Utc::now() - d);

        for (i, path) in paths.iter().enumerate() {
            progress(i, paths.len());
            let meta = match fs::metadata(path) {
                Ok(m) => m,
                Err(_) => continue, // vanished between discovery and read
            };
            let len = meta.len();
            let mtime = meta.modified().unwrap_or(now);
            if let Some(c) = cutoff {
                let mt: DateTime<Utc> = mtime.into();
                if mt < c {
                    report.files_skipped_old += 1;
                    continue;
                }
            }
            let settled = now
                .duration_since(mtime)
                .map(|d| d >= self.settle_after)
                .unwrap_or(false);

            let (id, mut offset, prev_len, prev_mtime) = match self.files.get(path) {
                Some(s) => (s.id, s.offset, s.len, s.mtime),
                None => {
                    let id = self.next_file_id;
                    self.next_file_id += 1;
                    (id, 0, u64::MAX, SystemTime::UNIX_EPOCH)
                }
            };

            if len < offset {
                self.store.remove_file(id);
                offset = 0;
                report.files_reset += 1;
            }
            let changed = len != prev_len || mtime != prev_mtime;
            let pending_tail = offset < len;
            if !(changed || (pending_tail && settled)) {
                continue;
            }

            let mut buf = Vec::new();
            if offset < len {
                let mut f = fs::File::open(path)?;
                f.seek(SeekFrom::Start(offset))?;
                f.read_to_end(&mut buf)?;
            }
            report.files_read += 1;
            report.bytes_read += buf.len() as u64;
            let consumed = process_buffer(&buf, settled, id, &mut self.store, &mut report);
            offset += consumed as u64;

            self.files.insert(
                path.clone(),
                FileState {
                    id,
                    len,
                    mtime,
                    offset,
                },
            );
        }
        progress(paths.len(), paths.len());

        if let Some(c) = cutoff {
            report.events_pruned = self.store.prune_before(c);
        }
        Ok(report)
    }
}

fn discover(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            // Ignore errors in subdirectories (permissions, races).
            let _ = discover(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

/// Returns the number of bytes consumed from `buf`.
fn process_buffer(
    buf: &[u8],
    settled: bool,
    file: u32,
    store: &mut UsageStore,
    report: &mut ScanReport,
) -> usize {
    let mut pos = 0;
    let mut consumed = 0;
    while let Some(nl) = buf[pos..].iter().position(|&b| b == b'\n') {
        handle_line(&buf[pos..pos + nl], file, store, report);
        pos += nl + 1;
        consumed = pos;
    }
    let tail = &buf[consumed..];
    if !tail.is_empty() {
        if settled {
            handle_line(tail, file, store, report);
            consumed = buf.len();
        } else {
            report.partial_lines += 1;
        }
    }
    consumed
}

fn handle_line(line: &[u8], file: u32, store: &mut UsageStore, report: &mut ScanReport) {
    let line = trim_ascii(line);
    if line.is_empty() {
        return;
    }
    let text = match std::str::from_utf8(line) {
        Ok(t) => t,
        Err(_) => {
            report.lines_malformed += 1;
            return;
        }
    };
    match parse_line(text) {
        ParsedLine::Usage(ev) => match store.upsert(ev, file) {
            Upsert::Added => report.events_added += 1,
            Upsert::Updated => report.events_updated += 1,
        },
        ParsedLine::Ignored => report.lines_ignored += 1,
        ParsedLine::Malformed => report.lines_malformed += 1,
    }
}

fn trim_ascii(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if first.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = s {
        if last.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    s
}
