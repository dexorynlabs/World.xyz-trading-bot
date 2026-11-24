//! world.xyz trading bot — entry point and wiring.

mod config;
mod engine;
mod execution;
mod http;
mod risk;
mod solana_rpc;
mod strategy;
mod types;
mod venue;
mod wallet;

use std::{sync::Arc, time::Duration};

use anyhow::{bail, Context, Result};
use tracing::{info, warn};

use config::{Config, Mode, SellSize};
use execution::Executor;
use risk::RiskManager;
use solana_rpc::SolanaRpc;
use strategy::threshold::ThresholdStrategy;
use types::Instrument;
use venue::{markets::MarketsClient, orders::OrderClient};
use wallet::Wallet;

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("fatal: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    dotenvy::dotenv().ok();
    let cfg = Config::from_env()?;
    init_logging(cfg.log_json);
    info!(mode = ?cfg.mode, dry_run = cfg.dry_run, order_api = %cfg.order_api_url, "starting");

    let http = http::build_client(cfg.http_timeout_ms)?;
    let markets = MarketsClient::new(
        http.clone(),
        cfg.markets_api_url.clone(),
        cfg.http_origin.clone(),
        cfg.collateral_mint.clone(),
    );

    if cfg.mode == Mode::Markets {
        return list_markets(&markets, &cfg).await;
    }

    let wallet = Wallet::from_secret(&cfg.wallet_secret.as_ref().expect("validated").0)?;
    info!(wallet = wallet.pubkey(), "wallet loaded");

    let instrument = resolve_instrument(&cfg, &markets).await?;
    info!(market = %instrument.label, outcome = %instrument.outcome, mint = %instrument.outcome_mint, "instrument");

    let rpc = Arc::new(SolanaRpc::new(
        http.clone(),
        cfg.rpc_url.clone(),
        cfg.broadcast_urls(),
        cfg.commitment.clone(),
    ));
    let orders = OrderClient::new(http.clone(), &cfg);

    // Pre-open every connection on the hot path, then keep them warm.
    let mut hot_urls = cfg.broadcast_urls();
    hot_urls.push(orders.base_url().to_string());
    http::warm_up(&http, &hot_urls).await;
    spawn_keepalive(http.clone(), hot_urls);

    let mut exec = Executor::new(
        rpc,
        orders,
        wallet,
        instrument,
        cfg.exec.clone(),
        cfg.dry_run,
        RiskManager::new(cfg.risk.clone()),
    );

    match cfg.mode {
        Mode::Buy => {
            let q = exec.quote(exec.buy_intent(cfg.order_size_usd)).await?;
            let pos = exec.instrument().shares_to_ui(exec.position_atomic().await.unwrap_or(0));
            info!(price = q.price, shares = q.shares, notional = q.notional, "buy quote");
            engine::log_fill(&exec.execute(q, pos).await?);
        }
        Mode::Sell => {
            let held = exec.position_atomic().await?;
            let amount = match cfg.sell_size {
                SellSize::All => held,
                SellSize::Shares(n) => exec.instrument().shares_to_atomic(n),
            };
            if amount == 0 {
                bail!("nothing to sell (position is 0)");
            }
            if amount > held && !cfg.dry_run {
                warn!(held, amount, "selling more than held; venue will likely reject");
            }
            let pos = exec.instrument().shares_to_ui(held);
            let q = exec.quote(exec.sell_intent(amount)).await?;
            info!(price = q.price, shares = q.shares, notional = q.notional, "sell quote");
            engine::log_fill(&exec.execute(q, pos).await?);
        }
        Mode::Strategy => {
            let s = &cfg.strategy;
            let mut strat = ThresholdStrategy {
                entry_price: s.entry_price,
                take_profit_price: s.take_profit_price,
                stop_loss_price: s.stop_loss_price,
                order_amount_atomic: exec.instrument().collateral_to_atomic(cfg.order_size_usd),
                dust_atomic: exec.instrument().shares_to_atomic(0.01),
            };
            engine::run(&mut exec, &mut strat, s, cfg.dry_run).await?;
        }
        Mode::Markets => unreachable!(),
    }
    Ok(())
}

async fn resolve_instrument(cfg: &Config, markets: &MarketsClient) -> Result<Instrument> {
    let (label, mint) = match (cfg.mint_for_outcome(), cfg.market_id.as_deref()) {
        (Some(mint), id) => (id.unwrap_or("manual").to_string(), mint.to_string()),
        (None, Some(id)) => {
            let m = markets
                .find(id)
                .await?
                .with_context(|| format!("market '{id}' not found in catalog"))?;
            if m.initialized == Some(false) {
                warn!(market = %m.id, "market not initialized on-chain; first trade may pay init cost");
            }
            let mint = m
                .mint(cfg.outcome)
                .with_context(|| format!("catalog has no {} mint for '{id}'", cfg.outcome))?
                .to_string();
            (format!("{} | {}", m.id, m.title), mint)
        }
        (None, None) => bail!("no market configured"),
    };
    Ok(Instrument {
        label,
        outcome: cfg.outcome,
        outcome_mint: mint,
        collateral_mint: cfg.collateral_mint.clone(),
        collateral_decimals: cfg.collateral_decimals,
        outcome_decimals: cfg.outcome_decimals,
    })
}

async fn list_markets(markets: &MarketsClient, cfg: &Config) -> Result<()> {
    let list = markets.search(cfg.market_search.as_deref(), cfg.markets_list_limit).await?;
    let f = |p: Option<f64>| p.map_or("  -  ".to_string(), |v| format!("{v:.3}"));
    println!(
        "{:<28} {:>6} {:>6} {:>6} {:>6} {:>12} {:<10}  {}",
        "MARKET_ID", "Ybid", "Yask", "Nbid", "Nask", "VOLUME", "STATUS", "TITLE"
    );
    for m in &list {
        println!(
            "{:<28} {:>6} {:>6} {:>6} {:>6} {:>12} {:<10}  {}",
            m.id,
            f(m.yes_bid),
            f(m.yes_ask),
            f(m.no_bid),
            f(m.no_ask),
            m.volume.map_or("-".to_string(), |v| format!("{v:.0}")),
            m.status.as_deref().unwrap_or("-"),
            m.title
        );
        if let (Some(y), Some(n)) = (&m.yes_mint, &m.no_mint) {
            println!("{:<28} YES_MINT={y} NO_MINT={n}", "");
        }
    }
    println!("\n{} market(s)", list.len());
    Ok(())
}

fn spawn_keepalive(http: reqwest::Client, urls: Vec<String>) {
    tokio::spawn(async move {
        let mut t = tokio::time::interval(Duration::from_secs(20));
        t.tick().await;
        loop {
            t.tick().await;
            http::warm_up(&http, &urls).await;
        }
    });
}

fn init_logging(json: bool) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter).with_target(false);
    if json {
        builder.json().init();
    } else {
        builder.init();
    }
}
