//! Typed configuration loaded once from environment / `.env`.
//!
//! Everything a trader may want to tune is here; nothing is hard-coded in the
//! trading path. Validation happens at startup so the hot loop never has to.

use std::{env, fmt, str::FromStr};

use anyhow::{bail, Context, Result};

use crate::risk::RiskLimits;
use crate::types::Outcome;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Buy,
    Sell,
    Strategy,
    Markets,
}

impl FromStr for Mode {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "buy" => Mode::Buy,
            "sell" => Mode::Sell,
            "strategy" => Mode::Strategy,
            "markets" => Mode::Markets,
            other => bail!("BOT_MODE must be buy|sell|strategy|markets, got '{other}'"),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SellSize {
    All,
    Shares(f64),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PriorityFee {
    Auto,
    None,
    Lamports(u64),
}

/// Secret wrapper so the key never lands in logs via `{:?}`.
#[derive(Clone)]
pub struct Secret(pub String);
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[derive(Debug, Clone)]
pub struct StrategyConfig {
    pub entry_price: f64,
    pub take_profit_price: f64,
    pub stop_loss_price: f64,
    pub poll_interval_ms: u64,
    pub cooldown_ms: u64,
    pub position_refresh_ms: u64,
}

#[derive(Debug, Clone)]
pub struct ExecConfig {
    pub confirm_timeout_ms: u64,
    pub confirm_poll_ms: u64,
    pub rebroadcast_interval_ms: u64,
    pub skip_preflight: bool,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub mode: Mode,
    pub dry_run: bool,
    pub wallet_secret: Option<Secret>,

    // network
    pub rpc_url: String,
    pub send_rpc_urls: Vec<String>,
    pub commitment: String,
    pub http_timeout_ms: u64,

    // venue
    pub order_api_url: String,
    pub order_api_key: Option<Secret>,
    pub markets_api_url: String,
    pub http_origin: Option<String>,
    pub collateral_mint: String,
    pub collateral_decimals: u8,
    pub outcome_decimals: u8,

    // market
    pub market_id: Option<String>,
    pub yes_mint: Option<String>,
    pub no_mint: Option<String>,
    pub outcome: Outcome,
    pub market_search: Option<String>,
    pub markets_list_limit: usize,

    // order
    pub order_size_usd: f64,
    pub sell_size: SellSize,
    pub slippage_bps: u16,
    pub prediction_market_slippage_bps: Option<u16>,
    pub priority_fee: PriorityFee,
    pub extra_order_params: Vec<(String, String)>,

    pub strategy: StrategyConfig,
    pub risk: RiskLimits,
    pub exec: ExecConfig,
    pub log_json: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let mode: Mode = req("BOT_MODE", "markets")?;

        let cfg = Config {
            mode,
            dry_run: req("DRY_RUN", "true")?,
            wallet_secret: opt_str("WALLET_PRIVATE_KEY").map(Secret),

            rpc_url: string("SOLANA_RPC_URL", "https://api.mainnet-beta.solana.com"),
            send_rpc_urls: list("SOLANA_SEND_RPC_URLS"),
            commitment: string("COMMITMENT", "confirmed").to_ascii_lowercase(),
            http_timeout_ms: req("HTTP_TIMEOUT_MS", "5000")?,

            order_api_url: trim_slash(string(
                "ORDER_API_URL",
                "https://aggregator-api-proxy.world-xyz.workers.dev",
            )),
            order_api_key: opt_str("DFLOW_API_KEY").map(Secret),
            markets_api_url: trim_slash(string(
                "MARKETS_API_URL",
                "https://markets-api-proxy.world-xyz.workers.dev",
            )),
            http_origin: opt_str("HTTP_ORIGIN"),
            collateral_mint: string(
                "COLLATERAL_MINT",
                "CASHx9KJUStyftLFWGvEVf59SGeG9sh5FfcnZMVPCASH",
            ),
            collateral_decimals: req("COLLATERAL_DECIMALS", "6")?,
            outcome_decimals: req("OUTCOME_DECIMALS", "6")?,

            market_id: opt_str("MARKET_ID"),
            yes_mint: opt_str("YES_MINT"),
            no_mint: opt_str("NO_MINT"),
            outcome: req("OUTCOME", "yes")?,
            market_search: opt_str("MARKET_SEARCH"),
            markets_list_limit: req("MARKETS_LIST_LIMIT", "50")?,

            order_size_usd: req("ORDER_SIZE_USD", "5")?,
            sell_size: parse_sell_size(&string("SELL_SHARES", "all"))?,
            slippage_bps: req("SLIPPAGE_BPS", "200")?,
            prediction_market_slippage_bps: opt("PREDICTION_MARKET_SLIPPAGE_BPS")?,
            priority_fee: parse_priority_fee(&string("PRIORITY_FEE_LAMPORTS", "auto"))?,
            extra_order_params: parse_kv(&string("EXTRA_ORDER_PARAMS", "")),

            strategy: StrategyConfig {
                entry_price: req("ENTRY_PRICE", "0.40")?,
                take_profit_price: req("TAKE_PROFIT_PRICE", "0.60")?,
                stop_loss_price: req("STOP_LOSS_PRICE", "0.25")?,
                poll_interval_ms: req("POLL_INTERVAL_MS", "500")?,
                cooldown_ms: req("COOLDOWN_MS", "3000")?,
                position_refresh_ms: req("POSITION_REFRESH_MS", "10000")?,
            },
            risk: RiskLimits {
                max_order_usd: req("MAX_ORDER_USD", "50")?,
                max_position_usd: req("MAX_POSITION_USD", "200")?,
                max_trades_per_day: req("MAX_TRADES_PER_DAY", "100")?,
                max_buy_price: req("MAX_BUY_PRICE", "0.95")?,
                min_sell_price: req("MIN_SELL_PRICE", "0.01")?,
                max_consecutive_failures: req("MAX_CONSECUTIVE_FAILURES", "5")?,
            },
            exec: ExecConfig {
                confirm_timeout_ms: req("CONFIRM_TIMEOUT_MS", "30000")?,
                confirm_poll_ms: req("CONFIRM_POLL_MS", "250")?,
                rebroadcast_interval_ms: req("REBROADCAST_INTERVAL_MS", "500")?,
                skip_preflight: req("SKIP_PREFLIGHT", "true")?,
            },
            log_json: req("LOG_JSON", "false")?,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if self.mode != Mode::Markets && self.wallet_secret.is_none() {
            bail!("WALLET_PRIVATE_KEY is required for BOT_MODE={:?}", self.mode);
        }
        if self.mode != Mode::Markets
            && self.market_id.is_none()
            && self.mint_for_outcome().is_none()
        {
            bail!("set MARKET_ID, or YES_MINT/NO_MINT matching OUTCOME");
        }
        if !matches!(self.commitment.as_str(), "processed" | "confirmed" | "finalized") {
            bail!("COMMITMENT must be processed|confirmed|finalized");
        }
        if self.slippage_bps > 10_000 {
            bail!("SLIPPAGE_BPS must be <= 10000");
        }
        if self.order_size_usd <= 0.0 {
            bail!("ORDER_SIZE_USD must be > 0");
        }
        for (name, p) in [
            ("ENTRY_PRICE", self.strategy.entry_price),
            ("TAKE_PROFIT_PRICE", self.strategy.take_profit_price),
            ("STOP_LOSS_PRICE", self.strategy.stop_loss_price),
            ("MAX_BUY_PRICE", self.risk.max_buy_price),
            ("MIN_SELL_PRICE", self.risk.min_sell_price),
        ] {
            if !(0.0..=1.0).contains(&p) {
                bail!("{name} must be within 0..1 (collateral per share), got {p}");
            }
        }
        let s = &self.strategy;
        if self.mode == Mode::Strategy
            && !(s.stop_loss_price < s.entry_price && s.entry_price < s.take_profit_price)
        {
            bail!("strategy requires STOP_LOSS_PRICE < ENTRY_PRICE < TAKE_PROFIT_PRICE");
        }
        if self.strategy.poll_interval_ms == 0 || self.exec.confirm_poll_ms == 0 {
            bail!("POLL_INTERVAL_MS and CONFIRM_POLL_MS must be > 0");
        }
        Ok(())
    }

    /// Mint explicitly configured for the selected outcome (if any).
    pub fn mint_for_outcome(&self) -> Option<&str> {
        match self.outcome {
            Outcome::Yes => self.yes_mint.as_deref(),
            Outcome::No => self.no_mint.as_deref(),
        }
    }

    /// All endpoints a signed tx is broadcast to (primary first, de-duplicated).
    pub fn broadcast_urls(&self) -> Vec<String> {
        let mut urls = vec![self.rpc_url.clone()];
        for u in &self.send_rpc_urls {
            if !urls.contains(u) {
                urls.push(u.clone());
            }
        }
        urls
    }
}

// ---------------------------------------------------------------------------
// env helpers
// ---------------------------------------------------------------------------

fn opt_str(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn string(key: &str, default: &str) -> String {
    opt_str(key).unwrap_or_else(|| default.to_string())
}

fn req<T: FromStr>(key: &str, default: &str) -> Result<T>
where
    T::Err: fmt::Display,
{
    let raw = string(key, default);
    raw.parse::<T>()
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("invalid value for {key}: '{raw}'"))
}

fn opt<T: FromStr>(key: &str) -> Result<Option<T>>
where
    T::Err: fmt::Display,
{
    match opt_str(key) {
        None => Ok(None),
        Some(raw) => raw
            .parse::<T>()
            .map(Some)
            .map_err(|e| anyhow::anyhow!("invalid value for {key}: '{raw}': {e}")),
    }
}

fn list(key: &str) -> Vec<String> {
    string(key, "")
        .split(',')
        .map(|s| trim_slash(s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect()
}

fn trim_slash(s: String) -> String {
    s.trim_end_matches('/').to_string()
}

fn parse_sell_size(raw: &str) -> Result<SellSize> {
    if raw.eq_ignore_ascii_case("all") {
        return Ok(SellSize::All);
    }
    let n: f64 = raw.parse().with_context(|| format!("SELL_SHARES: '{raw}'"))?;
    if n <= 0.0 {
        bail!("SELL_SHARES must be > 0 or 'all'");
    }
    Ok(SellSize::Shares(n))
}

fn parse_priority_fee(raw: &str) -> Result<PriorityFee> {
    Ok(match raw.to_ascii_lowercase().as_str() {
        "auto" => PriorityFee::Auto,
        "none" | "0" | "" => PriorityFee::None,
        n => PriorityFee::Lamports(
            n.parse()
                .with_context(|| format!("PRIORITY_FEE_LAMPORTS: '{raw}'"))?,
        ),
    })
}

fn parse_kv(raw: &str) -> Vec<(String, String)> {
    raw.split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            let (k, v) = (k.trim(), v.trim());
            (!k.is_empty()).then(|| (k.to_string(), v.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_helpers() {
        assert_eq!(parse_sell_size("ALL").unwrap(), SellSize::All);
        assert_eq!(parse_sell_size("2.5").unwrap(), SellSize::Shares(2.5));
        assert_eq!(parse_priority_fee("auto").unwrap(), PriorityFee::Auto);
        assert_eq!(parse_priority_fee("5000").unwrap(), PriorityFee::Lamports(5000));
        assert_eq!(
            parse_kv("a=1&b=two& =x"),
            vec![("a".into(), "1".into()), ("b".into(), "two".into())]
        );
    }
}
