# world.xyz Trading Bot (Rust template)

A latency-first starter bot for [World](https://world.xyz), the Solana prediction market.
Use it as a base for your own strategies: venue, execution, risk and strategy code live in separate layers, and every trading parameter is set in `.env`.

Out of the box it can:

- **buy**: one-shot market buy of YES or NO for `ORDER_SIZE_USD` of CASH
- **sell**: one-shot market sell of `SELL_SHARES` (or `all`)
- **strategy**: a threshold loop (enter below `ENTRY_PRICE`, exit at take-profit or stop-loss)
- **markets**: list and search the market catalog to find `MARKET_ID` and the YES/NO mints

> ⚠️ World has no official public trading API. The endpoints used here come from the World web app, via the third-party reverse-engineering in
> [chainstacklabs/world-xyz-research](https://github.com/chainstacklabs/world-xyz-research). Trades route through DFlow's `/order` API.
> Endpoints and response shapes can change without notice. Start with `DRY_RUN=true` and small sizes.

---

## How a trade flows

```
   strategy ──intent──► OrderClient GET /order ──► executable quote + unsigned tx   (1 round trip)
                                                        │
                            RiskManager.check(quote) ◄──┘   price / size / position / daily caps
                                                        │
                        Wallet.sign_wire_tx (in place, ~µs)
                                                        │
              SolanaRpc.sendTransaction ──► fan-out to every SOLANA_SEND_RPC_URLS (parallel)
                                                        │
      confirm loop: getSignatureStatuses + rebroadcast + blockhash-expiry guard
                                                        │
                  (executionMode=async) poll /order-status until closed
```

Key design decision: **the quote is the order.** `/order` is always called with `userPublicKey`, so the response already contains the built transaction.
When the strategy accepts a price, the bot signs and sends right away, with no second request.
Strategies decide on the executable price for their actual size, not on a displayed bid/ask that may be stale.

## Project layout

```
src/
├── main.rs          wiring, modes, logging
├── config.rs        typed .env loading + validation (all parameters)
├── types.rs         Side, Outcome, Instrument, Intent, Quote, Fill, Timings
├── http.rs          shared low-latency HTTP client, warm-up
├── wallet.rs        key loading + in-place ed25519 signing of wire txs
├── solana_rpc.rs    minimal JSON-RPC: send (fan-out), status, balances
├── execution.rs     quote → risk → sign → broadcast → confirm pipeline
├── risk.rs          pre-trade limits + failure circuit breaker
├── engine.rs        strategy event loop
├── strategy/
│   ├── mod.rs       Strategy trait
│   └── threshold.rs reference strategy
└── venue/
    ├── orders.rs    DFlow-compatible /order + /order-status
    └── markets.rs   World market catalog (discovery only)
```

## Latency choices

| Area | What the template does |
|---|---|
| Round trips | Quote and transaction come back in one `/order` call. There's no separate quote → build step. |
| Signing | The signature is patched straight into the serialized tx bytes with no solana-sdk deserialize/serialize. This takes microseconds and pulls in a much smaller dependency tree. |
| Connections | One shared pool with `TCP_NODELAY`, HTTP/2 keep-alive pings, and up to 32 idle connections per host. Every hot endpoint is warmed at startup and re-pinged every 20 s. |
| Broadcast | The signed tx goes to all RPCs in parallel with `skipPreflight=true` and `maxRetries=0`. The bot does its own rebroadcast every `REBROADCAST_INTERVAL_MS` as a fire-and-forget task. |
| Confirmation | `getSignatureStatuses` polling at `CONFIRM_POLL_MS`. It stops early on an on-chain error or an expired blockhash. |
| Hot path | Config is parsed and validated once. The market catalog is never touched while trading. |
| Build | `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`. |
| Observability | Every fill logs `quote / sign / send / confirm / total` timings. |

For production, run on a VPS close to your RPC and the order API (US-East is typical).
Use a paid RPC, and add staked or "sender" endpoints to `SOLANA_SEND_RPC_URLS`.

## Setup

```bash
cp .env.example .env            # then edit
cargo build --release
```

1. **Find a market.** Set `BOT_MODE=markets` and optionally `MARKET_SEARCH=bitcoin`, then run. It prints each `MARKET_ID` with its prices and YES/NO mints.
2. **Pick it.** Set `MARKET_ID=...` (resolved through the catalog at startup), or paste `YES_MINT` / `NO_MINT` directly for the fastest startup. Set `OUTCOME=yes|no`.
3. **Wallet.** Put a base58 secret key, a JSON byte array, or a keypair file path in `WALLET_PRIVATE_KEY`. Fund it with CASH, plus a little SOL for fees.
4. **Dry run.** With `DRY_RUN=true`, run `BOT_MODE=buy`. You get the real quote, risk check and signature, but nothing is broadcast.
5. **Go live.** Set `DRY_RUN=false`.

```bash
./target/release/world-xyz-bot          # reads .env
```

## Configuration reference

Every variable is documented in [.env.example](.env.example). Summary:

| Group | Variables |
|---|---|
| Mode | `BOT_MODE`, `DRY_RUN` |
| Wallet | `WALLET_PRIVATE_KEY` |
| Network | `SOLANA_RPC_URL`, `SOLANA_SEND_RPC_URLS`, `COMMITMENT`, `HTTP_TIMEOUT_MS` |
| Venue | `ORDER_API_URL`, `DFLOW_API_KEY`, `MARKETS_API_URL`, `HTTP_ORIGIN`, `COLLATERAL_MINT`, `COLLATERAL_DECIMALS`, `OUTCOME_DECIMALS` |
| Market | `MARKET_ID`, `YES_MINT`, `NO_MINT`, `OUTCOME`, `MARKET_SEARCH`, `MARKETS_LIST_LIMIT` |
| Order | `ORDER_SIZE_USD`, `SELL_SHARES`, `SLIPPAGE_BPS`, `PREDICTION_MARKET_SLIPPAGE_BPS`, `PRIORITY_FEE_LAMPORTS`, `MAX_BUY_PRICE`, `MIN_SELL_PRICE`, `EXTRA_ORDER_PARAMS` |
| Strategy | `ENTRY_PRICE`, `TAKE_PROFIT_PRICE`, `STOP_LOSS_PRICE`, `POLL_INTERVAL_MS`, `COOLDOWN_MS`, `POSITION_REFRESH_MS` |
| Risk | `MAX_ORDER_USD`, `MAX_POSITION_USD`, `MAX_TRADES_PER_DAY`, `MAX_CONSECUTIVE_FAILURES` |
| Execution | `CONFIRM_TIMEOUT_MS`, `CONFIRM_POLL_MS`, `REBROADCAST_INTERVAL_MS`, `SKIP_PREFLIGHT` |
| Logging | `RUST_LOG`, `LOG_JSON` |

`EXTRA_ORDER_PARAMS` (for example `predictionMarketInitPayer=<pubkey>`) is forwarded raw to `/order`, so you can follow venue API changes without rebuilding.

### Order API choice

| `ORDER_API_URL` | Notes |
|---|---|
| `https://aggregator-api-proxy.world-xyz.workers.dev` | The proxy World's web app uses. No key needed. |
| `https://quote-api.dflow.net` | DFlow production. Needs `DFLOW_API_KEY` (apply at DFlow). |
| `https://dev-quote-api.dflow.net` | DFlow dev. Keyless and rate-limited, for testing only. |

## Writing your own strategy

```rust
impl Strategy for MyStrategy {
    fn name(&self) -> &'static str { "my-strategy" }
    fn next_intent(&mut self, s: &MarketState) -> Option<Intent> { /* what to quote */ }
    fn should_execute(&mut self, s: &MarketState, q: &Quote) -> bool { /* accept this price? */ }
    fn on_fill(&mut self, f: &Fill) { /* update state */ }
}
```

Add it under `src/strategy/` and select it in `main.rs`. The risk checks and execution path stay the same.
To support another Solana venue, replace `src/venue/`. Nothing else depends on World-specific details.

## Testing

```bash
cargo test
```

Unit tests cover config parsing, unit conversion, wire-tx signing and verification, catalog parsing, risk limits, and strategy decisions.

## Building on Windows

With the `x86_64-pc-windows-gnu` toolchain you need a full MinGW-w64 on `PATH` (gcc + binutils). Two ways to get it:

- WinLibs: <https://winlibs.com>. Extract it and add `mingw64\bin` to `PATH`.
- MSVC instead: install *Visual Studio Build Tools* (C++), then run `rustup default stable-x86_64-pc-windows-msvc`.

On Linux and macOS, `cargo build --release` works as-is. Linux is the recommended deployment target.

## Notes & caveats

- **Fees**: the venue sets them per fill, and observed fees were roughly 0–8%+ of stake. The quote's `inAmount`/`outAmount` already include them, so the risk price checks run on the price you actually get.
- **Fill amounts** in logs are the quoted amounts. The real fill can differ within `SLIPPAGE_BPS`.
- **Settlement**: winning positions are paid out by the operator after resolution. There is no claim step.
- This is a template, not financial advice. Use at your own risk.
