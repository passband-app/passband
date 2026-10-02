//! Per-model token prices (USD per million tokens) for the cost columns.
//!
//! A model with no price, built in or given by its candidate, has NO cost:
//! `report` refuses the column rather than print a silent zero.
use serde::{Deserialize, Serialize};
use squelch_core::triage::llm::Usage;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Prices {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
}

/// Unless stated: cache read 0.1x input, cache write 1.25x (5 minute TTL) and
/// 2x (1 hour TTL).
pub const CACHE_READ_MULT: f64 = 0.1;
pub const CACHE_WRITE_5M_MULT: f64 = 1.25;
pub const CACHE_WRITE_1H_MULT: f64 = 2.0;

impl Prices {
    pub fn from_input_output(input: f64, output: f64) -> Self {
        Self {
            input,
            output,
            cache_read: input * CACHE_READ_MULT,
            cache_write_5m: input * CACHE_WRITE_5M_MULT,
            cache_write_1h: input * CACHE_WRITE_1H_MULT,
        }
    }

    /// USD for one call. Cache writes the provider did not split by TTL are
    /// priced at the 1 hour rate, which is what triage requests.
    pub fn cost(&self, usage: &Usage) -> f64 {
        let (w5m, w1h) = usage
            .cache_creation
            .map(|split| {
                (
                    split.ephemeral_5m_input_tokens,
                    split.ephemeral_1h_input_tokens,
                )
            })
            .unwrap_or((0, 0));
        let unsplit = usage.cache_creation_input_tokens.saturating_sub(w5m + w1h);
        (usage.input_tokens as f64 * self.input
            + usage.output_tokens as f64 * self.output
            + usage.cache_read_input_tokens as f64 * self.cache_read
            + w5m as f64 * self.cache_write_5m
            + (w1h + unsplit) as f64 * self.cache_write_1h)
            / 1_000_000.0
    }
}

impl Prices {
    /// USD for one call as production's cache would have served it: the
    /// prefix (this call's cache reads plus writes) priced at
    /// `hit * cache_read + (1 - hit) * cache_write_1h`, everything else as
    /// billed. Removes the cold-cache penalty a short eval run pays.
    pub fn projected_cost(&self, usage: &Usage, hit: f64) -> f64 {
        let prefix = (usage.cache_read_input_tokens + usage.cache_creation_input_tokens) as f64;
        (usage.input_tokens as f64 * self.input
            + usage.output_tokens as f64 * self.output
            + prefix * (hit * self.cache_read + (1.0 - hit) * self.cache_write_1h))
            / 1_000_000.0
    }
}

/// First-party Claude list prices. A gateway-qualified id
/// (`anthropic/claude-...`) prices as its bare id.
pub fn builtin(model: &str) -> Option<Prices> {
    let bare = model.strip_prefix("anthropic/").unwrap_or(model);
    let (input, output, cache_read) = match bare {
        "claude-opus-5-5" => (4.0, 20.0, Some(0.20)),
        "claude-opus-5" => (5.0, 25.0, None),
        "claude-sonnet-5-5" => (2.0, 10.0, Some(0.20)),
        "claude-sonnet-5" => (2.0, 10.0, None),
        "claude-haiku-4-5" => (1.0, 5.0, None),
        _ => return None,
    };
    let mut prices = Prices::from_input_output(input, output);
    if let Some(read) = cache_read {
        prices.cache_read = read;
    }
    Some(prices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use squelch_core::triage::llm::CacheCreationBreakdown;

    #[test]
    fn builtin_table_and_cache_defaults() {
        let opus = builtin("claude-opus-5-5").unwrap();
        assert_eq!(
            (opus.input, opus.output, opus.cache_read),
            (4.0, 20.0, 0.20)
        );
        assert_eq!((opus.cache_write_5m, opus.cache_write_1h), (5.0, 8.0));
        let haiku = builtin("anthropic/claude-haiku-4-5").unwrap();
        assert_eq!(haiku.cache_read, 0.1);
        assert_eq!(builtin("claude-sonnet-5").unwrap().cache_read, 0.2);
        assert!(builtin("gpt-unknown").is_none());
    }

    /// Opus 5.5, 2000 uncached + 6000 prefix + 500 out. At hit 0.9 the
    /// prefix costs 0.9*0.20 + 0.1*8 = 0.98/MTok; at 0.5, 0.10 + 4 = 4.10.
    #[test]
    fn projection_prices_the_prefix_at_the_hit_rate() {
        let prices = builtin("claude-opus-5-5").unwrap();
        let usage = Usage {
            input_tokens: 2_000,
            output_tokens: 500,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 6_000,
            cache_creation: None,
        };
        // 2000*4 + 500*20 + 6000*0.98 = 8000 + 10000 + 5880 = 23880 / 1e6
        assert!((prices.projected_cost(&usage, 0.9) - 0.023_88).abs() < 1e-12);
        // 8000 + 10000 + 6000*4.10 = 42600 / 1e6
        assert!((prices.projected_cost(&usage, 0.5) - 0.042_6).abs() < 1e-12);
        // Where the prefix came from does not matter, only its size.
        let warm = Usage {
            cache_read_input_tokens: 6_000,
            cache_creation_input_tokens: 0,
            ..usage
        };
        assert_eq!(
            prices.projected_cost(&warm, 0.9),
            prices.projected_cost(&usage, 0.9)
        );
    }

    #[test]
    fn cost_prices_each_token_class() {
        let prices = builtin("claude-opus-5-5").unwrap();
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cache_read_input_tokens: 1_000_000,
            cache_creation_input_tokens: 300_000,
            cache_creation: Some(CacheCreationBreakdown {
                ephemeral_5m_input_tokens: 100_000,
                ephemeral_1h_input_tokens: 200_000,
            }),
        };
        // 4 + 2 + 0.2 + 0.5 + 1.6
        assert!((prices.cost(&usage) - 8.3).abs() < 1e-9);
        let unsplit = Usage {
            cache_creation: None,
            ..usage
        };
        // All 300k writes at the 1h rate: 4 + 2 + 0.2 + 2.4
        assert!((prices.cost(&unsplit) - 8.6).abs() < 1e-9);
        // A split that does not cover the total: the rest at the 1h rate.
        let partial = Usage {
            cache_creation: Some(CacheCreationBreakdown {
                ephemeral_5m_input_tokens: 100_000,
                ephemeral_1h_input_tokens: 0,
            }),
            ..usage
        };
        // 4 + 2 + 0.2 + 0.5 + 1.6
        assert!((prices.cost(&partial) - 8.3).abs() < 1e-9);
    }
}
