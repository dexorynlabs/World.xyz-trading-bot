//! DFlow-compatible order API (`GET /order`, `GET /order-status`).
//!
//! `/order` returns the executable quote *and* a fully built transaction for
//! `userPublicKey`, so a single round trip gives us something we can sign and
//! broadcast immediately.

use std::time::Instant;

use anyhow::{bail, Context, Result};
use reqwest::{Client, RequestBuilder};
use serde_json::Value;

use super::{atomic, first_str};
use crate::config::{Config, PriorityFee};
use crate::solana_rpc::truncate;

pub struct OrderRequest<'a> {
    pub input_mint: &'a str,
    pub output_mint: &'a str,
    pub amount: u64,
    pub user_public_key: &'a str,
}

/// Normalized `/order` response. Raw JSON is kept for debugging/extensions.
#[derive(Debug, Clone)]
pub struct OrderResponse {
    pub in_amount: u64,
    pub out_amount: u64,
    pub transaction_b64: Option<String>,
    pub last_valid_block_height: Option<u64>,
    pub execution_mode: Option<String>,
    /// Full venue response, for extensions / debugging.
    #[allow(dead_code)]
    pub raw: Value,
    pub latency_us: u128,
}

pub struct OrderClient {
    http: Client,
    base: String,
    api_key: Option<String>,
    origin: Option<String>,
    /// Static query params computed once from config (slippage, fees, extras).
    static_params: Vec<(String, String)>,
}

impl OrderClient {
    pub fn new(http: Client, cfg: &Config) -> Self {
        let mut p: Vec<(String, String)> = vec![("slippageBps".into(), cfg.slippage_bps.to_string())];
        if let Some(bps) = cfg.prediction_market_slippage_bps {
            p.push(("predictionMarketSlippageBps".into(), bps.to_string()));
        }
        match &cfg.priority_fee {
            PriorityFee::Auto => p.push(("prioritizationFeeLamports".into(), "auto".into())),
            PriorityFee::Lamports(n) => p.push(("prioritizationFeeLamports".into(), n.to_string())),
            PriorityFee::None => {}
        }
        p.extend(cfg.extra_order_params.iter().cloned());

        Self {
            http,
            base: cfg.order_api_url.clone(),
            api_key: cfg.order_api_key.as_ref().map(|s| s.0.clone()),
            origin: cfg.http_origin.clone(),
            static_params: p,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn decorate(&self, rb: RequestBuilder) -> RequestBuilder {
        let rb = match &self.api_key {
            Some(k) => rb.header("x-api-key", k),
            None => rb,
        };
        match &self.origin {
            Some(o) => rb.header("origin", o),
            None => rb,
        }
    }

    pub async fn order(&self, req: &OrderRequest<'_>) -> Result<OrderResponse> {
        let t = Instant::now();
        let amount = req.amount.to_string();
        let rb = self
            .http
            .get(format!("{}/order", self.base))
            .query(&[
                ("inputMint", req.input_mint),
                ("outputMint", req.output_mint),
                ("amount", amount.as_str()),
                ("userPublicKey", req.user_public_key),
            ])
            .query(&self.static_params);

        let resp = self.decorate(rb).send().await.context("/order request failed")?;
        let status = resp.status();
        let body = resp.text().await.context("/order body")?;
        if !status.is_success() {
            bail!("/order HTTP {status}: {}", truncate(&body, 400));
        }
        let raw: Value = serde_json::from_str(&body)
            .with_context(|| format!("/order invalid JSON: {}", truncate(&body, 200)))?;
        if let Some(code) = raw.get("code").and_then(Value::as_str) {
            bail!("/order rejected: {code} {}", first_str(&raw, &["msg", "message"]).unwrap_or(""));
        }

        let in_amount = atomic(raw.get("inAmount")).unwrap_or(req.amount);
        let out_amount = atomic(raw.get("outAmount")).context("/order: missing outAmount")?;
        Ok(OrderResponse {
            in_amount,
            out_amount,
            transaction_b64: first_str(&raw, &["transaction", "tx", "swapTransaction"]).map(str::to_owned),
            last_valid_block_height: atomic(raw.get("lastValidBlockHeight")),
            execution_mode: first_str(&raw, &["executionMode"]).map(str::to_owned),
            latency_us: t.elapsed().as_micros(),
            raw,
        })
    }

    /// Status of an async-mode order. Returns the venue's status string
    /// (e.g. "pending", "open", "closed", "failed", "expired").
    pub async fn order_status(&self, signature: &str) -> Result<String> {
        let rb = self
            .http
            .get(format!("{}/order-status", self.base))
            .query(&[("signature", signature)]);
        let resp = self.decorate(rb).send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            bail!("/order-status HTTP {status}: {}", truncate(&body, 300));
        }
        let v: Value = serde_json::from_str(&body)?;
        Ok(first_str(&v, &["status"]).unwrap_or("unknown").to_ascii_lowercase())
    }
}
