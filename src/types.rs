//! Core domain types shared across venue, execution, risk and strategy layers.

use std::{fmt, str::FromStr, time::Instant};

use anyhow::bail;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Yes,
    No,
}

impl FromStr for Outcome {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> anyhow::Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "yes" | "y" => Outcome::Yes,
            "no" | "n" => Outcome::No,
            other => bail!("OUTCOME must be yes|no, got '{other}'"),
        })
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Outcome::Yes => "YES",
            Outcome::No => "NO",
        })
    }
}

/// Resolved market the bot trades: the collateral and one outcome token.
#[derive(Debug, Clone)]
pub struct Instrument {
    pub label: String,
    pub outcome: Outcome,
    pub outcome_mint: String,
    pub collateral_mint: String,
    pub collateral_decimals: u8,
    pub outcome_decimals: u8,
}

impl Instrument {
    pub fn collateral_to_atomic(&self, ui: f64) -> u64 {
        to_atomic(ui, self.collateral_decimals)
    }
    pub fn shares_to_atomic(&self, ui: f64) -> u64 {
        to_atomic(ui, self.outcome_decimals)
    }
    pub fn collateral_to_ui(&self, atomic: u64) -> f64 {
        to_ui(atomic, self.collateral_decimals)
    }
    pub fn shares_to_ui(&self, atomic: u64) -> f64 {
        to_ui(atomic, self.outcome_decimals)
    }
}

/// What to trade. `amount` is atomic units of the *input* token:
/// collateral for a buy, outcome shares for a sell.
#[derive(Debug, Clone, Copy)]
pub struct Intent {
    pub side: Side,
    pub amount: u64,
}

/// An executable quote: the venue already built the transaction, so
/// "quote" and "order" are the same round trip (latency first).
#[derive(Debug, Clone)]
pub struct Quote {
    pub intent: Intent,
    /// Atomic input spent.
    pub in_amount: u64,
    /// Atomic output received.
    pub out_amount: u64,
    /// Effective price in collateral per share (0..1).
    pub price: f64,
    /// Notional in collateral (UI units).
    pub notional: f64,
    /// Shares moved (UI units).
    pub shares: f64,
    /// Base64 wire transaction, unsigned by us.
    pub transaction_b64: Option<String>,
    pub last_valid_block_height: Option<u64>,
    pub execution_mode: Option<String>,
    pub received_at: Instant,
    pub quote_latency_us: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillStatus {
    /// DRY_RUN: everything except broadcast.
    Simulated,
    Confirmed,
}

#[derive(Debug, Clone)]
pub struct Fill {
    pub side: Side,
    pub status: FillStatus,
    pub signature: Option<String>,
    pub price: f64,
    pub in_amount: u64,
    pub out_amount: u64,
    pub notional: f64,
    pub shares: f64,
    pub timings: Timings,
}

/// Per-order latency breakdown, microseconds.
#[derive(Debug, Clone, Copy, Default)]
pub struct Timings {
    pub quote_us: u128,
    pub sign_us: u128,
    pub send_us: u128,
    pub confirm_us: u128,
    /// Quote received -> confirmed (or simulated).
    pub total_us: u128,
}

impl fmt::Display for Timings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "quote={:.1}ms sign={:.3}ms send={:.1}ms confirm={:.1}ms total={:.1}ms",
            ms(self.quote_us),
            ms(self.sign_us),
            ms(self.send_us),
            ms(self.confirm_us),
            ms(self.total_us)
        )
    }
}

fn ms(us: u128) -> f64 {
    us as f64 / 1000.0
}

pub fn to_atomic(ui: f64, decimals: u8) -> u64 {
    (ui * 10f64.powi(decimals as i32)).floor().max(0.0) as u64
}

pub fn to_ui(atomic: u64, decimals: u8) -> f64 {
    atomic as f64 / 10f64.powi(decimals as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_conversion() {
        assert_eq!(to_atomic(5.0, 6), 5_000_000);
        assert_eq!(to_atomic(0.1234567, 6), 123_456);
        assert!((to_ui(2_500_000, 6) - 2.5).abs() < 1e-12);
    }
}
