//! Threshold strategy: buy below ENTRY_PRICE, exit at TAKE_PROFIT_PRICE or
//! STOP_LOSS_PRICE. Simple on purpose — the reference for writing your own.

use super::{MarketState, Strategy};
use crate::types::{Fill, Intent, Quote, Side};

pub struct ThresholdStrategy {
    pub entry_price: f64,
    pub take_profit_price: f64,
    pub stop_loss_price: f64,
    /// Buy notional, atomic collateral.
    pub order_amount_atomic: u64,
    /// Positions below this are treated as flat (dust).
    pub dust_atomic: u64,
}

impl Strategy for ThresholdStrategy {
    fn name(&self) -> &'static str {
        "threshold"
    }

    fn next_intent(&mut self, s: &MarketState) -> Option<Intent> {
        Some(if s.position_atomic <= self.dust_atomic {
            Intent { side: Side::Buy, amount: self.order_amount_atomic }
        } else {
            Intent { side: Side::Sell, amount: s.position_atomic }
        })
    }

    fn should_execute(&mut self, _s: &MarketState, q: &Quote) -> bool {
        match q.intent.side {
            Side::Buy => q.price <= self.entry_price,
            Side::Sell => q.price >= self.take_profit_price || q.price <= self.stop_loss_price,
        }
    }

    fn on_fill(&mut self, fill: &Fill) {
        tracing::info!(side = %fill.side, price = fill.price, "threshold: filled");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn q(side: Side, price: f64) -> Quote {
        Quote {
            intent: Intent { side, amount: 1 },
            in_amount: 1,
            out_amount: 1,
            price,
            notional: 1.0,
            shares: 1.0,
            transaction_b64: None,
            last_valid_block_height: None,
            execution_mode: None,
            received_at: Instant::now(),
            quote_latency_us: 0,
        }
    }

    #[test]
    fn decisions() {
        let mut s = ThresholdStrategy {
            entry_price: 0.4,
            take_profit_price: 0.6,
            stop_loss_price: 0.25,
            order_amount_atomic: 5_000_000,
            dust_atomic: 1_000,
        };
        let flat = MarketState { position_atomic: 0, position_shares: 0.0 };
        let long = MarketState { position_atomic: 10_000_000, position_shares: 10.0 };

        assert_eq!(s.next_intent(&flat).unwrap().side, Side::Buy);
        assert_eq!(s.next_intent(&long).unwrap().amount, 10_000_000);
        assert!(s.should_execute(&flat, &q(Side::Buy, 0.39)));
        assert!(!s.should_execute(&flat, &q(Side::Buy, 0.41)));
        assert!(s.should_execute(&long, &q(Side::Sell, 0.61)));
        assert!(s.should_execute(&long, &q(Side::Sell, 0.20)));
        assert!(!s.should_execute(&long, &q(Side::Sell, 0.50)));
    }
}
