//! Lenient parsing of one transcript line.
//!
//! Field names below were verified against real transcripts on 2026-09-09
//! (Claude Code 2.1.205 – 2.1.260). Unknown fields are ignored.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::model::{TokenUsage, UsageEvent};

#[derive(Debug, PartialEq)]
pub enum ParsedLine {
    /// An assistant record carrying usage.
    Usage(UsageEvent),
    /// Valid JSON, but not a usage-bearing assistant record (user, system, summary,
    /// synthetic error message, …).
    Ignored,
    /// Not valid JSON (truncated write, corruption).
    Malformed,
}

#[derive(Deserialize)]
struct RawRecord {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    #[serde(rename = "isSidechain")]
    is_sidechain: Option<bool>,
    #[serde(rename = "isApiErrorMessage")]
    is_api_error_message: Option<bool>,
    message: Option<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct RawUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation: Option<RawCacheCreation>,
    output_tokens_details: Option<RawOutputDetails>,
    speed: Option<String>,
}

#[derive(Deserialize)]
struct RawCacheCreation {
    ephemeral_5m_input_tokens: Option<u64>,
    ephemeral_1h_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct RawOutputDetails {
    thinking_tokens: Option<u64>,
}

/// Parse one line. Never panics; never fails on unexpected shapes.
pub fn parse_line(line: &str) -> ParsedLine {
    let rec: RawRecord = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(_) => {
            // Distinguish "valid JSON of a shape we don't model" from garbage.
            return if serde_json::from_str::<serde::de::IgnoredAny>(line).is_ok() {
                ParsedLine::Ignored
            } else {
                ParsedLine::Malformed
            };
        }
    };

    if rec.kind.as_deref() != Some("assistant") {
        return ParsedLine::Ignored;
    }
    if rec.is_api_error_message == Some(true) {
        return ParsedLine::Ignored;
    }
    let msg = match rec.message {
        Some(m) => m,
        None => return ParsedLine::Ignored,
    };
    let model = match msg.model {
        Some(m) if m != "<synthetic>" => m,
        _ => return ParsedLine::Ignored,
    };
    let (id, usage) = match (msg.id, msg.usage) {
        (Some(id), Some(u)) => (id, u),
        _ => return ParsedLine::Ignored,
    };
    let timestamp = match rec
        .timestamp
        .as_deref()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
    {
        Some(t) => t.with_timezone(&Utc),
        None => return ParsedLine::Ignored,
    };

    let (c5, c1) = usage
        .cache_creation
        .map(|c| {
            (
                c.ephemeral_5m_input_tokens.unwrap_or(0),
                c.ephemeral_1h_input_tokens.unwrap_or(0),
            )
        })
        .unwrap_or((0, 0));

    ParsedLine::Usage(UsageEvent {
        message_id: id,
        session_id: rec.session_id.unwrap_or_default(),
        timestamp,
        model,
        usage: TokenUsage {
            input: usage.input_tokens.unwrap_or(0),
            output: usage.output_tokens.unwrap_or(0),
            cache_creation: usage.cache_creation_input_tokens.unwrap_or(0),
            cache_creation_5m: c5,
            cache_creation_1h: c1,
            cache_read: usage.cache_read_input_tokens.unwrap_or(0),
            thinking: usage
                .output_tokens_details
                .and_then(|d| d.thinking_tokens)
                .unwrap_or(0),
        },
        is_sidechain: rec.is_sidechain.unwrap_or(false),
        fast: usage.speed.as_deref() == Some("fast"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_record_is_ignored() {
        let l = r#"{"type":"user","message":{"role":"user","content":"hi"}}"#;
        assert_eq!(parse_line(l), ParsedLine::Ignored);
    }

    #[test]
    fn garbage_is_malformed() {
        assert_eq!(parse_line(r#"{"type":"assistant","message":{"#), ParsedLine::Malformed);
        assert_eq!(parse_line("not json"), ParsedLine::Malformed);
    }

    #[test]
    fn message_as_string_is_ignored_not_malformed() {
        let l = r#"{"type":"assistant","message":"weird"}"#;
        assert_eq!(parse_line(l), ParsedLine::Ignored);
    }

    #[test]
    fn synthetic_is_ignored() {
        let l = r#"{"type":"assistant","timestamp":"2026-08-26T23:23:12.900Z","sessionId":"s","isApiErrorMessage":true,"message":{"id":"x","model":"<synthetic>","usage":{"input_tokens":0,"output_tokens":0}}}"#;
        assert_eq!(parse_line(l), ParsedLine::Ignored);
    }

    #[test]
    fn full_assistant_record() {
        let l = r#"{"type":"assistant","timestamp":"2026-08-26T23:24:03.549Z","sessionId":"sess","isSidechain":false,"message":{"id":"msg_1","model":"claude-opus-5","usage":{"input_tokens":2,"cache_creation_input_tokens":800,"cache_read_input_tokens":30556,"output_tokens":264,"output_tokens_details":{"thinking_tokens":22},"cache_creation":{"ephemeral_1h_input_tokens":800,"ephemeral_5m_input_tokens":0},"speed":"standard"}}}"#;
        match parse_line(l) {
            ParsedLine::Usage(ev) => {
                assert_eq!(ev.message_id, "msg_1");
                assert_eq!(ev.model, "claude-opus-5");
                assert_eq!(ev.usage.input, 2);
                assert_eq!(ev.usage.output, 264);
                assert_eq!(ev.usage.cache_creation, 800);
                assert_eq!(ev.usage.cache_creation_1h, 800);
                assert_eq!(ev.usage.cache_read, 30556);
                assert_eq!(ev.usage.thinking, 22);
                assert!(!ev.fast);
                assert!(!ev.is_sidechain);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
