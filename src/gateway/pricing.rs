//! Model prices, for the cost of answered requests.

use serde::{Deserialize, Serialize};

use super::TokenUsage;

/// The price of one model family: currency units per million tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPrice {
    /// Model id prefix (case-insensitive; the longest matching prefix wins).
    pub prefix: String,
    pub input_per_million: f64,
    pub output_per_million: f64,
}

impl ModelPrice {
    pub fn new(prefix: impl Into<String>, input_per_million: f64, output_per_million: f64) -> Self {
        Self {
            prefix: prefix.into(),
            input_per_million,
            output_per_million,
        }
    }
}

/// Prices by model prefix. Empty by default: prices change and differ by
/// provider and contract, so the caller states its own.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PriceTable {
    prices: Vec<ModelPrice>,
}

impl PriceTable {
    pub fn new(prices: impl IntoIterator<Item = ModelPrice>) -> Self {
        Self {
            prices: prices.into_iter().collect(),
        }
    }

    /// Parses a JSON array of [`ModelPrice`]s.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str::<Vec<ModelPrice>>(json).map(Self::new)
    }

    pub fn is_empty(&self) -> bool {
        self.prices.is_empty()
    }

    /// The price of `model`, if listed.
    pub fn price(&self, model: &str) -> Option<&ModelPrice> {
        let lower = model.trim().to_ascii_lowercase();
        self.prices
            .iter()
            .filter(|price| lower.starts_with(&price.prefix.trim().to_ascii_lowercase()))
            .max_by_key(|price| price.prefix.trim().len())
    }

    /// The cost of `usage` on `model` in millionths of the currency unit
    /// (0 when the model has no price).
    pub fn cost_micros(&self, model: &str, usage: &TokenUsage) -> u64 {
        let Some(price) = self.price(model) else {
            return 0;
        };
        let input = usage.prompt_tokens.unwrap_or(0) as f64;
        let output = usage.completion_tokens.unwrap_or(0) as f64;
        // Per million tokens × tokens = micro-units per token × tokens.
        (input * price.input_per_million + output * price.output_per_million)
            .round()
            .max(0.0) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_longest_prefix_prices_a_model() {
        let table = PriceTable::new([
            ModelPrice::new("gpt-4o", 2.5, 10.0),
            ModelPrice::new("gpt-4o-mini", 0.15, 0.6),
        ]);
        let usage = TokenUsage {
            prompt_tokens: Some(1_000_000),
            completion_tokens: Some(100_000),
            total_tokens: None,
        };
        // 0.15 + 0.06 units.
        assert_eq!(table.cost_micros("gpt-4o-mini-2024", &usage), 210_000);
        assert_eq!(table.cost_micros("gpt-4o", &usage), 3_500_000);
        assert_eq!(table.cost_micros("unknown", &usage), 0);
    }
}
