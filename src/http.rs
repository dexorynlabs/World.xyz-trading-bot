//! Shared HTTP client tuned for low latency.
//!
//! One client (one connection pool) is shared by every component so TLS
//! sessions stay warm. Nagle is disabled, idle connections are kept, and
//! HTTP/2 pings keep long-lived connections from being reaped.

use std::time::Duration;

use anyhow::Result;
use reqwest::Client;

pub fn build_client(timeout_ms: u64) -> Result<Client> {
    Ok(Client::builder()
        .tcp_nodelay(true)
        .tcp_keepalive(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(300))
        .pool_max_idle_per_host(32)
        .http2_keep_alive_interval(Duration::from_secs(15))
        .http2_keep_alive_while_idle(true)
        .connect_timeout(Duration::from_millis(timeout_ms.min(3000)))
        .timeout(Duration::from_millis(timeout_ms))
        .user_agent(concat!("world-xyz-bot/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

/// Opens connections up-front so the first order doesn't pay DNS + TCP + TLS.
pub async fn warm_up(client: &Client, urls: &[String]) {
    let futs = urls.iter().map(|u| async move {
        let t = std::time::Instant::now();
        let res = client.head(u).send().await;
        tracing::debug!(url = %u, ok = res.is_ok(), ms = t.elapsed().as_millis() as u64, "warm-up");
    });
    futures::future::join_all(futs).await;
}
