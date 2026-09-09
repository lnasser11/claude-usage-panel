//! De-duplicated event store.
//!
//! Claude Code writes one `assistant` record per content block, all sharing the
//! same `message.id`, and the usage on later records is monotonically greater
//! (verified on real data: 4331 records → 2084 distinct ids, 60 with growing
//! usage). The rule is therefore: last record for a message id wins.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::model::{TokenUsage, UsageEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Added,
    Updated,
}

#[derive(Debug, Clone)]
struct Stored {
    ts: DateTime<Utc>,
    model: u32,
    session: u32,
    file: u32,
    usage: TokenUsage,
    sidechain: bool,
    fast: bool,
}

/// A borrowed view of one stored event.
#[derive(Debug, Clone, Copy)]
pub struct EventRef<'a> {
    pub message_id: &'a str,
    pub timestamp: DateTime<Utc>,
    pub model: &'a str,
    pub session: &'a str,
    pub usage: TokenUsage,
    pub is_sidechain: bool,
    pub fast: bool,
}

#[derive(Default, Debug)]
struct Interner {
    names: Vec<String>,
    ids: HashMap<String, u32>,
}

impl Interner {
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.ids.get(s) {
            return id;
        }
        let id = self.names.len() as u32;
        self.names.push(s.to_owned());
        self.ids.insert(s.to_owned(), id);
        id
    }
    fn name(&self, id: u32) -> &str {
        &self.names[id as usize]
    }
}

#[derive(Default, Debug)]
pub struct UsageStore {
    events: HashMap<String, Stored>,
    models: Interner,
    sessions: Interner,
}

impl UsageStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace the event for `ev.message_id`.
    pub fn upsert(&mut self, ev: UsageEvent, file: u32) -> Upsert {
        let model = self.models.intern(&ev.model);
        let session = self.sessions.intern(&ev.session_id);
        let stored = Stored {
            ts: ev.timestamp,
            model,
            session,
            file,
            usage: ev.usage,
            sidechain: ev.is_sidechain,
            fast: ev.fast,
        };
        match self.events.insert(ev.message_id, stored) {
            Some(_) => Upsert::Updated,
            None => Upsert::Added,
        }
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = EventRef<'_>> + '_ {
        self.events.iter().map(move |(id, s)| EventRef {
            message_id: id,
            timestamp: s.ts,
            model: self.models.name(s.model),
            session: self.sessions.name(s.session),
            usage: s.usage,
            is_sidechain: s.sidechain,
            fast: s.fast,
        })
    }

    /// Events with `start <= timestamp < end`.
    pub fn between(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> impl Iterator<Item = EventRef<'_>> + '_ {
        self.iter()
            .filter(move |e| e.timestamp >= start && e.timestamp < end)
    }

    /// Drop every event that came from `file` (used when a file shrinks or is rewritten).
    pub fn remove_file(&mut self, file: u32) -> usize {
        let before = self.events.len();
        self.events.retain(|_, s| s.file != file);
        before - self.events.len()
    }

    /// Drop events older than `cutoff`.
    pub fn prune_before(&mut self, cutoff: DateTime<Utc>) -> usize {
        let before = self.events.len();
        self.events.retain(|_, s| s.ts >= cutoff);
        before - self.events.len()
    }

    pub fn earliest(&self) -> Option<DateTime<Utc>> {
        self.events.values().map(|s| s.ts).min()
    }

    pub fn latest(&self) -> Option<DateTime<Utc>> {
        self.events.values().map(|s| s.ts).max()
    }
}
