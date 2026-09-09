//! Cost estimation at Anthropic list price.
//!
//! The table in `pricing.json` was copied from the official pricing page on the
//! date recorded in its `fetched` field. Figures produced here are ESTIMATES of
//! what the usage would cost at API list price. They are not billing data, and
//! on a subscription plan they are not what the user pays.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::TokenUsage;

const BUILTIN: &str = include_str!("../pricing.json");

/// USD per million tokens.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct ModelPrice {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

/// Fast-mode base rates (USD per million). Cache multipliers apply on top of these
/// per the pricing page: 1.25x / 2x for writes, 0.1x for reads.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct FastPrice {
    pub input: f64,
    pub output: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingTable {
    pub source: String,
    pub fetched: String,
    pub unit: String,
    pub models: BTreeMap<String, ModelPrice>,
    #[serde(default)]
    pub fast_mode: BTreeMap<String, FastPrice>,
}

impl PricingTable {
    pub fn builtin() -> Self {
        serde_json::from_str(BUILTIN).expect("embedded pricing.json is valid")
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Map a transcript model string to a pricing key.
    ///
    /// Strips a `[1m]` context suffix and a trailing `-YYYYMMDD` date
    /// (e.g. `claude-haiku-4-5-20251001` → `claude-haiku-4-5`).
    pub fn normalize(model: &str) -> String {
        let mut m = model.trim();
        if let Some(stripped) = m.strip_suffix("[1m]") {
            m = stripped;
        }
        if let Some(idx) = m.rfind('-') {
            let tail = &m[idx + 1..];
            if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) {
                m = &m[..idx];
            }
        }
        m.to_string()
    }

    pub fn lookup(&self, model: &str) -> Option<&ModelPrice> {
        self.models.get(&Self::normalize(model))
    }

    /// Estimated cost in USD, or `None` when the model is not in the table.
    pub fn cost(&self, model: &str, usage: &TokenUsage, fast: bool) -> Option<f64> {
        let key = Self::normalize(model);
        let price = if fast {
            match self.fast_mode.get(&key) {
                Some(f) => ModelPrice {
                    input: f.input,
                    output: f.output,
                    cache_write_5m: f.input * 1.25,
                    cache_write_1h: f.input * 2.0,
                    cache_read: f.input * 0.1,
                },
                None => *self.models.get(&key)?,
            }
        } else {
            *self.models.get(&key)?
        };
        Some(cost_with(&price, usage))
    }
}

fn cost_with(p: &ModelPrice, u: &TokenUsage) -> f64 {
    // Cache writes: use the TTL breakdown when present; any remainder without a
    // breakdown is priced at the cheaper 5-minute rate.
    let broken_down = u.cache_creation_5m + u.cache_creation_1h;
    let remainder = u.cache_creation.saturating_sub(broken_down);
    let write_5m = u.cache_creation_5m + remainder;
    let write_1h = u.cache_creation_1h;

    (u.input as f64 * p.input
        + u.output as f64 * p.output
        + write_5m as f64 * p.cache_write_5m
        + write_1h as f64 * p.cache_write_1h
        + u.cache_read as f64 * p.cache_read)
        / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn normalize_strips_date_and_context_suffix() {
        assert_eq!(PricingTable::normalize("claude-haiku-4-5-20251001"), "claude-haiku-4-5");
        assert_eq!(PricingTable::normalize("claude-opus-5[1m]"), "claude-opus-5");
        assert_eq!(PricingTable::normalize("claude-opus-4-8"), "claude-opus-4-8");
    }

    #[test]
    fn builtin_table_loads_and_has_expected_models() {
        let t = PricingTable::builtin();
        for m in [
            "claude-fable-5-1",
            "claude-fable-5",
            "claude-opus-5",
            "claude-opus-4-8",
            "claude-sonnet-5",
            "claude-sonnet-4-6",
            "claude-haiku-4-5",
        ] {
            assert!(t.lookup(m).is_some(), "missing {m}");
        }
        assert!(t.source.starts_with("https://"));
    }

    #[test]
    fn cache_read_rates() {
        let t = PricingTable::builtin();
        let u = TokenUsage { cache_read: 1_000_000, ..Default::default() };
        approx(t.cost("claude-opus-5", &u, false).unwrap(), 0.50);
        approx(t.cost("claude-fable-5-1", &u, false).unwrap(), 0.25);
        approx(t.cost("claude-fable-5", &u, false).unwrap(), 1.00);
    }

    #[test]
    fn cache_write_ttl_rates() {
        let t = PricingTable::builtin();
        let u1h = TokenUsage { cache_creation: 1_000_000, cache_creation_1h: 1_000_000, ..Default::default() };
        approx(t.cost("claude-opus-5", &u1h, false).unwrap(), 10.0);
        let u5m = TokenUsage { cache_creation: 1_000_000, cache_creation_5m: 1_000_000, ..Default::default() };
        approx(t.cost("claude-opus-5", &u5m, false).unwrap(), 6.25);
        // No breakdown → priced at the 5m rate.
        let ubare = TokenUsage { cache_creation: 1_000_000, ..Default::default() };
        approx(t.cost("claude-opus-5", &ubare, false).unwrap(), 6.25);
    }

    #[test]
    fn fast_mode_uses_premium_base() {
        let t = PricingTable::builtin();
        let u = TokenUsage { input: 1_000_000, output: 1_000_000, ..Default::default() };
        approx(t.cost("claude-opus-5", &u, true).unwrap(), 60.0);
        approx(t.cost("claude-opus-5", &u, false).unwrap(), 30.0);
        // Model without a fast-mode row falls back to standard pricing.
        approx(t.cost("claude-sonnet-5", &u, true).unwrap(), 12.0);
    }

    #[test]
    fn unknown_model_is_none() {
        let t = PricingTable::builtin();
        assert!(t.cost("claude-unicorn-9", &TokenUsage::default(), false).is_none());
    }
}
