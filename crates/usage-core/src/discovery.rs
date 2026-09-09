//! Locating Claude Code's data directory.
//!
//! Verified: transcripts live at `<config dir>/projects/<project-slug>/<sessionId>.jsonl`
//! with subagent transcripts at `<sessionId>/subagents/agent-*.jsonl`. The default
//! config dir is `~/.claude`.
//!
//! ASSUMPTION (not verified against docs): the `CLAUDE_CONFIG_DIR` environment
//! variable relocates the config dir. It is honoured when set, which is harmless
//! when unset.

use std::path::PathBuf;

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

pub fn claude_config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        if !d.is_empty() {
            return Some(PathBuf::from(d));
        }
    }
    home_dir().map(|h| h.join(".claude"))
}

/// `~/.claude/projects`
pub fn default_projects_dir() -> Option<PathBuf> {
    claude_config_dir().map(|d| d.join("projects"))
}

/// `~/.claude/settings.json` (where the statusLine hook is configured).
pub fn settings_path() -> Option<PathBuf> {
    claude_config_dir().map(|d| d.join("settings.json"))
}

/// `~/.claude/usage-snapshot.json` — written by the statusline hook, read by the panel.
/// This file name is ours, not Claude Code's.
pub fn default_snapshot_path() -> Option<PathBuf> {
    claude_config_dir().map(|d| d.join("usage-snapshot.json"))
}
