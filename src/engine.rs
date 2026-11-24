//! Strategy event loop: tick -> intent -> executable quote -> decide -> execute.

use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, error, info, warn};

use crate::config::StrategyConfig;
use crate::execution::Executor;
use crate::strategy::{MarketState, Strategy};
use crate::types::{Fill, FillStatus, Side};

pub async fn run(exec: &mut Executor, strategy: &mut dyn Strategy, cfg: &StrategyConfig, dry_run: bool) -> Result<()> {
    let mut position = exec.position_atomic().await?;
    let mut last_refresh = Instant::now();
    let mut cooldown_until = Instant::now();
    let refresh_every = Duration::from_millis(cfg.position_refresh_ms);

    let mut ticker = interval(Duration::from_millis(cfg.poll_interval_ms));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    info!(
        strategy = strategy.name(),
        position = exec.instrument().shares_to_ui(position),
        poll_ms = cfg.poll_interval_ms,
        dry_run,
        "strategy loop started (Ctrl+C to stop)"
    );

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => { info!("shutdown requested"); break; }
            _ = ticker.tick() => {}
        }
        if exec.risk().halted() {
            error!("risk manager halted trading; exiting loop");
            break;
        }
        if Instant::now() < cooldown_until {
            continue;
        }
        // In DRY_RUN the on-chain balance never changes, so keep the simulated one.
        if !dry_run && last_refresh.elapsed() >= refresh_every {
            match exec.position_atomic().await {
                Ok(p) => position = p,
                Err(e) => warn!(error = %e, "position refresh failed"),
            }
            last_refresh = Instant::now();
        }

        let state = MarketState {
            position_atomic: position,
            position_shares: exec.instrument().shares_to_ui(position),
        };
        let Some(intent) = strategy.next_intent(&state) else { continue };

        let quote = match exec.quote(intent).await {
            Ok(q) => q,
            Err(e) => {
                warn!(error = %e, "quote failed");
                continue;
            }
        };
        debug!(
            side = %intent.side,
            price = format_args!("{:.4}", quote.price),
            notional = format_args!("{:.4}", quote.notional),
            quote_ms = quote.quote_latency_us as f64 / 1000.0,
            "quote"
        );
        if !strategy.should_execute(&state, &quote) {
            continue;
        }

        match exec.execute(quote, state.position_shares).await {
            Ok(fill) => {
                log_fill(&fill);
                position = match (fill.status, fill.side) {
                    (FillStatus::Simulated, Side::Buy) => position.saturating_add(fill.out_amount),
                    (FillStatus::Simulated, Side::Sell) => position.saturating_sub(fill.in_amount),
                    (FillStatus::Confirmed, _) => exec.position_atomic().await.unwrap_or(position),
                };
                last_refresh = Instant::now();
                strategy.on_fill(&fill);
                cooldown_until = Instant::now() + Duration::from_millis(cfg.cooldown_ms);
            }
            Err(e) => {
                error!(error = %format_args!("{e:#}"), "execution rejected/failed");
                cooldown_until = Instant::now() + Duration::from_millis(cfg.cooldown_ms);
            }
        }
    }
    Ok(())
}

pub fn log_fill(fill: &Fill) {
    info!(
        side = %fill.side,
        status = ?fill.status,
        price = format_args!("{:.4}", fill.price),
        notional = format_args!("{:.4}", fill.notional),
        shares = format_args!("{:.4}", fill.shares),
        sig = fill.signature.as_deref().unwrap_or("-"),
        latency = %fill.timings,
        "FILL"
    );
}
