use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Token counts from one API response's `message.usage` object.
///
/// `output` already includes thinking tokens; `thinking` is only the
/// `output_tokens_details.thinking_tokens` detail when present.
/// `cache_creation` is the total cache write; the `_5m` / `_1h` fields are the
/// breakdown from `usage.cache_creation` when present.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_creation_5m: u64,
    pub cache_creation_1h: u64,
    pub cache_read: u64,
    pub thinking: u64,
}

impl TokenUsage {
    /// All tokens that were processed: fresh input + cache writes + cache reads + output.
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_creation + self.cache_read
    }

    pub fn add(&mut self, other: &TokenUsage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_creation += other.cache_creation;
        self.cache_creation_5m += other.cache_creation_5m;
        self.cache_creation_1h += other.cache_creation_1h;
        self.cache_read += other.cache_read;
        self.thinking += other.thinking;
    }
}

/// One API response, extracted from an `assistant` transcript record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageEvent {
    /// `message.id` (the API `msg_…` id). Several transcript records share one id.
    pub message_id: String,
    pub session_id: String,
    pub timestamp: DateTime<Utc>,
    /// Raw `message.model` string as written by Claude Code.
    pub model: String,
    pub usage: TokenUsage,
    /// `isSidechain` — true for subagent traffic.
    pub is_sidechain: bool,
    /// `usage.speed == "fast"` (fast mode is priced differently).
    pub fast: bool,
}
