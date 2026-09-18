# Scalable Terminal

A native desktop trading terminal for the Scalable Capital broker. Written in Rust with egui. One binary, no web view, no browser.

## Status: early and under heavy development

Read this part before you run anything.

This is a young project and it changes daily. Some of it is solid: the code that reads and interprets the broker's JSON is covered by tests that run against real captured payloads. Much of the rest is not. Panels have been checked by eye on one account, on one screen size, against one small portfolio of four positions. Error paths are mostly unexercised. Whole areas have been used once and declared fine.

Nobody has ever placed an order through this app. Not once. The preview half of the order flow works and is tested. The half that actually sends an order to the market has never run.

So treat it as a viewing tool that happens to have a ticket attached, not as trading software you would rely on. Check anything important against the official app before you act on it. If a number here disagrees with what Scalable Capital shows you, believe Scalable Capital and please report it.

## What it looks like

The chart view. Watchlist across the top, candles below, account and orders on the right.

![Chart view](docs/chart.png)

The portfolio view, with allocation, diversification scores and stress scenarios.

![Portfolio view](docs/portfolio.png)

The derivatives view, listing what is tradable on whichever instrument you last clicked.

![Derivatives view](docs/derivatives.png)

These were taken with the app's own screenshot mode, described near the end of this file. The account holder's name is replaced with a placeholder by the `--redact` flag.

## Goal

The Scalable Capital web and phone apps are built for buying an ETF once a month. They are not built for trading. You cannot see a bid and an ask at the same time. You cannot see what the spread is costing you. You cannot see cost basis next to a live price. You cannot see what is resting on the market next to the position it would close.

This project aims at the thing Interactive Brokers got right with Trader Workstation: one dense screen where price, position, risk and the order ticket are all visible at once, and where every number you need to make a decision is already on the screen rather than three taps away.

It will never be a full Trader Workstation, because the data behind it is thinner. What it can do is take everything Scalable Capital actually exposes and put it in front of you properly, instead of hiding it.

## How the backend works

There is no public REST API for Scalable Capital retail brokerage accounts. There is, since August 2026, an official agent interface called Agentic Investing. It ships as a command line tool called `sc` and as a hosted MCP server.

This terminal uses the `sc` command line tool as its entire backend. It does not scrape the website, it does not drive a browser, it does not reverse engineer private endpoints. Everything it shows came out of a command Scalable Capital publishes and supports.

Concretely:

* The app never talks to the network itself. It spawns `sc` as a child process, passes `--json`, and parses what comes back on standard output.
* `sc` handles the login. You run `sc login` once in a terminal, which does an OAuth device code flow in your browser. The session lives in the CLI, not in this app. This app never sees or stores your password.
* Every response is a JSON envelope shaped `{"ok": bool, "command": string, "data": ...}`. A logical failure such as an expired session arrives with `ok` set to false and a process exit code of zero, so failure is detected from the envelope rather than from the exit status.
* Almost every read command nests its real payload one level deeper, under `data.result`. The one exception found so far is `broker chart`, which puts the payload directly in `data`. The unwrapping helper tolerates both.
* All of this happens on a background thread. The user interface thread never waits on a subprocess.

The commands in use are `whoami`, `broker overview`, `broker cash-breakdown`, `broker holdings`, `broker transactions`, `broker analytics`, `broker watchlist`, `broker quote`, `broker chart`, `broker search`, `broker derivatives search`, `broker trade buy`, `broker trade sell` and `broker trade cancel`.

Two details about the CLI that shaped the design:

* Working orders have no command of their own. There is a cancel command but no list command, and the account overview carries no order list. Resting orders are therefore read out of `broker transactions` and filtered to the ones with status PENDING.
* Orders are deliberately two phase. The first call returns a confirmation id plus a full pre trade cost disclosure. The second call repeats the order with that confirmation id attached. The CLI publishes this as a contract and requires that the disclosure is shown to a human before the second call. The confirm dialog in this app is that contract rendered as a screen.

## Rate limits

This is the single biggest constraint on the design, and it is not documented anywhere, so it was measured.

Quotes are tolerant. Charts and derivative searches are not.

Quote polling, measured on 17 September 2026:

```
1 instrument                160 to 230 ms
9 instruments sequential          2077 ms
9 instruments, 8 in parallel       491 ms
27 instruments, 8 in parallel      882 ms
27 instruments, 12 in parallel     769 ms
```

That works out to roughly 33 ms per instrument once requests overlap. Around 50 instruments per round of a second and a half is comfortable. Going wider than 8 parallel requests buys very little, because the cost is dominated by starting a process rather than by the network. Nothing was ever refused at any width tried.

Charts are the opposite. Eight chart requests in quick succession were all refused:

```
RATE_LIMITED: backend rate limit exceeded during BrokerChart
```

The limit cleared after 46 seconds. Derivative searches refuse in the same way with `BrokerDerivativesSearch` in the message.

How the app handles this:

* Any refusal is treated as applying to the whole account, not just to the endpoint that tripped it. Hammering a second endpoint after tripping the first only makes things worse.
* On a refusal all polling stops for 90 seconds. The measured recovery was 46 seconds, so the pause is deliberately longer than what was observed.
* A countdown appears in the top bar. It keeps ticking on its own, because during a pause nothing completes and nothing would otherwise trigger a redraw.
* A chart request made during a pause is not sent. It is queued, and it is issued for real once the pause ends.
* Charts are cached per instrument and per timeframe. Derivative searches are cached per underlying, family and direction. Flipping between tabs and timeframes costs nothing after the first load.

Practical advice: if you widen the poll interval in the top bar and the latency readout stays flat, you have room. If you start seeing the pause banner, you are asking for charts faster than the backend will serve them.

## Install and run

Do these steps in order. The terminal is only a front end, so if the CLI underneath it is not working, the terminal will show you empty panels and you will have no idea why. Prove the CLI works first.

### Step 1. Turn on AI trading in your Scalable Capital account

Log in to the Scalable Capital web platform. Go to Profile, then Security, then Agentic Investing. Enable it.

This is the feature that lets an external program act on your account at all. It is the same switch that lets ChatGPT or Claude connect to your broker. Without it every command below fails with an authentication error, and there is nothing this app can do about that.

### Step 2. Install the CLI

```
brew tap ScalableCapital/tap
brew trust scalablecapital/tap
brew install scalable-cli
```

Homebrew will refuse to install from a third party tap until you trust it, which is why the middle line is there.

### Step 3. Log in

```
sc login
```

This is interactive. It opens a browser, you approve a device code, and the session is stored by the CLI. It has to be done in a real terminal. The app cannot do it for you and never sees your credentials.

If you only want to look around, with no ability to place an order at all, use this instead:

```
sc login --local-read-only
```

That blocks every write command until you log in again without the flag. It is a good way to try the terminal for the first time.

### Step 4. Try the CLI by hand before starting the terminal

This step is not optional if you want to save yourself confusion. Run these and confirm each one prints real data:

```
sc whoami --json
sc broker overview --json
sc broker holdings --json
sc broker watchlist --json
sc broker quote --isin <one of your ISINs> --json
sc broker chart --isin <one of your ISINs> --timeframe 1d --json
```

What you are checking:

* Every response starts with `"ok":true`. If it says `"ok":false` with `no_session`, go back to step 3. If it says `rate_limited`, wait a minute and try again.
* `holdings` and `watchlist` list the things you expect to see.
* `quote` returns a bid and an ask, not just a mid.

If `sc` works and the terminal does not, the problem is this app and worth reporting. If `sc` itself is failing, the terminal cannot help you, and the fix is with the CLI or your account settings.

It is also worth spending a few minutes just reading `sc --help` and `sc broker --help`. Everything this terminal can do is something the CLI can do, so knowing the CLI tells you exactly where the ceiling is.

### Step 5. Run the terminal

```
cargo run --release
```

## Underlying technology

Nothing exotic is involved. The parts are:

Rust, as the language. The binary is self contained, starts instantly, and has no runtime to install.

egui, as the user interface. It is an immediate mode toolkit, which means the whole screen is described from scratch on every frame rather than kept as a tree of widget objects that have to be mutated and kept in sync. For a screen that is mostly dense tables of numbers changing several times a second, that model is a good fit and it keeps the code direct: the table you see on screen is a loop over the current data, not a set of update callbacks. eframe is the shell around it that opens the window, and rendering goes through wgpu to the GPU. egui_plot draws the chart.

The Scalable CLI, as the entire data layer, described in the previous section. Worth repeating that this app has no HTTP client, no credentials, no API keys and no persistent storage of account data. It starts a subprocess and reads JSON.

serde and serde_json, for parsing those responses.

Plain standard library threads and channels for concurrency. There is no async runtime. The user interface runs on the main thread, one background thread owns all input and output, and they talk over a channel plus a shared mutex protected state struct. When quotes are fetched, that background thread opens a short lived pool of eight more threads which pull instruments from a shared counter until the list is done. This is deliberately simple. The work is slow subprocess calls rather than thousands of sockets, so an async runtime would add machinery without buying anything.

The direct dependency list is seven crates: eframe, egui, egui_extras, egui_plot, serde, serde_json, and image, the last of which exists only for the screenshot mode described below. That pulls in around 186 crates once the graphics stack underneath egui is counted, which is what a GPU rendered window costs on any toolchain.

## Screenshot mode

The app can photograph itself:

```
cargo run --release -- --screenshot out.png 8 --tab chart
```

It opens, waits the given number of seconds so the data has arrived, writes a PNG and exits. The optional `--tab` picks the view, one of chart, derivatives, portfolio, log or raw. The optional `--select` picks the instrument, so a screenshot is reproducible rather than dependent on whichever holding happened to sort first.

`--redact` replaces the account holder's name with a placeholder. It works outside screenshot mode too, so it is also useful when sharing your screen. Note that it hides the name and nothing else: balances, positions and resting orders all remain visible, so look at an image before publishing it.

The screenshots at the top of this file were produced with:

```
cargo run --release -- --redact --screenshot docs/chart.png 10 --tab chart --select <isin>
```

This exists because the app cannot be brought to the foreground from a script on macOS, so an ordinary screen capture photographs whatever window happens to be in front instead. It was built to check layout changes, and it earned its place immediately by revealing that the panels were badly proportioned and that a column of numbers was wrapping vertically. It is a development tool, not a feature, but it is genuinely useful if you want to see what a change did.

## What is on the screen

Top bar: who is logged in, how long the last quote round took and how many instruments it covered, the poll interval, refresh buttons, the rate limit countdown when one is active, and the view tabs.

Watchlist strip across the top: this is your real Scalable Capital watchlist. Adding and removing here changes the account, so the terminal and your phone stay in agreement. Columns are bid, ask, mid, intraday change and spread in basis points. The spread is colour coded, because it is the number that decides whether a trade is worth doing and the official apps never show it. A marker flags any quote the broker itself considers stale.

Right hand column: cash and buying power, positions with cost basis and unrealised profit marked against a live mid, working orders with a cancel button next to each, and the order ticket.

Main area, one of five views:

* Chart. Candles or a line, with the previous close drawn as a dashed baseline. Timeframes are 1d, 7d, 1m, 3m, 6m, ytd, 1y and max. Moving averages at 20, 50 and 200 days.
* Derivatives. Every knockout, factor certificate and warrant tradable on whatever instrument you last clicked. Filter by family and by direction. Columns include leverage, strike, knockout barrier and distance to the knockout barrier, which is colour coded as a survival margin. Clicking a row makes that derivative the active instrument for the chart and the ticket.
* Portfolio. Total, securities and cash, return per timeframe, holdings with portfolio weight and profit, allocation by product type, asset class, sector and region, diversification scores, and modelled stress scenarios against a benchmark.
* Log. Every `sc` invocation, timed. This is how you see what the backend is actually doing and how long it takes.
* Raw. The unmodified JSON behind each endpoint.

## Placing an order

Selling sizes by shares only. There are all and half buttons, and they are computed from the shares that are genuinely free.

That distinction matters. The broker reports `blocked_quantity` as zero even for a position that is entirely committed to a resting sell order. Sizing a sell from quantity minus blocked would therefore offer shares that are already on the market. This app subtracts resting sells itself, shows the free amount, and disables the all and half buttons when nothing is free.

Preview runs the first phase. The dialog then shows everything the disclosure contains: share count, estimated volume, bid, ask, mid, spread in basis points, venue and its status, entry, ongoing and exit costs, suitability, any warning, and how many seconds the confirmation remains valid.

Submit runs the second phase. It only becomes clickable when you have typed CONFIRM, the instrument is tradable, the confirmation has not expired, and any unsuitability warning has been accepted. When the validity countdown reaches zero the button disarms itself rather than letting the order fail at the broker.

Order types are market, limit and stop. Venue can be overridden. There are no bracket or OCO orders, because the CLI does not offer them.

## Trailing stops

The CLI has no trailing order type, no amend command, and nothing resembling OCO. So a trailing stop cannot be handed to the broker. It can only be imitated from here: watch the price, and when it rises far enough, cancel the resting stop and place a new one higher.

This app does the watching and the arithmetic, and then asks you before it moves anything.

Arm a trail on a position you hold, as a percentage or as an absolute amount. From then on the app tracks the high water mark, which only ever rises, and works out where the stop should sit. When the resting stop falls meaningfully behind, a button appears offering to move it up. Until you press that button nothing happens to your orders.

Pressing it cancels the resting stop and previews the replacement. The usual confirm dialog appears, with full costs and the validity countdown, and you complete it the same way as any other order.

Three things to understand before using it:

The position is unprotected in the middle. Your shares are committed to the resting stop, so the old order has to be cancelled before a replacement can even be previewed. That ordering is forced by the broker. Between the cancel and your confirmation there is no stop on the position, and the app says so in red for as long as that is true.

It only follows while the app is open, and only as finely as it polls. Close the terminal and your stop simply stays where it was last placed, still protecting you at that level, no longer tracking.

It will not chase small moves. A replacement costs three calls and opens that unprotected window, so a ratchet is only offered once the improvement is worth at least a tenth of a percent.

The high water mark is written to `~/.config/scalable-terminal/trails.json`, because it is the one piece of a trail that cannot be recovered from the broker. Everything else, the resting stop and the share count, is read back from live account state on every refresh, so an order you cancel or move elsewhere is picked up rather than quietly disagreed with.

If you want a hard ceiling on order size, set `max_order_notional`, `allowed_isins` and `denied_isins` in the CLI's own `config.toml`. Those are enforced by the CLI itself, which is a better place for a risk limit than this app.

## What the data cannot do

No streaming. The CLI has no websocket and no server sent events, so prices are polled. One process per instrument, eight at a time. The round time is always on screen so you can see the cost rather than guess at it.

No market depth. Level one only, a single bid and a single ask.

No OHLC. The chart endpoint returns mid price ticks and nothing else, so the candles in this app are built here, by bucketing those ticks into time intervals. Open and close are the first and last tick in a bucket, high and low are its extremes. Empty buckets are skipped rather than carried forward, so a gap in the data stays visibly a gap instead of turning into a flat bar that never traded. They are real candles derived from real ticks, but they are derived, and the chart says so.

The chart endpoint also downsamples according to how long a span you ask for, and never returns more than about 190 points:

```
timeframe   points   typical gap   span
1d             173        10 min     1.5 days
7d             186        30 min     7.5 days
1m             191         2 hours   31.5 days
3m              67         1 day     91.9 days
6m             130         1 day    183.9 days
ytd            182         1 day    257.9 days
1y             127         2 days   363.9 days
max            107        30 days   3212.9 days
```

This is why the moving averages are measured in calendar days rather than in bars. A twenty bar average would mean three hours on the 1d view and fifty years on the max view, and a two hundred bar window would not exist on any timeframe at all. Measured in days, 20, 50 and 200 all mean what a trader expects them to mean. A window longer than the loaded data is refused rather than drawn short, and the button explains why when you hover it. All three averages are available on ytd, 1y and max. None of them are available on 1d, which is correct, because a single session does not contain a twenty day average.

## Tests

```
cargo test
```

Twenty five tests. They run the extractors against real payloads captured from a live account, with account identifiers removed, stored in `tests/fixtures`.

The point of them is that the JSON shapes are undocumented and can change without warning. If a future CLI release renames a field, a test fails instead of a panel quietly going blank.

They check more than field names. Securities plus cash must reconcile to the reported total. Chart timestamps must increase. Allocation weights must sum to one. Candle aggregation must satisfy the usual open, high, low, close relationships and must account for every tick exactly once. A moving average must match a mean computed directly. An expired confirmation must disarm the submit button. Free to sell quantity must subtract resting sells.

To refresh the fixtures after upgrading the CLI, run each `sc broker <command> --json` again into `tests/fixtures` and strip the account and portfolio identifiers.

## Layout of the code

* `src/sc.rs` wraps the command line tool. Envelope parsing, exit codes, rate limit classification, tolerant path lookup.
* `src/model.rs` turns payloads into typed values, and holds the derived logic: candle aggregation, moving averages, free to sell quantity.
* `src/worker.rs` is the background thread. Quote fan out, chart and derivative caches, the rate limit pause, and the two phase order flow.
* `src/app.rs` is the interface.
* `src/tests.rs` holds the tests described above.

## Where it stands

What has genuinely been verified against a live account:

* Every read shape. All the JSON parsing was written against captured real responses, not guessed, and the tests hold it in place.
* Quote polling and its timings, which is where the numbers earlier in this file come from.
* The rate limit behaviour, including how long a refusal actually lasts.
* The first phase of the order flow, which returns the disclosure and a confirmation id.
* The candle aggregation and the moving averages, by calculation rather than by eye.

What has not:

* Placing an order. The second phase has never run.
* Cancelling an order.
* Adding and removing watchlist entries beyond a couple of tries.
* Anything on an account that looks different from the one it was built against. Four positions, one currency, no crypto, no savings plans, no derivatives held.
* Error handling in general. Sessions expiring mid use, instruments that stop trading, malformed responses. These are handled in code and almost none of it has been provoked for real.

Expect rough edges and expect things to move. If something looks wrong, it may well be wrong.
