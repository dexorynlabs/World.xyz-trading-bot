//! Venue adapters for World (world.xyz).
//!
//! * [`orders`]  — DFlow-compatible `/order` builder (quote + unsigned tx in one call)
//! * [`markets`] — World market catalog (discovery, mint resolution)
//!
//! To port the template to another Solana venue, replace these two modules;
//! execution, risk and strategy layers are venue-agnostic.

pub mod markets;
pub mod orders;

use serde_json::Value;

/// Reads a JSON value that may be a number or a numeric string.
pub(crate) fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Reads an atomic amount that may be a number or a numeric string.
pub(crate) fn atomic(v: Option<&Value>) -> Option<u64> {
    match v? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// First present string field among `keys`.
pub(crate) fn first_str<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str))
}
