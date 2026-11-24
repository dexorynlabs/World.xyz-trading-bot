//! Strategy interface.
//!
//! The engine asks the strategy *what to quote*; the venue answers with an
//! executable quote (price + built tx); the strategy then decides *whether to
//! execute that exact quote*. Deciding on the executable price rather than a
//! displayed bid/ask avoids acting on stale or unfillable levels.
//!
//! Add a strategy: implement [`Strategy`] in a new module and select it in
//! `main.rs`.

pub mod threshold;

use crate::types::{Fill, Intent, Quote};

/// Snapshot the engine hands to the strategy each tick.
#[derive(Debug, Clone, Copy)]
pub struct MarketState {
    /// Outcome-token holding, atomic units.
    pub position_atomic: u64,
    /// Same, UI units.
    pub position_shares: f64,
}

pub trait Strategy: Send {
    fn name(&self) -> &'static str;

    /// What to quote now, or `None` to idle this tick.
    fn next_intent(&mut self, state: &MarketState) -> Option<Intent>;

    /// Execute this exact quote?
    fn should_execute(&mut self, state: &MarketState, quote: &Quote) -> bool;

    fn on_fill(&mut self, _fill: &Fill) {}
}
