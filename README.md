# Scalable Terminal

Native desktop trading terminal for the Scalable Broker. Rust + egui, single binary, no web view.

Backend is the official `sc` CLI (Scalable Agentic Investing). No browser automation, no session scraping, no reverse-engineered endpoints.

## Setup

1. Install the CLI:

       brew tap ScalableCapital/tap
       brew trust scalablecapital/tap
       brew install scalable-cli

2. Enable CLI access: Scalable web platform -> Profile -> Security -> Agentic Investing.

3. Log in (interactive OAuth device-code flow, must be done in a terminal):

       sc login

   Read-only session for evaluation: `sc login --local-read-only`

4. Run:

       cargo run --release

## Layout

- Top bar: session state, poll latency readout, poll interval, view tabs.
- Left: watchlist grid, synced with the watchlist on your Scalable account. Bid, ask, mid, intraday change, spread in basis points, stale-quote flag. Securities search adds straight to it.
- Right: account summary, positions with unrealized P&L marked against live mids, working orders with cancel, order ticket.
- Center: Portfolio / Chart / Log / Raw.

### Chart tab

- Mid-price line with the previous close as a dashed baseline, last price and change against it in the header.
- Timeframes `1d 7d 1m 3m 6m ytd 1y max`, plus a reload that bypasses the cache.
- Moving averages at 20, 50 and 200 **days**. A window longer than the loaded series is disabled, with the reason on hover, rather than drawn short.
- Charts are cached per instrument and timeframe, because this endpoint is rate limited.

### Portfolio tab

- Total, securities, cash, unrealized P&L as headline figures.
- Absolute return per timeframe, intraday through max.
- Holdings with portfolio weight, cost basis, P&L in currency and percent.
- Allocation by product type, asset class, equity sector and region, region broken down one level.
- Diversification scores per dimension.
- Stress scenarios: modelled move of your book against its benchmark.

## What it shows that the Scalable web app does not

- Spread in basis points per instrument, colour-coded. This is the execution cost the web app never states.
- Bid and ask side by side with one-click limit placement at bid, mid, or ask.
- Cost basis and unrealized P&L per position, marked against a live mid rather than an end-of-day snapshot.
- Portfolio weight per holding, next to the allocation and diversification analytics.
- Full pre-trade cost disclosure surfaced in the confirm dialog instead of buried behind links.
- Working orders and cancel in the same pane as the positions they offset.
- Every CLI call timed, so the real latency of the data path is visible rather than assumed.
- Raw JSON inspector for every endpoint.

## Verified against a live account

Read paths, the phase-1 disclosure paths and the latency numbers were all confirmed against a real session on 2026-09-17. The one path never exercised is phase 2: no order has been submitted through this app.

## Order flow

Orders follow the CLI's mandated two-phase contract:

1. Preview runs phase 1 with no `--confirm`. Returns a confirmation id plus the full pre-trade disclosure.
2. The confirm dialog renders the disclosure: shares, estimated volume, bid/ask/mid, spread, venue and status, entry/ongoing/exit costs, suitability, warnings, confirmation validity countdown.
3. Submit runs phase 2 with `--confirm <id>`. It is armed only when the user types CONFIRM, the instrument is tradable, the confirmation has not expired, and any unsuitability warning has been accepted.

Supported order types: market, limit, stop. Venue override is optional. Selling sizes by shares only, with `all` and `half` buttons that respect blocked quantity.

Local risk controls (`allowed_isins`, `denied_isins`, `max_order_notional`) belong in the CLI's `config.toml`. They are enforced by the CLI, not by this app.

## Known constraints

- No streaming. The CLI exposes no websocket or SSE transport, so quotes are polled with one `sc broker quote` process per ISIN, across a fixed pool of `QUOTE_FANOUT` (8) workers. Round wall-clock is shown in the top bar.

  Measured against a live session, 2026-09-17:

      1 ISIN                    160-230 ms
      9 ISINs sequential        2077 ms
      9 ISINs, 8 wide            491 ms
      27 ISINs, 8 wide           882 ms
      27 ISINs, 12 wide          769 ms

  About 33 ms per instrument amortised, no failures at any fan-out tried. Roughly 50 instruments per second-and-a-half round. Past 8 workers the gain is small: process spawn cost dominates, not the network.

- There IS a backend rate limit, and it is easy to hit. Eight `broker chart` calls in quick succession returned `rate_limited` on every one: `RATE_LIMITED: backend rate limit exceeded during BrokerChart`. It cleared after 46 seconds. Quotes proved far more tolerant — 27 in a burst, 12 wide, never tripped it.

  The app treats a rate limit as account-wide: any `rate_limited` response pauses all polling for 90 seconds, shows a banner with the countdown, and suppresses chart requests it knows would be refused. Charts are cached per instrument and timeframe so switching tabs and timeframes costs nothing.
- No market depth. Level 1 only.
- No bracket, OCO, or trailing orders. The CLI does not expose them.

- Working orders are read from `broker transactions` filtered to `status == PENDING`. The CLI exposes no list-orders command and `broker overview` carries no order list.
- Charts are mid-price ticks only. No OHLC, so no candlesticks. The previous close arrives separately as `closing_reference_point` and is drawn as a dashed baseline.
- The chart endpoint downsamples by span and never returns more than about 190 points. Measured on one instrument, 2026-09-17:

      timeframe   points   median gap   span
      1d             173       10 min    1.5 d
      7d             186       30 min    7.5 d
      1m             191        2 h     31.5 d
      3m              67        1 d     91.9 d
      6m             130        1 d    183.9 d
      ytd            182        1 d    257.9 d
      1y             127        2 d    363.9 d
      max            107       30 d   3212.9 d

  This is why the moving averages use calendar-day windows rather than observation counts. An n-point window would mean 3 hours on `1d` and 50 years on `max`, and a 200-point window would never exist on any timeframe. With day windows, 20/50/200 all draw on `ytd`, `1y` and `max`; on `1d` none of them do, and the buttons say so.
- No market depth. Level 1 only.
- Most endpoints nest their payload under `data.result`; `broker.chart` does not. `sc::result()` tolerates both.
- `blocked_quantity` does not account for resting sell orders. A position entirely committed to a working sell still reports `blocked: 0`, so free-to-sell quantity is computed by subtracting working sells from the position. Verified against a live account where all three holdings had resting sells and all three reported `blocked: 0`.
- `broker search` takes its query positionally, not as a flag. Chart timeframes are lowercase: `1d 7d 1m 3m 6m ytd 1y max`.
- The only reference close a quote exposes is inside `quote_performances`, under the `INTRADAY` timeframe. Intraday change is derived from it.

## Tests

    cargo test

Fifteen tests run the extractors against payloads captured from a live account, with account identifiers redacted, in `tests/fixtures/`. They pin the undocumented JSON shapes, so a change in the CLI surface fails a test rather than silently blanking a panel. They also check that securities plus cash reconcile to the reported total, that chart timestamps are monotonic, that allocation weights sum to one, that a lapsed confirmation disarms submit, and that free-to-sell quantity accounts for resting sells.

To refresh the fixtures after a CLI upgrade, re-run each `sc broker <cmd> --json` into `tests/fixtures/` and redact the ids.

## Files

- `src/sc.rs` - CLI wrapper, JSON envelope and exit-code handling, tolerant path extraction.
- `src/model.rs` - domain types built from `sc` payloads.
- `src/worker.rs` - background I/O thread, quote polling fan-out, trade phases.
- `src/app.rs` - egui UI.

Watchlist persists to `~/.config/scalable-terminal/watchlist.json`.
