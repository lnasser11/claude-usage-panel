//! Core logic for the Claude usage panel.
//!
//! This crate has no UI and no Windows-specific dependency. It:
//! - discovers the Claude Code projects directory ([`discovery`]),
//! - incrementally scans transcript JSONL files ([`scanner`]),
//! - de-duplicates streamed assistant records by API message id ([`store`]),
//! - aggregates tokens by day / session / model and estimates cost ([`aggregate`], [`pricing`]),
//! - reads the rate-limit snapshot written by the statusline hook ([`snapshot`]).
//!
//! Cost figures are estimates at Anthropic list price, not billing data.

pub mod aggregate;
pub mod discovery;
pub mod model;
pub mod parse;
pub mod pricing;
pub mod scanner;
pub mod snapshot;
pub mod store;

pub use aggregate::{DayTotals, ModelTotals, SessionTotals, Totals};
pub use model::{TokenUsage, UsageEvent};
pub use pricing::PricingTable;
pub use scanner::{ScanReport, Scanner};
pub use snapshot::{LimitSnapshot, LimitWindow, WindowState};
pub use store::UsageStore;
