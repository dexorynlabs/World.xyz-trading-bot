//! World market catalog (`GET /api/v1/markets`, cursor-paginated).
//!
//! The catalog is undocumented, so parsing is deliberately tolerant: we read
//! fields by name from raw JSON and accept several known aliases. The catalog
//! is used for discovery and mint resolution only — never on the hot path.

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde_json::Value;

use super::{first_str, num};
use crate::solana_rpc::truncate;
use crate::types::Outcome;

#[derive(Debug, Clone)]
pub struct MarketSummary {
    pub id: String,
    pub title: String,
    pub status: Option<String>,
    pub yes_mint: Option<String>,
    pub no_mint: Option<String>,
    pub initialized: Option<bool>,
    pub yes_bid: Option<f64>,
    pub yes_ask: Option<f64>,
    pub no_bid: Option<f64>,
    pub no_ask: Option<f64>,
    pub volume: Option<f64>,
}

impl MarketSummary {
    pub fn mint(&self, outcome: Outcome) -> Option<&str> {
        match outcome {
            Outcome::Yes => self.yes_mint.as_deref(),
            Outcome::No => self.no_mint.as_deref(),
        }
    }

    fn from_json(v: &Value, collateral_mint: &str) -> Option<Self> {
        let id = first_str(v, &["ticker", "marketTicker", "id", "slug"])?.to_string();
        let title = first_str(v, &["title", "question", "name"]).unwrap_or("").to_string();

        // `accounts` is keyed by collateral mint -> { yesMint, noMint, isInitialized, ... }
        let accounts = v.get("accounts");
        let acct = accounts
            .and_then(|a| a.get(collateral_mint))
            .or_else(|| accounts.and_then(Value::as_object).and_then(|m| m.values().next()))
            .unwrap_or(v);

        Some(Self {
            id,
            title,
            status: first_str(v, &["status"]).map(str::to_owned),
            yes_mint: first_str(acct, &["yesMint"]).map(str::to_owned),
            no_mint: first_str(acct, &["noMint"]).map(str::to_owned),
            initialized: acct.get("isInitialized").and_then(Value::as_bool),
            yes_bid: price(v.get("yesBid")),
            yes_ask: price(v.get("yesAsk")),
            no_bid: price(v.get("noBid")),
            no_ask: price(v.get("noAsk")),
            volume: v.get("volume").and_then(num),
        })
    }
}

/// Normalizes a price to 0..1. Values > 1 are treated as cents.
fn price(v: Option<&Value>) -> Option<f64> {
    let p = num(v?)?;
    Some(if p > 1.0 { p / 100.0 } else { p })
}

pub struct MarketsClient {
    http: Client,
    base: String,
    origin: Option<String>,
    collateral_mint: String,
}

impl MarketsClient {
    pub fn new(http: Client, base: String, origin: Option<String>, collateral_mint: String) -> Self {
        Self { http, base, origin, collateral_mint }
    }

    async fn page(&self, cursor: Option<&str>) -> Result<(Vec<Value>, Option<String>)> {
        let mut rb = self.http.get(format!("{}/api/v1/markets", self.base));
        if let Some(c) = cursor {
            rb = rb.query(&[("cursor", c)]);
        }
        if let Some(o) = &self.origin {
            rb = rb.header("origin", o);
        }
        let resp = rb.send().await.context("markets request failed")?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            bail!("markets HTTP {status}: {}", truncate(&body, 300));
        }
        let v: Value = serde_json::from_str(&body).context("markets: invalid JSON")?;

        let items = match &v {
            Value::Array(a) => a.clone(),
            _ => ["markets", "data", "items", "results"]
                .iter()
                .find_map(|k| v.get(*k).and_then(Value::as_array).cloned())
                .unwrap_or_default(),
        };
        let next = first_str(&v, &["cursor", "nextCursor", "next_cursor"])
            .filter(|c| !c.is_empty())
            .map(str::to_owned);
        Ok((items, next))
    }

    /// Walks the catalog, calling `visit` per market until it returns `false`.
    async fn scan(&self, max_pages: usize, mut visit: impl FnMut(MarketSummary) -> bool) -> Result<()> {
        let mut cursor: Option<String> = None;
        for _ in 0..max_pages {
            let (items, next) = self.page(cursor.as_deref()).await?;
            if items.is_empty() {
                break;
            }
            for item in &items {
                if let Some(m) = MarketSummary::from_json(item, &self.collateral_mint) {
                    if !visit(m) {
                        return Ok(());
                    }
                }
            }
            match next {
                Some(c) if Some(&c) != cursor.as_ref() => cursor = Some(c),
                _ => break,
            }
        }
        Ok(())
    }

    pub async fn find(&self, id: &str) -> Result<Option<MarketSummary>> {
        let mut found = None;
        self.scan(200, |m| {
            if m.id.eq_ignore_ascii_case(id) {
                found = Some(m);
                false
            } else {
                true
            }
        })
        .await?;
        Ok(found)
    }

    pub async fn search(&self, query: Option<&str>, limit: usize) -> Result<Vec<MarketSummary>> {
        let q = query.map(str::to_ascii_lowercase);
        let mut out = Vec::new();
        self.scan(200, |m| {
            let hit = q.as_ref().map_or(true, |q| {
                m.id.to_ascii_lowercase().contains(q) || m.title.to_ascii_lowercase().contains(q)
            });
            if hit {
                out.push(m);
            }
            out.len() < limit
        })
        .await?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_market_shape() {
        let v = json!({
            "ticker": "ARGvEGY", "title": "ARG vs EGY", "yesBid": "0.24", "yesAsk": 26,
            "accounts": { "CASH": { "yesMint": "Y", "noMint": "N", "isInitialized": true } }
        });
        let m = MarketSummary::from_json(&v, "CASH").unwrap();
        assert_eq!(m.mint(Outcome::Yes), Some("Y"));
        assert_eq!(m.mint(Outcome::No), Some("N"));
        assert_eq!(m.yes_bid, Some(0.24));
        assert_eq!(m.yes_ask, Some(0.26));
        assert_eq!(m.initialized, Some(true));
    }
}
