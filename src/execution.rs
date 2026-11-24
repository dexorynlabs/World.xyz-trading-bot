//! Order execution pipeline:
//!
//!   quote (+built tx)  ->  risk check  ->  sign in place  ->  parallel broadcast
//!   ->  confirm (with rebroadcast + blockhash-expiry guard)  ->  async settle
//!
//! Every stage is timed; the breakdown is logged with each fill.

use std::{sync::Arc, time::{Duration, Instant}};

use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use tracing::{debug, info, warn};

use crate::config::ExecConfig;
use crate::risk::RiskManager;
use crate::solana_rpc::SolanaRpc;
use crate::types::{Fill, FillStatus, Instrument, Intent, Quote, Side, Timings};
use crate::venue::orders::{OrderClient, OrderRequest};
use crate::wallet::Wallet;

pub struct Executor {
    rpc: Arc<SolanaRpc>,
    orders: OrderClient,
    wallet: Wallet,
    instrument: Instrument,
    exec: ExecConfig,
    dry_run: bool,
    risk: RiskManager,
}

impl Executor {
    pub fn new(
        rpc: Arc<SolanaRpc>,
        orders: OrderClient,
        wallet: Wallet,
        instrument: Instrument,
        exec: ExecConfig,
        dry_run: bool,
        risk: RiskManager,
    ) -> Self {
        Self { rpc, orders, wallet, instrument, exec, dry_run, risk }
    }

    pub fn instrument(&self) -> &Instrument {
        &self.instrument
    }

    pub fn risk(&self) -> &RiskManager {
        &self.risk
    }

    pub fn buy_intent(&self, notional_usd: f64) -> Intent {
        Intent { side: Side::Buy, amount: self.instrument.collateral_to_atomic(notional_usd) }
    }

    pub fn sell_intent(&self, shares_atomic: u64) -> Intent {
        Intent { side: Side::Sell, amount: shares_atomic }
    }

    /// Current outcome-token holding (atomic).
    pub async fn position_atomic(&self) -> Result<u64> {
        self.rpc.token_balance(self.wallet.pubkey(), &self.instrument.outcome_mint).await
    }

    /// One round trip: executable price + unsigned transaction.
    pub async fn quote(&self, intent: Intent) -> Result<Quote> {
        ensure!(intent.amount > 0, "order amount is zero");
        let inst = &self.instrument;
        let (input, output) = match intent.side {
            Side::Buy => (inst.collateral_mint.as_str(), inst.outcome_mint.as_str()),
            Side::Sell => (inst.outcome_mint.as_str(), inst.collateral_mint.as_str()),
        };
        let r = self
            .orders
            .order(&OrderRequest {
                input_mint: input,
                output_mint: output,
                amount: intent.amount,
                user_public_key: self.wallet.pubkey(),
            })
            .await?;

        let (coll, shares) = match intent.side {
            Side::Buy => (r.in_amount, r.out_amount),
            Side::Sell => (r.out_amount, r.in_amount),
        };
        ensure!(coll > 0 && shares > 0, "empty quote (in={}, out={})", r.in_amount, r.out_amount);
        let notional = inst.collateral_to_ui(coll);
        let shares_ui = inst.shares_to_ui(shares);

        Ok(Quote {
            intent,
            in_amount: r.in_amount,
            out_amount: r.out_amount,
            price: notional / shares_ui,
            notional,
            shares: shares_ui,
            transaction_b64: r.transaction_b64,
            last_valid_block_height: r.last_valid_block_height,
            execution_mode: r.execution_mode,
            received_at: Instant::now(),
            quote_latency_us: r.latency_us,
        })
    }

    /// Risk-checks, signs and (unless DRY_RUN) broadcasts and confirms `q`.
    /// `position_shares` is the current holding in UI units.
    pub async fn execute(&mut self, q: Quote, position_shares: f64) -> Result<Fill> {
        self.risk.check(&q, position_shares)?;
        let mut t = Timings { quote_us: q.quote_latency_us, ..Default::default() };

        let ts = Instant::now();
        let tx_b64 = q
            .transaction_b64
            .as_deref()
            .context("venue returned no transaction (check userPublicKey / venue support)")?;
        let mut wire = B64.decode(tx_b64).context("transaction is not valid base64")?;
        let signature = self.wallet.sign_wire_tx(&mut wire)?;
        let signed = B64.encode(&wire);
        t.sign_us = ts.elapsed().as_micros();

        let status = if self.dry_run {
            info!(side = %q.intent.side, price = q.price, sig = %signature, "DRY_RUN: signed, not broadcast");
            FillStatus::Simulated
        } else {
            match self.land(&signed, &signature, &q, &mut t).await {
                Ok(()) => FillStatus::Confirmed,
                Err(e) => {
                    self.risk.record_failure();
                    return Err(e.context(format!("execution failed (sig {signature})")));
                }
            }
        };
        self.risk.record_success();
        t.total_us = q.received_at.elapsed().as_micros();

        Ok(Fill {
            side: q.intent.side,
            status,
            signature: Some(signature),
            price: q.price,
            in_amount: q.in_amount,
            out_amount: q.out_amount,
            notional: q.notional,
            shares: q.shares,
            timings: t,
        })
    }

    async fn land(&self, signed: &str, sig: &str, q: &Quote, t: &mut Timings) -> Result<()> {
        let ts = Instant::now();
        self.rpc.send_transaction(signed, self.exec.skip_preflight).await?;
        t.send_us = ts.elapsed().as_micros();
        debug!(%sig, endpoints = self.rpc.endpoints().len(), "broadcast");

        let tc = Instant::now();
        self.confirm(signed, sig, q.last_valid_block_height).await?;
        if q.execution_mode.as_deref() == Some("async") {
            self.await_async_settlement(sig).await?;
        }
        t.confirm_us = tc.elapsed().as_micros();
        Ok(())
    }

    async fn confirm(&self, signed: &str, sig: &str, last_valid: Option<u64>) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(self.exec.confirm_timeout_ms);
        let poll = Duration::from_millis(self.exec.confirm_poll_ms);
        let rebroadcast = Duration::from_millis(self.exec.rebroadcast_interval_ms);
        let mut last_send = Instant::now();
        let mut iter: u32 = 0;

        loop {
            tokio::time::sleep(poll).await;
            iter = iter.wrapping_add(1);

            match self.rpc.signature_status(sig).await {
                Ok(Some(st)) => {
                    if let Some(err) = st.err {
                        bail!("transaction failed on-chain: {err}");
                    }
                    if st.reached(self.rpc.commitment()) {
                        debug!(slot = st.slot, "confirmed");
                        return Ok(());
                    }
                }
                Ok(None) => {}
                Err(e) => debug!(error = %e, "status poll error"),
            }

            if Instant::now() >= deadline {
                bail!("not confirmed within {} ms", self.exec.confirm_timeout_ms);
            }
            if let (Some(h), 0) = (last_valid, iter % 8) {
                if let Ok(bh) = self.rpc.block_height().await {
                    if bh > h {
                        bail!("blockhash expired (height {bh} > last valid {h})");
                    }
                }
            }
            if last_send.elapsed() >= rebroadcast {
                // Fire-and-forget so polling cadence is unaffected.
                let rpc = Arc::clone(&self.rpc);
                let tx = signed.to_owned();
                tokio::spawn(async move {
                    let _ = rpc.send_transaction(&tx, true).await;
                });
                last_send = Instant::now();
            }
        }
    }

    /// Async-mode orders land a tx first, then fill off the critical path.
    async fn await_async_settlement(&self, sig: &str) -> Result<()> {
        let deadline = Instant::now() + Duration::from_millis(self.exec.confirm_timeout_ms);
        loop {
            match self.orders.order_status(sig).await {
                Ok(s) if s == "closed" => return Ok(()),
                Ok(s) if s == "failed" || s == "expired" => bail!("async order {s}"),
                Ok(s) => debug!(status = %s, "async order pending"),
                Err(e) => warn!(error = %e, "order-status poll error"),
            }
            if Instant::now() >= deadline {
                bail!("async order not settled within timeout");
            }
            tokio::time::sleep(Duration::from_millis(self.exec.confirm_poll_ms)).await;
        }
    }
}
