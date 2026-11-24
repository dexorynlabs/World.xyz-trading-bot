//! Pre-trade risk checks, applied to the *actual executable quote*
//! (not a stale mid), right before signing.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};

use crate::types::{Quote, Side};

#[derive(Debug, Clone)]
pub struct RiskLimits {
    pub max_order_usd: f64,
    pub max_position_usd: f64,
    pub max_trades_per_day: u32,
    pub max_buy_price: f64,
    pub min_sell_price: f64,
    pub max_consecutive_failures: u32,
}

#[derive(Debug)]
pub struct RiskManager {
    limits: RiskLimits,
    day: u64,
    trades_today: u32,
    consecutive_failures: u32,
}

impl RiskManager {
    pub fn new(limits: RiskLimits) -> Self {
        Self { limits, day: utc_day(), trades_today: 0, consecutive_failures: 0 }
    }

    pub fn halted(&self) -> bool {
        self.consecutive_failures >= self.limits.max_consecutive_failures
    }

    /// `position_shares`: current holding of the outcome token (UI units).
    pub fn check(&mut self, q: &Quote, position_shares: f64) -> Result<()> {
        self.roll_day();
        let l = &self.limits;
        if self.halted() {
            bail!("risk: halted after {} consecutive failures", self.consecutive_failures);
        }
        if self.trades_today >= l.max_trades_per_day {
            bail!("risk: MAX_TRADES_PER_DAY ({}) reached", l.max_trades_per_day);
        }
        if !q.price.is_finite() || q.price <= 0.0 {
            bail!("risk: invalid quote price {}", q.price);
        }
        if q.notional > l.max_order_usd {
            bail!("risk: notional {:.4} > MAX_ORDER_USD {}", q.notional, l.max_order_usd);
        }
        match q.intent.side {
            Side::Buy => {
                if q.price > l.max_buy_price {
                    bail!("risk: buy price {:.4} > MAX_BUY_PRICE {}", q.price, l.max_buy_price);
                }
                let after = (position_shares + q.shares) * q.price;
                if after > l.max_position_usd {
                    bail!("risk: position {:.2} would exceed MAX_POSITION_USD {}", after, l.max_position_usd);
                }
            }
            Side::Sell => {
                if q.price < l.min_sell_price {
                    bail!("risk: sell price {:.4} < MIN_SELL_PRICE {}", q.price, l.min_sell_price);
                }
            }
        }
        Ok(())
    }

    pub fn record_success(&mut self) {
        self.roll_day();
        self.trades_today += 1;
        self.consecutive_failures = 0;
    }

    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
    }

    fn roll_day(&mut self) {
        let d = utc_day();
        if d != self.day {
            self.day = d;
            self.trades_today = 0;
        }
    }
}

fn utc_day() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() / 86_400).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Intent;
    use std::time::Instant;

    fn limits() -> RiskLimits {
        RiskLimits {
            max_order_usd: 10.0,
            max_position_usd: 20.0,
            max_trades_per_day: 2,
            max_buy_price: 0.9,
            min_sell_price: 0.05,
            max_consecutive_failures: 2,
        }
    }

    fn quote(side: Side, price: f64, notional: f64) -> Quote {
        Quote {
            intent: Intent { side, amount: 0 },
            in_amount: 0,
            out_amount: 0,
            price,
            notional,
            shares: notional / price,
            transaction_b64: None,
            last_valid_block_height: None,
            execution_mode: None,
            received_at: Instant::now(),
            quote_latency_us: 0,
        }
    }

    #[test]
    fn enforces_limits() {
        let mut r = RiskManager::new(limits());
        assert!(r.check(&quote(Side::Buy, 0.5, 5.0), 0.0).is_ok());
        assert!(r.check(&quote(Side::Buy, 0.95, 5.0), 0.0).is_err()); // price
        assert!(r.check(&quote(Side::Buy, 0.5, 11.0), 0.0).is_err()); // size
        assert!(r.check(&quote(Side::Buy, 0.5, 5.0), 35.0).is_err()); // position
        assert!(r.check(&quote(Side::Sell, 0.01, 1.0), 10.0).is_err()); // min sell
        r.record_success();
        r.record_success();
        assert!(r.check(&quote(Side::Sell, 0.5, 1.0), 10.0).is_err()); // daily cap
    }

    #[test]
    fn halts_on_failures() {
        let mut r = RiskManager::new(limits());
        r.record_failure();
        r.record_failure();
        assert!(r.halted());
        r.record_success();
        assert!(!r.halted());
    }
}
