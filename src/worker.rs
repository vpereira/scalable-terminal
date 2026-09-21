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
pub const RATE_LIMIT_BACKOFF_MAX: Duration = Duration::from_secs(900);

/// Which instruments to poll this round.
///
/// While probing, one call is enough to learn whether the limit has lifted, and
/// spending the whole watchlist to find out that it has not is what kept the
/// app stuck in a refusal loop.
pub fn poll_list(all: Vec<String>, probing: bool) -> Vec<String> {
    let mut list = all;
    if probing {
        list.truncate(1);
    }
    list
}

/// Backoff doubles with each consecutive refusal, capped.
///
/// A fixed wait is not enough: when the limit is a rolling quota rather than a
/// short burst rule, retrying at full rate every 90 seconds just trips it again
/// and the app never recovers.
pub fn backoff_for(level: u32) -> Duration {
    let secs = RATE_LIMIT_BACKOFF
        .as_secs()
        .saturating_mul(1u64 << level.min(6));
    Duration::from_secs(secs.min(RATE_LIMIT_BACKOFF_MAX.as_secs()))
}
pub const TIMEFRAMES: [&str; 8] = ["1d", "7d", "1m", "3m", "6m", "ytd", "1y", "max"];

#[derive(Debug, Clone)]
pub enum Cmd {
    RefreshAll,
    RefreshQuotes,
    /// Instruments worth polling, computed by the UI from the active list.
    SetPollSet(Vec<String>),
    WatchlistAdd(String),
    WatchlistRemove(String),
    LoadChart {
        isin: String,
        timeframe: String,
        force: bool,
    },
    LoadDerivatives {
        underlying: String,
        dtype: String,
        strategy: String,
    },
    ArmTrail {
        isin: String,
        distance: f64,
        percent: bool,
    },
    DisarmTrail(String),
    /// Cancel the resting stop and preview a replacement higher up. Phase two is
    /// left to the confirm dialog: this never places an order on its own.
    RatchetTrail {
        isin: String,
    },
    Search(String),
    PreviewTrade(TradeIntent),
    SubmitTrade {
        intent: TradeIntent,
        confirmation_id: String,
        accept_unsuitable: bool,
    },
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
        if let Some(v) = &self.venue
            && !v.trim().is_empty()
        {
            a.push("--venue".into());
            a.push(v.clone());
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
    pub poll_set: Vec<String>,
    pub quotes: HashMap<String, Quote>,
    /// Instrument names learned from any endpoint. A name does not change, so
    /// caching it keeps a row identifiable when its quote is missing.
    pub names: HashMap<String, String>,
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
    pub trails: Vec<Trail>,
    /// Set while a ratchet has cancelled the old stop but not yet placed the new
    /// one. The position is unprotected for this whole window.
    pub trail_gap: Option<String>,
    pub trail_error: Option<String>,
    pub watchlist_error: Option<String>,
    /// Set when the backend rate-limits us. All polling pauses until it passes.
    pub backoff_until: Option<Instant>,
    /// Consecutive refusals. Drives how long the next pause lasts.
    pub backoff_level: u32,
    /// After a pause, spend one call finding out whether the limit has lifted
    /// rather than burning the whole watchlist to discover it has not.
    pub probe_next: bool,
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
    /// The stop order currently protecting a position, if any.
    pub fn resting_stop(&self, isin: &str) -> Option<PendingOrder> {
        self.orders
            .iter()
            .find(|o| {
                o.isin == isin && o.side.eq_ignore_ascii_case("SELL") && o.stop_price.is_some()
            })
            .cloned()
    }

    pub fn backoff_secs_left(&self) -> Option<u64> {
        let until = self.backoff_until?;
        let left = until.saturating_duration_since(Instant::now());
        (!left.is_zero()).then(|| left.as_secs() + 1)
    }

    /// The backend rate-limits per endpoint; treat a hit as account-wide and
    /// stand down, because hammering a second endpoint will trip that one too.
    fn note_rate_limit(&mut self, from: &str) {
        let wait = backoff_for(self.backoff_level);
        self.backoff_level = (self.backoff_level + 1).min(6);
        self.backoff_until = Some(Instant::now() + wait);
        self.probe_next = true;
        let level = self.backoff_level;
        self.push_log(
            "rate limited",
            0,
            false,
            format!("{from}: backing off {}s (attempt {level})", wait.as_secs()),
        );
    }

    fn push_log(&mut self, cmd: &str, ms: u128, ok: bool, detail: impl Into<String>) {
        self.log.push(CallLog {
            cmd: cmd.to_string(),
            ms,
            ok,
            detail: detail.into(),
        });
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
    state.lock().unwrap().trails = load_trails();
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
            let (held_off, probing) = {
                let s = state.lock().unwrap();
                (s.backoff_secs_left().is_some(), s.probe_next)
            };
            if interval > Duration::ZERO && !held_off {
                let list = poll_list(state.lock().unwrap().poll_targets(), probing);
                if !list.is_empty() {
                    refresh_quotes(&state, &list);
                    ctx.request_repaint();
                }
            }
            next_poll = Instant::now()
                + if interval > Duration::ZERO {
                    interval
                } else {
                    Duration::from_secs(3600)
                };
        }
    }
}

impl Shared {
    /// What to poll. The UI sets this from the active list; until it has, fall
    /// back to the broker watchlist plus holdings so startup is never blank.
    fn poll_targets(&self) -> Vec<String> {
        if !self.poll_set.is_empty() {
            return self.poll_set.clone();
        }
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
            let list = state.lock().unwrap().poll_targets();
            refresh_quotes(state, &list);
        }
        Cmd::RefreshQuotes => {
            let list = state.lock().unwrap().poll_targets();
            refresh_quotes(state, &list);
        }
        Cmd::SetPollSet(set) => {
            // Anything newly on screen has no quote yet. Waiting for the next
            // tick leaves a row of dashes for up to a whole poll interval, which
            // at 90 seconds looks like a broken instrument.
            let fresh: Vec<String> = {
                let mut s = state.lock().unwrap();
                s.poll_set = set.clone();
                set.iter()
                    .filter(|i| !s.quotes.contains_key(*i))
                    .cloned()
                    .collect()
            };
            if !fresh.is_empty() {
                let held_off = state.lock().unwrap().backoff_secs_left().is_some();
                if !held_off {
                    refresh_quotes(state, &fresh);
                }
            }
        }
        Cmd::WatchlistAdd(isin) => {
            let call = sc::run(&["broker", "watchlist", "add", "--isin", &isin]);
            // The broker answers `ok: true` even when it declines to add, and
            // reports the real outcome in `is_on_watchlist`. It refuses any
            // instrument already held. Without this check the add looks like it
            // worked and the row simply never appears.
            let refused = match &call.data {
                Ok(v) => {
                    sc::pick(sc::result(v), &["is_on_watchlist"]).and_then(Value::as_bool)
                        == Some(false)
                }
                Err(_) => false,
            };
            log_call(state, "watchlist.add", &call, &isin);
            if refused {
                let mut s = state.lock().unwrap();
                let held = s.holdings.iter().any(|h| h.isin == isin);
                s.watchlist_error = Some(if held {
                    format!(
                        "{isin} is a position you hold; Scalable will not watchlist it. It is in Positions."
                    )
                } else {
                    format!("{isin} was declined by the broker")
                });
                s.push_log("watchlist.add", 0, false, format!("{isin} declined"));
            } else {
                state.lock().unwrap().watchlist_error = None;
            }
            refresh_watchlist(state);
            refresh_quotes(state, &[isin]);
        }
        Cmd::WatchlistRemove(isin) => {
            let call = sc::run(&["broker", "watchlist", "remove", "--isin", &isin]);
            log_call(state, "watchlist.remove", &call, &isin);
            refresh_watchlist(state);
        }
        Cmd::LoadChart {
            isin,
            timeframe,
            force,
        } => load_chart(state, &isin, &timeframe, force),
        Cmd::LoadDerivatives {
            underlying,
            dtype,
            strategy,
        } => load_derivatives(state, &underlying, &dtype, &strategy),
        Cmd::ArmTrail {
            isin,
            distance,
            percent,
        } => {
            let mut s = state.lock().unwrap();
            let mid = s.quotes.get(&isin).and_then(|q| q.mid).unwrap_or(0.0);
            s.trails.retain(|t| t.isin != isin);
            s.trails
                .push(Trail::new(isin.clone(), distance, percent, mid));
            s.trail_error = None;
            s.push_log("trail.arm", 0, true, format!("{isin} at {mid}"));
            let trails = s.trails.clone();
            drop(s);
            save_trails(&trails);
        }
        Cmd::DisarmTrail(isin) => {
            let mut s = state.lock().unwrap();
            s.trails.retain(|t| t.isin != isin);
            s.push_log("trail.disarm", 0, true, isin);
            let trails = s.trails.clone();
            drop(s);
            save_trails(&trails);
        }
        Cmd::RatchetTrail { isin } => ratchet_trail(state, &isin),
        Cmd::Search(q) => do_search(state, &q),
        Cmd::PreviewTrade(intent) => do_preview(state, &intent),
        Cmd::SubmitTrade {
            intent,
            confirmation_id,
            accept_unsuitable,
        } => {
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
        Err(e) => s.push_log(
            name,
            call.elapsed.as_millis(),
            false,
            format!("{ctxinfo}: {e}"),
        ),
    }
}

fn check_session(state: &Arc<Mutex<Shared>>) {
    let call = sc::run(&["whoami"]);
    let mut s = state.lock().unwrap();
    match &call.data {
        Ok(v) => {
            let r = sc::result(v);
            let first =
                sc::str_at(r, &["personOverview/personalDetails/firstName"]).unwrap_or_default();
            let last =
                sc::str_at(r, &["personOverview/personalDetails/lastName"]).unwrap_or_default();
            let name = format!("{first} {last}").trim().to_string();
            s.session = Some(if name.is_empty() {
                "authenticated".into()
            } else {
                name
            });
            s.session_error = None;
            s.push_log("whoami", call.elapsed.as_millis(), true, "session ok");
        }
        Err(e) => {
            // Only a genuine auth failure means the session is gone. A locked
            // Mac or a rate limit are transient, and telling the user to log in
            // again would be wrong advice in both cases.
            match e.kind {
                sc::ScErrorKind::DeviceLocked => {
                    s.session_error = Some("Mac is locked, unlock it to resume".into());
                }
                sc::ScErrorKind::RateLimited => {
                    s.session_error = Some("rate limited, retrying shortly".into());
                    s.note_rate_limit("whoami");
                }
                sc::ScErrorKind::Auth => {
                    s.session = None;
                    s.session_error = Some(format!("{e} — run `sc login`"));
                }
                _ => {
                    s.session_error = Some(e.to_string());
                }
            }
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
        state
            .lock()
            .unwrap()
            .push_log("refresh", 0, false, "an sc call panicked");
        return;
    };

    let mut s = state.lock().unwrap();

    match &ov.data {
        Ok(v) => {
            s.account.apply_overview(sc::result(v));
            s.overview_raw = v.clone();
            s.push_log("broker.overview", ov.elapsed.as_millis(), true, "");
        }
        Err(e) => s.push_log(
            "broker.overview",
            ov.elapsed.as_millis(),
            false,
            e.to_string(),
        ),
    }
    match &cash.data {
        Ok(v) => {
            s.account.apply_cash(sc::result(v));
            s.push_log("broker.cash-breakdown", cash.elapsed.as_millis(), true, "");
        }
        Err(e) => s.push_log(
            "broker.cash-breakdown",
            cash.elapsed.as_millis(),
            false,
            e.to_string(),
        ),
    }
    match &hd.data {
        Ok(v) => {
            s.holdings = Holding::list_from(sc::result(v));
            for h in &s.holdings.clone() {
                if !h.isin.is_empty() && !h.name.is_empty() {
                    s.names.insert(h.isin.clone(), h.name.clone());
                }
            }
            s.holdings_raw = v.clone();
            let n = s.holdings.len();
            s.push_log(
                "broker.holdings",
                hd.elapsed.as_millis(),
                true,
                format!("{n} positions"),
            );
        }
        Err(e) => s.push_log(
            "broker.holdings",
            hd.elapsed.as_millis(),
            false,
            e.to_string(),
        ),
    }
    match &tx.data {
        Ok(v) => {
            s.orders = PendingOrder::pending_from_transactions(sc::result(v));
            let n = s.orders.len();
            s.push_log(
                "broker.transactions",
                tx.elapsed.as_millis(),
                true,
                format!("{n} working"),
            );
        }
        Err(e) => s.push_log(
            "broker.transactions",
            tx.elapsed.as_millis(),
            false,
            e.to_string(),
        ),
    }
    // A stop is resting again, so the unprotected window is over.
    if let Some(isin) = s.trail_gap.clone()
        && s.resting_stop(&isin).is_some()
    {
        s.trail_gap = None;
        s.push_log("trail.gap", 0, true, format!("{isin} protected again"));
    }
    match &an.data {
        Ok(v) => {
            s.analytics = Analytics::from_json(sc::result(v));
            s.push_log("broker.analytics", an.elapsed.as_millis(), true, "");
        }
        Err(e) => s.push_log(
            "broker.analytics",
            an.elapsed.as_millis(),
            false,
            e.to_string(),
        ),
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
            let items = sc::pick(r, &["items"])
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
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
                if !q.name.is_empty() {
                    s.names.insert(q.isin.clone(), q.name.clone());
                }
                s.quotes.entry(q.isin.clone()).or_insert(q);
            }
            let n = s.watchlist.len();
            s.push_log(
                "broker.watchlist",
                call.elapsed.as_millis(),
                true,
                format!("{n} items"),
            );
        }
        Err(e) => s.push_log(
            "broker.watchlist",
            call.elapsed.as_millis(),
            false,
            e.to_string(),
        ),
    }
}

/// One instrument's result from a quote round: the ISIN, the parsed quote when
/// it succeeded, how long the call took, and the error when it did not.
type QuoteResult = (String, Option<Quote>, u128, Option<String>);

/// Fan out one `sc broker quote` per ISIN across a fixed worker pool.
/// This is the polling ceiling: round wall-clock ~= (n / QUOTE_FANOUT) * per_call.
fn refresh_quotes(state: &Arc<Mutex<Shared>>, isins: &[String]) {
    if isins.is_empty() {
        return;
    }
    let started = Instant::now();
    let results: Arc<Mutex<Vec<QuoteResult>>> = Arc::new(Mutex::new(Vec::new()));

    // One pool of QUOTE_FANOUT workers pulling from a shared cursor. Chunking with
    // a scope per chunk would join at every chunk boundary and serialise the round
    // on each chunk's slowest call, which would make the latency readout lie.
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..QUOTE_FANOUT.min(isins.len()) {
            let next = &next;
            let results = results.clone();
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= isins.len() {
                        break;
                    }
                    let isin = &isins[i];
                    let call = sc::run(&["broker", "quote", "--isin", isin]);
                    let ms = call.elapsed.as_millis();
                    let entry = match &call.data {
                        Ok(v) => (
                            isin.clone(),
                            Some(Quote::from_json(isin, sc::result(v))),
                            ms,
                            None,
                        ),
                        Err(e) => (isin.clone(), None, ms, Some(e.to_string())),
                    };
                    results.lock().unwrap().push(entry);
                }
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
                if !q.name.is_empty() {
                    s.names.insert(isin.clone(), q.name.clone());
                }
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
    // Advance the high water marks from the prices we just fetched.
    let mut advanced = false;
    let marks: Vec<(String, f64)> = s
        .trails
        .iter()
        .filter_map(|t| {
            s.quotes
                .get(&t.isin)
                .and_then(|q| q.mid)
                .map(|m| (t.isin.clone(), m))
        })
        .collect();
    for (isin, mid) in marks {
        if let Some(t) = s.trails.iter_mut().find(|t| t.isin == isin) {
            advanced |= t.observe(mid);
        }
    }
    if advanced {
        let trails = s.trails.clone();
        s.push_log("trail.mark", 0, true, "high water advanced");
        let snapshot = trails;
        drop(s);
        save_trails(&snapshot);
        s = state.lock().unwrap();
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
    } else if failures == 0 {
        // Only a round that fully succeeded proves the limit has lifted.
        if s.backoff_level > 0 || s.probe_next {
            s.push_log("rate limit", 0, true, "clear, resuming full rate");
        }
        s.backoff_level = 0;
        s.probe_next = false;
    }
}

fn load_chart(state: &Arc<Mutex<Shared>>, isin: &str, timeframe: &str, force: bool) {
    let key = (isin.to_string(), timeframe.to_string());
    {
        let mut s = state.lock().unwrap();
        if !force && let Some(c) = s.chart_cache.get(&key) {
            s.chart = c.clone();
            s.chart_error = None;
            return;
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
            s.push_log(
                "broker.chart",
                call.elapsed.as_millis(),
                true,
                format!("{isin} {timeframe}: {n} pts"),
            );
        }
        Err(e) => {
            if e.kind == sc::ScErrorKind::RateLimited {
                s.note_rate_limit("broker.chart");
                s.chart_error = Some(format!(
                    "rate limited — retrying in {}s",
                    RATE_LIMIT_BACKOFF.as_secs()
                ));
                s.deferred_chart = Some((isin.to_string(), timeframe.to_string()));
            } else {
                s.chart_error = Some(e.to_string());
            }
            s.chart = Chart {
                isin: isin.to_string(),
                timeframe: timeframe.to_string(),
                ..Default::default()
            };
            s.push_log(
                "broker.chart",
                call.elapsed.as_millis(),
                false,
                format!("{}: {e}", call.cmdline()),
            );
        }
    }
}

fn trails_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join(".config/scalable-terminal/trails.json")
}

/// Persist trails. Everything else about a trail is re-derived from broker state,
/// but the high water mark is local memory: lose it and the stop silently loosens
/// back to the current price on restart.
fn save_trails(trails: &[Trail]) {
    let rows: Vec<Value> = trails
        .iter()
        .map(|t| {
            serde_json::json!({
                "isin": t.isin,
                "distance": t.distance,
                "percent": t.percent,
                "high_water": t.high_water,
            })
        })
        .collect();
    let path = trails_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        path,
        serde_json::to_string_pretty(&rows).unwrap_or_default(),
    );
}

pub fn load_trails() -> Vec<Trail> {
    let Ok(txt) = std::fs::read_to_string(trails_path()) else {
        return Vec::new();
    };
    let Ok(rows) = serde_json::from_str::<Vec<Value>>(&txt) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|r| {
            let isin = sc::str_at(r, &["isin"])?;
            let distance = sc::f64_at(r, &["distance"])?;
            let percent = r.get("percent").and_then(Value::as_bool).unwrap_or(true);
            let high_water = sc::f64_at(r, &["high_water"]).unwrap_or(0.0);
            let mut t = Trail::new(isin, distance, percent, 0.0);
            t.high_water = high_water;
            t.valid().then_some(t)
        })
        .collect()
}

/// Move a trailing stop up one step.
///
/// The old order has to die before the replacement can even be previewed: the
/// shares are committed to it, so a preview of the same size would be refused.
/// That ordering is forced by the broker, and it means the position is
/// unprotected from the cancel until the user confirms the new order. The gap is
/// recorded in `trail_gap` so the interface can say so loudly.
///
/// This deliberately stops at phase one. Placing the replacement is the user's
/// explicit act, through the same confirm dialog as any other order.
fn ratchet_trail(state: &Arc<Mutex<Shared>>, isin: &str) {
    let plan = {
        let s = state.lock().unwrap();
        let Some(trail) = s.trails.iter().find(|t| t.isin == isin).cloned() else {
            return;
        };
        let Some(stop) = trail.suggested_stop() else {
            return;
        };
        let resting = s.resting_stop(isin);
        let shares = resting
            .as_ref()
            .and_then(|o| o.quantity)
            .or_else(|| {
                s.holdings
                    .iter()
                    .find(|h| h.isin == isin)
                    .map(|h| h.quantity)
            })
            .unwrap_or(0.0);
        (stop, shares, resting.map(|o| o.id))
    };
    let (stop, shares, order_id) = plan;
    if shares <= 0.0 {
        let mut s = state.lock().unwrap();
        s.trail_error = Some(format!("{isin}: no shares to protect"));
        return;
    }

    if let Some(id) = order_id {
        let call = sc::run(&["broker", "trade", "cancel", "--order-id", &id]);
        let mut s = state.lock().unwrap();
        match &call.data {
            Ok(_) => {
                s.trail_gap = Some(isin.to_string());
                s.push_log(
                    "trail.cancel",
                    call.elapsed.as_millis(),
                    true,
                    format!("{isin} {id}"),
                );
            }
            Err(e) => {
                // The old stop is still resting, so the position stays protected.
                s.trail_error = Some(format!("could not cancel the resting stop: {e}"));
                s.push_log(
                    "trail.cancel",
                    call.elapsed.as_millis(),
                    false,
                    e.to_string(),
                );
                return;
            }
        }
    }

    let intent = TradeIntent {
        isin: isin.to_string(),
        side: Side::Sell,
        order_type: OrderType::Stop,
        amount: None,
        shares: Some(shares),
        limit_price: None,
        stop_price: Some(stop),
        venue: None,
    };
    do_preview(state, &intent);

    let mut s = state.lock().unwrap();
    if s.preview.is_none() {
        s.trail_error = Some(format!(
            "the stop was cancelled but the replacement preview failed. {isin} is UNPROTECTED.              Place a stop manually or retry."
        ));
    }
}

/// Same discipline as charts: this endpoint rate-limits readily, so cache per
/// (underlying, type, strategy) and refuse a request during a backoff.
fn load_derivatives(state: &Arc<Mutex<Shared>>, underlying: &str, dtype: &str, strategy: &str) {
    let key = (
        underlying.to_string(),
        dtype.to_string(),
        strategy.to_string(),
    );
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
        "broker",
        "derivatives",
        "search",
        "--underlying",
        underlying,
        "--type",
        dtype,
        "--strategy",
        strategy,
        "--limit",
        "50",
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
                s.derivatives_error = Some(format!(
                    "rate limited — retry in {}s",
                    RATE_LIMIT_BACKOFF.as_secs()
                ));
            } else {
                s.derivatives_error = Some(e.to_string());
            }
            s.push_log(
                "derivatives.search",
                call.elapsed.as_millis(),
                false,
                e.to_string(),
            );
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
            for r in &s.search_results.clone() {
                if let (Some(isin), Some(name)) =
                    (sc::str_at(r, &["isin"]), sc::str_at(r, &["name"]))
                {
                    s.names.insert(isin, name);
                }
            }
            let n = s.search_results.len();
            s.push_log(
                "broker.search",
                call.elapsed.as_millis(),
                true,
                format!("{q}: {n} hits"),
            );
        }
        Err(e) => s.push_log(
            "broker.search",
            call.elapsed.as_millis(),
            false,
            format!("{q}: {e}"),
        ),
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
                s.push_log(
                    "trade.preview",
                    call.elapsed.as_millis(),
                    true,
                    p.confirmation_id.clone(),
                );
                s.preview = Some(p);
            }
            None => {
                s.preview_error = Some(format!(
                    "phase-1 returned no confirmation id.\n{}\n{}",
                    call.cmdline(),
                    call.raw_head(600)
                ));
                s.push_log(
                    "trade.preview",
                    call.elapsed.as_millis(),
                    false,
                    "no confirmation id",
                );
            }
        },
        Err(e) => {
            if e.kind == sc::ScErrorKind::DeviceLocked {
                s.session_error = Some("Mac is locked, unlock it to resume".into());
            } else if e.kind == sc::ScErrorKind::Auth {
                s.session = None;
                s.session_error = Some(format!("{e} — run `sc login`"));
            }
            s.preview_error = Some(e.to_string());
            s.push_log(
                "trade.preview",
                call.elapsed.as_millis(),
                false,
                e.to_string(),
            );
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
            s.push_log(
                "trade.submit",
                call.elapsed.as_millis(),
                false,
                e.to_string(),
            );
        }
    }
}
