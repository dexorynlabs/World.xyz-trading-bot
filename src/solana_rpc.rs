//! Minimal Solana JSON-RPC client: only what the hot path needs.
//!
//! Raw JSON-RPC over the shared reqwest pool keeps the dependency tree small
//! and lets us fan a signed transaction out to several endpoints in parallel.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, bail, Context, Result};
use reqwest::Client;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};

pub struct SolanaRpc {
    http: Client,
    primary: String,
    broadcast: Vec<String>,
    commitment: String,
    next_id: AtomicU64,
}

#[derive(Deserialize)]
struct RpcEnvelope<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Deserialize, Debug)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Deserialize)]
struct WithContext<T> {
    value: T,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SignatureStatus {
    pub slot: u64,
    pub err: Option<Value>,
    pub confirmation_status: Option<String>,
}

impl SignatureStatus {
    /// True once the status has reached at least `commitment`.
    pub fn reached(&self, commitment: &str) -> bool {
        let rank = |s: &str| match s {
            "processed" => 0,
            "confirmed" => 1,
            "finalized" => 2,
            _ => -1,
        };
        self.confirmation_status
            .as_deref()
            .is_some_and(|s| rank(s) >= rank(commitment))
    }
}

impl SolanaRpc {
    pub fn new(http: Client, primary: String, broadcast: Vec<String>, commitment: String) -> Self {
        Self {
            http,
            primary,
            broadcast,
            commitment,
            next_id: AtomicU64::new(1),
        }
    }

    pub fn commitment(&self) -> &str {
        &self.commitment
    }

    pub fn endpoints(&self) -> &[String] {
        &self.broadcast
    }

    async fn call<T: DeserializeOwned>(&self, url: &str, method: &str, params: Value) -> Result<T> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let resp = self.http.post(url).json(&body).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("{method} HTTP {status}: {}", truncate(&text, 300));
        }
        let env: RpcEnvelope<T> = resp.json().await.with_context(|| format!("{method}: bad JSON"))?;
        if let Some(e) = env.error {
            bail!("{method} RPC error {}: {}", e.code, e.message);
        }
        env.result.ok_or_else(|| anyhow!("{method}: empty result"))
    }

    /// Fans the signed tx out to every configured endpoint concurrently.
    /// Succeeds if at least one endpoint accepted it.
    pub async fn send_transaction(&self, tx_b64: &str, skip_preflight: bool) -> Result<()> {
        let params = json!([tx_b64, {
            "encoding": "base64",
            "skipPreflight": skip_preflight,
            "preflightCommitment": self.commitment,
            "maxRetries": 0
        }]);
        let futs = self
            .broadcast
            .iter()
            .map(|u| self.call::<String>(u, "sendTransaction", params.clone()));
        let results = futures::future::join_all(futs).await;

        let mut first_err = None;
        let mut accepted = 0usize;
        for (url, r) in self.broadcast.iter().zip(results) {
            match r {
                Ok(_) => accepted += 1,
                Err(e) => {
                    tracing::debug!(%url, error = %e, "sendTransaction rejected");
                    first_err.get_or_insert(e);
                }
            }
        }
        match (accepted, first_err) {
            (0, Some(e)) => Err(e),
            _ => Ok(()),
        }
    }

    pub async fn signature_status(&self, signature: &str) -> Result<Option<SignatureStatus>> {
        let r: WithContext<Vec<Option<SignatureStatus>>> = self
            .call(
                &self.primary,
                "getSignatureStatuses",
                json!([[signature], { "searchTransactionHistory": false }]),
            )
            .await?;
        Ok(r.value.into_iter().next().flatten())
    }

    pub async fn block_height(&self) -> Result<u64> {
        self.call(&self.primary, "getBlockHeight", json!([{ "commitment": self.commitment }]))
            .await
    }

    /// Total atomic balance of `mint` held by `owner` across all token accounts
    /// (works for both SPL Token and Token-2022).
    pub async fn token_balance(&self, owner: &str, mint: &str) -> Result<u64> {
        let r: WithContext<Vec<Value>> = self
            .call(
                &self.primary,
                "getTokenAccountsByOwner",
                json!([owner, { "mint": mint }, { "encoding": "jsonParsed", "commitment": self.commitment }]),
            )
            .await?;
        let mut total = 0u64;
        for acc in r.value {
            if let Some(a) = acc
                .pointer("/account/data/parsed/info/tokenAmount/amount")
                .and_then(Value::as_str)
            {
                total = total.saturating_add(a.parse().unwrap_or(0));
            }
        }
        Ok(total)
    }
}

pub fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}
