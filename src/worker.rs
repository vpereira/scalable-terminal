//! Background I/O. The UI thread never blocks on `sc`.
//!
//! `sc` has no streaming transport, so live prices are polled. Every round is
//! timed and surfaced in the UI: the polling ceiling is the real constraint on
//! this design and it should be visible, not assumed.

use crate::model::*;
use crate::sc;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const QUOTE_FANOUT: usize = 8;
pub const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(90);
pub const TIMEFRAMES: [&str; 8] = ["1d", "7d", "1m", "3m", "6m", "ytd", "1y", "max"];

#[derive(Debug, Clone)]
pub enum Cmd {
    RefreshAll,
    RefreshQuotes,
    WatchlistAdd(String),
    WatchlistRemove(String),
    LoadChart { isin: String, timeframe: String, force: bool },
    LoadDerivatives { underlying: String, dtype: String, strategy: String },
    Search(String),
    PreviewTrade(TradeIntent),
    SubmitTrade { intent: TradeIntent, confirmation_id: String, accept_unsuitable: bool },
    CancelOrder(String),
    ClearPreview,
    Shutdown,
}

#[derive(Debug, Clone)]
pub struct TradeIntent {
    pub isin: String,
    pub side: Side,
    pub order_type: OrderType,
    /// Buy may size by cash amount; sell is always by shares.
    pub amount: Option<f64>,
    pub shares: Option<f64>,
    pub limit_price: Option<f64>,
    pub stop_price: Option<f64>,
    pub venue: Option<String>,
}

impl TradeIntent {
    fn argv(&self, confirm: Option<&str>, accept_unsuitable: bool) -> Vec<String> {
        let mut a: Vec<String> = vec!["broker".into(), "trade".into(), self.side.cmd().into()];
        a.push("--isin".into());
        a.push(self.isin.clone());
        if let Some(sh) = self.shares {
            a.push("--shares".into());
            a.push(fmt_num(sh));
        } else if let Some(am) = self.amount {
            a.push("--amount".into());
            a.push(fmt_num(am));
        }
        a.push("--order-type".into());
        a.push(self.order_type.cmd().into());
        if let Some(p) = self.limit_price {
            a.push("--limit-price".into());
            a.push(fmt_num(p));
        }
        if let Some(p) = self.stop_price {
            a.push("--stop-price".into());
            a.push(fmt_num(p));
        }
        if let Some(v) = &self.venue {
            if !v.trim().is_empty() {
                a.push("--venue".into());
                a.push(v.clone());
            }
        }
        if let Some(id) = confirm {
            a.push("--confirm".into());
            a.push(id.to_string());
            if accept_unsuitable {
                a.push("--accept-unsuitable".into());
            }
        }
        a
    }
}

fn fmt_num(x: f64) -> String {
    if x.fract().abs() < 1e-9 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

#[derive(Debug, Clone)]
pub struct CallLog {
    pub cmd: String,
    pub ms: u128,
    pub ok: bool,
    pub detail: String,
}

#[derive(Default)]
pub struct Shared {
    pub session: Option<String>,
    pub session_error: Option<String>,
    pub watchlist: Vec<String>,
    pub quotes: HashMap<String, Quote>,
    pub holdings: Vec<Holding>,
    pub orders: Vec<PendingOrder>,
    pub account: Account,
    pub analytics: Analytics,
    pub overview_raw: Value,
    pub holdings_raw: Value,
    pub chart: Chart,
    pub chart_raw: Value,
    pub chart_error: Option<String>,
    pub chart_loading: bool,
    /// Charts are expensive and rate limited; keep what we have fetched.
    pub chart_cache: HashMap<(String, String), Chart>,
    pub derivatives: DerivativesPage,
    pub derivatives_error: Option<String>,
    pub derivatives_loading: bool,
    pub derivatives_cache: HashMap<(String, String, String), DerivativesPage>,
    /// Set when the backend rate-limits us. All polling pauses until it passes.
    pub backoff_until: Option<Instant>,
    /// A chart request refused during backoff, re-issued once it lapses.
    pub deferred_chart: Option<(String, String)>,
    pub search_results: Vec<Value>,
    pub preview: Option<TradePreview>,
    pub preview_error: Option<String>,
    pub preview_pending: bool,
    pub order_result: Option<String>,
    pub order_error: Option<String>,
    pub order_pending: bool,
    pub log: Vec<CallLog>,
    pub last_round_ms: u128,
    pub last_round_calls: usize,
    pub quote_ms_avg: f64,
}

impl Shared {
    pub fn backoff_secs_left(&self) -> Option<u64> {
        let until = self.backoff_until?;
        let left = until.saturating_duration_since(Instant::now());
        (!left.is_zero()).then(|| left.as_secs() + 1)
    }

    /// The backend rate-limits per endpoint; treat a hit as account-wide and
    /// stand down, because hammering a second endpoint will trip that one too.
    fn note_rate_limit(&mut self, from: &str) {
        self.backoff_until = Some(Instant::now() + RATE_LIMIT_BACKOFF);
        self.push_log(
            "rate limited",
            0,
            false,
            format!("{from}: backing off {}s", RATE_LIMIT_BACKOFF.as_secs()),
        );
    }

    fn push_log(&mut self, cmd: &str, ms: u128, ok: bool, detail: impl Into<String>) {
        self.log.push(CallLog { cmd: cmd.to_string(), ms, ok, detail: detail.into() });
        if self.log.len() > 400 {
            let drop = self.log.len() - 400;
            self.log.drain(0..drop);
        }
    }
}

pub struct Handle {
    pub tx: Sender<Cmd>,
    pub state: Arc<Mutex<Shared>>,
}

pub fn spawn(ctx: egui::Context, poll: Arc<Mutex<Duration>>) -> Handle {
    let (tx, rx) = std::sync::mpsc::channel::<Cmd>();
    let state = Arc::new(Mutex::new(Shared::default()));
    let st = state.clone();

    std::thread::Builder::new()
        .name("sc-io".into())
        .spawn(move || worker_loop(rx, st, ctx, poll))
        .expect("spawn worker");

    Handle { tx, state }
}

fn worker_loop(
    rx: Receiver<Cmd>,
    state: Arc<Mutex<Shared>>,
    ctx: egui::Context,
    poll: Arc<Mutex<Duration>>,
) {
    check_session(&state);
    ctx.request_repaint();

    let mut next_poll = Instant::now();

    loop {
        let wait = next_poll
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(250));

        match rx.recv_timeout(wait) {
            Ok(Cmd::Shutdown) => return,
            Ok(cmd) => {
                handle(&state, cmd);
                ctx.request_repaint();
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }

        // Once the backoff lapses, make good on the retry we promised.
        let retry = {
            let mut s = state.lock().unwrap();
            if s.backoff_secs_left().is_none() {
                if s.backoff_until.take().is_some() {
                    s.push_log("rate limit", 0, true, "backoff cleared, resuming");
                }
                s.deferred_chart.take()
            } else {
                None
            }
        };
        if let Some((isin, timeframe)) = retry {
            load_chart(&state, &isin, &timeframe, true);
            ctx.request_repaint();
        }

        if Instant::now() >= next_poll {
            let interval = *poll.lock().unwrap();
            let held_off = state.lock().unwrap().backoff_secs_left().is_some();
            if interval > Duration::ZERO && !held_off {
                let list = state.lock().unwrap().watchlist_plus_holdings();
                if !list.is_empty() {
                    refresh_quotes(&state, &list);
                    ctx.request_repaint();
                }
            }
            next_poll = Instant::now()
                + if interval > Duration::ZERO { interval } else { Duration::from_secs(3600) };
        }
    }
}

impl Shared {
    /// Poll everything on screen: the watchlist plus anything held, since
    /// positions need a live mid to mark P&L.
    fn watchlist_plus_holdings(&self) -> Vec<String> {
        let mut v = self.watchlist.clone();
        for h in &self.holdings {
            if !h.isin.is_empty() && !v.contains(&h.isin) {
                v.push(h.isin.clone());
            }
        }
        v
    }
}

fn handle(state: &Arc<Mutex<Shared>>, cmd: Cmd) {
    match cmd {
        Cmd::RefreshAll => {
            refresh_account(state);
            refresh_watchlist(state);
            let list = state.lock().unwrap().watchlist_plus_holdings();
            refresh_quotes(state, &list);
        }
        Cmd::RefreshQuotes => {
            let list = state.lock().unwrap().watchlist_plus_holdings();
            refresh_quotes(state, &list);
        }
        Cmd::WatchlistAdd(isin) => {
            let call = sc::run(&["broker", "watchlist", "add", "--isin", &isin]);
            log_call(state, "watchlist.add", &call, &isin);
            refresh_watchlist(state);
            refresh_quotes(state, &[isin]);
        }
        Cmd::WatchlistRemove(isin) => {
            let call = sc::run(&["broker", "watchlist", "remove", "--isin", &isin]);
            log_call(state, "watchlist.remove", &call, &isin);
            refresh_watchlist(state);
        }
        Cmd::LoadChart { isin, timeframe, force } => load_chart(state, &isin, &timeframe, force),
        Cmd::LoadDerivatives { underlying, dtype, strategy } => {
            load_derivatives(state, &underlying, &dtype, &strategy)
        }
        Cmd::Search(q) => do_search(state, &q),
        Cmd::PreviewTrade(intent) => do_preview(state, &intent),
        Cmd::SubmitTrade { intent, confirmation_id, accept_unsuitable } => {
            do_submit(state, &intent, &confirmation_id, accept_unsuitable);
            refresh_account(state);
        }
        Cmd::CancelOrder(id) => {
            let call = sc::run(&["broker", "trade", "cancel", "--order-id", &id]);
            {
                let mut s = state.lock().unwrap();
                match &call.data {
                    Ok(_) => s.order_result = Some(format!("cancelled {id}")),
                    Err(e) => s.order_error = Some(e.to_string()),
                }
            }
            log_call(state, "trade.cancel", &call, &id);
            refresh_account(state);
        }
        Cmd::ClearPreview => {
            let mut s = state.lock().unwrap();
            s.preview = None;
            s.preview_error = None;
            s.order_error = None;
            s.order_result = None;
        }
        Cmd::Shutdown => {}
    }
}

fn log_call(state: &Arc<Mutex<Shared>>, name: &str, call: &sc::Call, ctxinfo: &str) {
    let mut s = state.lock().unwrap();
    match &call.data {
        Ok(_) => s.push_log(name, call.elapsed.as_millis(), true, ctxinfo.to_string()),
        Err(e) => s.push_log(name, call.elapsed.as_millis(), false, format!("{ctxinfo}: {e}")),
    }
}

fn check_session(state: &Arc<Mutex<Shared>>) {
    let call = sc::run(&["whoami"]);
    let mut s = state.lock().unwrap();
    match &call.data {
        Ok(v) => {
            let r = sc::result(v);
            let first = sc::str_at(r, &["personOverview/personalDetails/firstName"]).unwrap_or_default();
            let last = sc::str_at(r, &["personOverview/personalDetails/lastName"]).unwrap_or_default();
            let name = format!("{first} {last}").trim().to_string();
            s.session = Some(if name.is_empty() { "authenticated".into() } else { name });
            s.session_error = None;
            s.push_log("whoami", call.elapsed.as_millis(), true, "session ok");
        }
        Err(e) => {
            s.session = None;
            s.session_error = Some(format!("{e}"));
            s.push_log("whoami", call.elapsed.as_millis(), false, e.to_string());
        }
    }
}

fn refresh_account(state: &Arc<Mutex<Shared>>) {
    // Five independent endpoints. Serially that is ~1 s on every refresh, and a
    // refresh follows every submit and cancel — so fan them out.
    let (mut ov, mut cash, mut hd, mut tx, mut an) = (None, None, None, None, None);
    std::thread::scope(|scope| {
        let a = scope.spawn(|| sc::run(&["broker", "overview"]));
        let b = scope.spawn(|| sc::run(&["broker", "cash-breakdown"]));
        let c = scope.spawn(|| sc::run(&["broker", "holdings"]));
        let d = scope.spawn(|| sc::run(&["broker", "transactions"]));
        let e = scope.spawn(|| sc::run(&["broker", "analytics"]));
        ov = a.join().ok();
        cash = b.join().ok();
        hd = c.join().ok();
        tx = d.join().ok();
        an = e.join().ok();
    });
    let (Some(ov), Some(cash), Some(hd), Some(tx), Some(an)) = (ov, cash, hd, tx, an) else {
        state.lock().unwrap().push_log("refresh", 0, false, "an sc call panicked");
        return;
    };

    let mut s = state.lock().unwrap();

    match &ov.data {
        Ok(v) => {
            s.account.apply_overview(sc::result(v));
            s.overview_raw = v.clone();
            s.push_log("broker.overview", ov.elapsed.as_millis(), true, "");
        }
        Err(e) => s.push_log("broker.overview", ov.elapsed.as_millis(), false, e.to_string()),
    }
    match &cash.data {
        Ok(v) => {
            s.account.apply_cash(sc::result(v));
            s.push_log("broker.cash-breakdown", cash.elapsed.as_millis(), true, "");
        }
        Err(e) => s.push_log("broker.cash-breakdown", cash.elapsed.as_millis(), false, e.to_string()),
    }
    match &hd.data {
        Ok(v) => {
            s.holdings = Holding::list_from(sc::result(v));
            s.holdings_raw = v.clone();
            let n = s.holdings.len();
            s.push_log("broker.holdings", hd.elapsed.as_millis(), true, format!("{n} positions"));
        }
        Err(e) => s.push_log("broker.holdings", hd.elapsed.as_millis(), false, e.to_string()),
    }
    match &tx.data {
        Ok(v) => {
            s.orders = PendingOrder::pending_from_transactions(sc::result(v));
            let n = s.orders.len();
            s.push_log("broker.transactions", tx.elapsed.as_millis(), true, format!("{n} working"));
        }
        Err(e) => s.push_log("broker.transactions", tx.elapsed.as_millis(), false, e.to_string()),
    }
    match &an.data {
        Ok(v) => {
            s.analytics = Analytics::from_json(sc::result(v));
            s.push_log("broker.analytics", an.elapsed.as_millis(), true, "");
        }
        Err(e) => s.push_log("broker.analytics", an.elapsed.as_millis(), false, e.to_string()),
    }
}

/// The broker's own watchlist is the source of truth, so the terminal stays in
/// sync with the phone and web app instead of keeping a private list.
fn refresh_watchlist(state: &Arc<Mutex<Shared>>) {
    let call = sc::run(&["broker", "watchlist"]);
    let mut s = state.lock().unwrap();
    match &call.data {
        Ok(v) => {
            let r = sc::result(v);
            let items = sc::pick(r, &["items"]).and_then(Value::as_array).cloned().unwrap_or_default();
            s.watchlist = items
                .iter()
                .filter_map(|i| sc::str_at(i, &["isin"]))
                .collect();
            // Seed rows with the mid the watchlist already carries; the quote
            // round fills in bid/ask a moment later.
            for i in &items {
                let q = Quote::from_watchlist_item(i);
                if q.isin.is_empty() {
                    continue;
                }
                s.quotes.entry(q.isin.clone()).or_insert(q);
            }
            let n = s.watchlist.len();
            s.push_log("broker.watchlist", call.elapsed.as_millis(), true, format!("{n} items"));
        }
        Err(e) => s.push_log("broker.watchlist", call.elapsed.as_millis(), false, e.to_string()),
    }
}

/// Fan out one `sc broker quote` per ISIN across a fixed worker pool.
/// This is the polling ceiling: round wall-clock ~= (n / QUOTE_FANOUT) * per_call.
fn refresh_quotes(state: &Arc<Mutex<Shared>>, isins: &[String]) {
    if isins.is_empty() {
        return;
    }
    let started = Instant::now();
    let results: Arc<Mutex<Vec<(String, Option<Quote>, u128, Option<String>)>>> =
        Arc::new(Mutex::new(Vec::new()));

    // One pool of QUOTE_FANOUT workers pulling from a shared cursor. Chunking with
    // a scope per chunk would join at every chunk boundary and serialise the round
    // on each chunk's slowest call, which would make the latency readout lie.
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..QUOTE_FANOUT.min(isins.len()) {
            let next = &next;
            let results = results.clone();
            scope.spawn(move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= isins.len() {
                    break;
                }
                let isin = &isins[i];
                let call = sc::run(&["broker", "quote", "--isin", isin]);
                let ms = call.elapsed.as_millis();
                let entry = match &call.data {
                    Ok(v) => (isin.clone(), Some(Quote::from_json(isin, sc::result(v))), ms, None),
                    Err(e) => (isin.clone(), None, ms, Some(e.to_string())),
                };
                results.lock().unwrap().push(entry);
            });
        }
    });

    let round_ms = started.elapsed().as_millis();
    let collected = std::mem::take(&mut *results.lock().unwrap());
    let mut s = state.lock().unwrap();
    let mut total_ms = 0u128;
    let mut failures = 0usize;
    let mut limited = false;
    for (isin, q, ms, err) in collected.iter() {
        total_ms += ms;
        match q {
            Some(q) => {
                s.quotes.insert(isin.clone(), q.clone());
            }
            None => {
                failures += 1;
                if let Some(e) = err {
                    if e.contains("rate_limited") {
                        limited = true;
                    }
                    s.push_log("broker.quote", *ms, false, format!("{isin}: {e}"));
                }
            }
        }
    }
    let calls = collected.len();
    let avg = total_ms as f64 / calls.max(1) as f64;
    s.quote_ms_avg = avg;
    s.last_round_ms = round_ms;
    s.last_round_calls = calls;
    s.push_log(
        "quote round",
        round_ms,
        failures == 0,
        format!("{calls} isins, {failures} failed, {avg:.0} ms/call"),
    );
    if limited {
        s.note_rate_limit("broker.quote");
    }
}

fn load_chart(state: &Arc<Mutex<Shared>>, isin: &str, timeframe: &str, force: bool) {
    let key = (isin.to_string(), timeframe.to_string());
    {
        let mut s = state.lock().unwrap();
        if !force {
            if let Some(c) = s.chart_cache.get(&key) {
                s.chart = c.clone();
                s.chart_error = None;
                return;
            }
        }
        // Do not spend a request we know will be refused — queue it instead, so
        // the retry actually happens rather than the message merely promising it.
        if let Some(left) = s.backoff_secs_left() {
            s.chart_error = Some(format!("rate limited — retrying in {left}s"));
            s.deferred_chart = Some(key);
            return;
        }
        s.chart_loading = true;
        s.chart_error = None;
    }

    let call = sc::run(&["broker", "chart", "--isin", isin, "--timeframe", timeframe]);
    let mut s = state.lock().unwrap();
    s.chart_loading = false;
    match &call.data {
        Ok(v) => {
            let chart = Chart::from_json(sc::result(v));
            let n = chart.points.len();
            s.chart_cache.insert(key, chart.clone());
            s.chart = chart;
            s.chart_raw = v.clone();
            s.chart_error = None;
            s.push_log("broker.chart", call.elapsed.as_millis(), true, format!("{isin} {timeframe}: {n} pts"));
        }
        Err(e) => {
            if e.kind == sc::ScErrorKind::RateLimited {
                s.note_rate_limit("broker.chart");
                s.chart_error = Some(format!("rate limited — retrying in {}s", RATE_LIMIT_BACKOFF.as_secs()));
                s.deferred_chart = Some((isin.to_string(), timeframe.to_string()));
            } else {
                s.chart_error = Some(e.to_string());
            }
            s.chart = Chart { isin: isin.to_string(), timeframe: timeframe.to_string(), ..Default::default() };
            s.push_log("broker.chart", call.elapsed.as_millis(), false, format!("{}: {e}", call.cmdline()));
        }
    }
}

/// Same discipline as charts: this endpoint rate-limits readily, so cache per
/// (underlying, type, strategy) and refuse a request during a backoff.
fn load_derivatives(state: &Arc<Mutex<Shared>>, underlying: &str, dtype: &str, strategy: &str) {
    let key = (underlying.to_string(), dtype.to_string(), strategy.to_string());
    {
        let mut s = state.lock().unwrap();
        if let Some(p) = s.derivatives_cache.get(&key) {
            s.derivatives = p.clone();
            s.derivatives_error = None;
            return;
        }
        if let Some(left) = s.backoff_secs_left() {
            s.derivatives_error = Some(format!("rate limited — retrying in {left}s"));
            return;
        }
        s.derivatives_loading = true;
        s.derivatives_error = None;
    }

    let call = sc::run(&[
        "broker", "derivatives", "search",
        "--underlying", underlying,
        "--type", dtype,
        "--strategy", strategy,
        "--limit", "50",
    ]);
    let mut s = state.lock().unwrap();
    s.derivatives_loading = false;
    match &call.data {
        Ok(v) => {
            let mut page = DerivativesPage::from_json(sc::result(v));
            if page.underlying.is_empty() {
                page.underlying = underlying.to_string();
            }
            let n = page.items.len();
            let total = page.total_available;
            s.derivatives_cache.insert(key, page.clone());
            s.derivatives = page;
            s.push_log(
                "derivatives.search",
                call.elapsed.as_millis(),
                true,
                format!("{underlying} {dtype}/{strategy}: {n} of {total}"),
            );
        }
        Err(e) => {
            if e.kind == sc::ScErrorKind::RateLimited {
                s.note_rate_limit("derivatives.search");
                s.derivatives_error =
                    Some(format!("rate limited — retry in {}s", RATE_LIMIT_BACKOFF.as_secs()));
            } else {
                s.derivatives_error = Some(e.to_string());
            }
            s.push_log("derivatives.search", call.elapsed.as_millis(), false, e.to_string());
        }
    }
}

/// `search` takes the query positionally, not as a flag.
fn do_search(state: &Arc<Mutex<Shared>>, q: &str) {
    let call = sc::run(&["broker", "search", q]);
    let mut s = state.lock().unwrap();
    match &call.data {
        Ok(v) => {
            s.search_results = sc::pick(sc::result(v), &["items"])
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let n = s.search_results.len();
            s.push_log("broker.search", call.elapsed.as_millis(), true, format!("{q}: {n} hits"));
        }
        Err(e) => s.push_log("broker.search", call.elapsed.as_millis(), false, format!("{q}: {e}")),
    }
}

fn do_preview(state: &Arc<Mutex<Shared>>, intent: &TradeIntent) {
    {
        let mut s = state.lock().unwrap();
        s.preview_pending = true;
        s.preview = None;
        s.preview_error = None;
        s.order_result = None;
        s.order_error = None;
    }
    let argv = intent.argv(None, false);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let call = sc::run(&refs);
    let mut s = state.lock().unwrap();
    s.preview_pending = false;
    match &call.data {
        Ok(v) => match TradePreview::from_json(v) {
            Some(p) => {
                s.push_log("trade.preview", call.elapsed.as_millis(), true, p.confirmation_id.clone());
                s.preview = Some(p);
            }
            None => {
                s.preview_error = Some(format!(
                    "phase-1 returned no confirmation id.\n{}\n{}",
                    call.cmdline(),
                    call.raw_head(600)
                ));
                s.push_log("trade.preview", call.elapsed.as_millis(), false, "no confirmation id");
            }
        },
        Err(e) => {
            if e.kind == sc::ScErrorKind::Auth {
                s.session = None;
                s.session_error = Some(format!("{e} — run `sc login`"));
            }
            s.preview_error = Some(e.to_string());
            s.push_log("trade.preview", call.elapsed.as_millis(), false, e.to_string());
        }
    }
}

fn do_submit(state: &Arc<Mutex<Shared>>, intent: &TradeIntent, id: &str, accept_unsuitable: bool) {
    {
        let mut s = state.lock().unwrap();
        s.order_pending = true;
        s.order_error = None;
        s.order_result = None;
    }
    let argv = intent.argv(Some(id), accept_unsuitable);
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    let call = sc::run(&refs);
    let mut s = state.lock().unwrap();
    s.order_pending = false;
    match &call.data {
        Ok(v) => {
            let r = sc::result(v);
            let oid = sc::str_at(r, &["id", "order_id", "transaction_id"])
                .unwrap_or_else(|| "(no id returned)".into());
            s.order_result = Some(format!("order submitted: {oid}"));
            s.preview = None;
            s.push_log("trade.submit", call.elapsed.as_millis(), true, oid);
        }
        Err(e) => {
            s.order_error = Some(e.to_string());
            s.push_log("trade.submit", call.elapsed.as_millis(), false, e.to_string());
        }
    }
}
