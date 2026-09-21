//! Domain types, pinned against live `sc` payloads (2026-09-17).
//!
//! Every read endpoint wraps its payload in `data.result`, so callers unwrap with
//! `sc::result()` before handing a value to these constructors.

use crate::sc::{f64_at, pick, str_at};
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn bool_at(v: &Value, path: &str) -> bool {
    pick(v, &[path]).and_then(Value::as_bool).unwrap_or(false)
}

/// `2026-09-17T18:14:28.493Z` -> unix seconds. Avoids pulling in a date crate for
/// what is only ever used as a plot axis and a staleness check.
pub fn parse_iso8601(s: &str) -> Option<f64> {
    if s.len() < 19 {
        return None;
    }
    let n = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (h, mi, sec) = (n(11, 13)?, n(14, 16)?, n(17, 19)?);
    let millis = s
        .get(20..23)
        .filter(|_| s.as_bytes().get(19) == Some(&b'.'))
        .and_then(|m| m.parse::<i64>().ok())
        .unwrap_or(0);
    let days = days_from_civil(y, mo, d);
    Some((days * 86_400 + h * 3600 + mi * 60 + sec) as f64 + millis as f64 / 1000.0)
}

/// Days-from-civil, then seconds. Kept separate so the parser above stays readable.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Trailing performance by window, in percent.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Performance {
    pub day: Option<f64>,
    pub week: Option<f64>,
    pub month: Option<f64>,
    pub quarter: Option<f64>,
    pub half: Option<f64>,
    pub year: Option<f64>,
}

impl Performance {
    pub fn get(&self, w: Window) -> Option<f64> {
        match w {
            Window::Day => self.day,
            Window::Week => self.week,
            Window::Month => self.month,
            Window::Quarter => self.quarter,
            Window::Half => self.half,
            Window::Year => self.year,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Window {
    Day,
    Week,
    Month,
    Quarter,
    Half,
    Year,
}

impl Window {
    pub const ALL: [Window; 6] = [
        Window::Day,
        Window::Week,
        Window::Month,
        Window::Quarter,
        Window::Half,
        Window::Year,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Window::Day => "1D",
            Window::Week => "1W",
            Window::Month => "1M",
            Window::Quarter => "3M",
            Window::Half => "6M",
            Window::Year => "1Y",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Quote {
    pub isin: String,
    pub name: String,
    pub security_type: String,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub mid: Option<f64>,
    pub prev_close: Option<f64>,
    pub change_abs: Option<f64>,
    /// Performance by window, in percent. Every quote carries these; relative
    /// strength across a group is the whole reason to keep them.
    pub perf: Performance,
    pub currency: String,
    pub outdated: bool,
    pub timestamp: String,
    pub raw: Value,
}

impl Quote {
    pub fn from_json(isin: &str, v: &Value) -> Self {
        let mid = f64_at(v, &["quote_mid_price"]);
        // Intraday performance is the only place a reference close is exposed.
        let mut change_abs = None;
        let mut perf = Performance::default();
        if let Some(arr) = pick(v, &["quote_performances"]).and_then(Value::as_array) {
            for p in arr {
                let pct = f64_at(p, &["performance"]).map(|x| x * 100.0);
                match str_at(p, &["timeframe"]).as_deref() {
                    Some("INTRADAY") => {
                        change_abs = f64_at(p, &["simple_absolute_return"]);
                        perf.day = pct;
                    }
                    Some("ONE_WEEK") => perf.week = pct,
                    Some("ONE_MONTH") => perf.month = pct,
                    Some("THREE_MONTHS") => perf.quarter = pct,
                    Some("SIX_MONTHS") => perf.half = pct,
                    Some("ONE_YEAR") => perf.year = pct,
                    _ => {}
                }
            }
        }
        Quote {
            isin: str_at(v, &["isin"]).unwrap_or_else(|| isin.to_string()),
            name: str_at(v, &["name"]).unwrap_or_default(),
            security_type: str_at(v, &["security_type"]).unwrap_or_default(),
            bid: f64_at(v, &["quote_bid_price"]),
            ask: f64_at(v, &["quote_ask_price"]),
            mid,
            prev_close: match (mid, change_abs) {
                (Some(m), Some(c)) => Some(m - c),
                _ => None,
            },
            change_abs,
            perf,
            currency: str_at(v, &["quote_currency"]).unwrap_or_default(),
            outdated: bool_at(v, "quote_is_outdated"),
            timestamp: str_at(v, &["quote_timestamp_utc"]).unwrap_or_default(),
            raw: v.clone(),
        }
    }

    /// Watchlist rows carry a mid but no bid/ask — enough to render a row before
    /// the per-ISIN quote round fills in the spread.
    pub fn from_watchlist_item(v: &Value) -> Self {
        Quote {
            isin: str_at(v, &["isin"]).unwrap_or_default(),
            name: str_at(v, &["name"]).unwrap_or_default(),
            security_type: str_at(v, &["security_type"]).unwrap_or_default(),
            mid: f64_at(v, &["quote_mid_price"]),
            currency: str_at(v, &["quote_currency"]).unwrap_or_default(),
            outdated: bool_at(v, "quote_is_outdated"),
            timestamp: str_at(v, &["quote_timestamp_utc"]).unwrap_or_default(),
            raw: v.clone(),
            ..Default::default()
        }
    }

    pub fn spread_abs(&self) -> Option<f64> {
        match (self.bid, self.ask) {
            (Some(b), Some(a)) => Some(a - b),
            _ => None,
        }
    }

    /// Spread in basis points of mid — the execution cost the broker UI never states.
    pub fn spread_bps(&self) -> Option<f64> {
        match (self.spread_abs(), self.mid) {
            (Some(s), Some(m)) if m > 0.0 => Some(s / m * 10_000.0),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Holding {
    pub isin: String,
    pub name: String,
    pub security_type: String,
    pub quantity: f64,
    pub blocked: f64,
    pub pending: f64,
    pub fifo_price: Option<f64>,
    pub mid: Option<f64>,
    pub valuation: Option<f64>,
    pub currency: String,
    pub outdated: bool,
    pub timestamp: String,
}

impl Holding {
    pub fn list_from(v: &Value) -> Vec<Holding> {
        pick(v, &["items"])
            .and_then(Value::as_array)
            .map(|a| a.iter().map(Holding::one).collect())
            .unwrap_or_default()
    }

    fn one(v: &Value) -> Holding {
        Holding {
            isin: str_at(v, &["isin"]).unwrap_or_default(),
            name: str_at(v, &["name"]).unwrap_or_default(),
            security_type: str_at(v, &["security_type"]).unwrap_or_default(),
            quantity: f64_at(v, &["quantity"]).unwrap_or(0.0),
            blocked: f64_at(v, &["blocked_quantity"]).unwrap_or(0.0),
            pending: f64_at(v, &["pending_quantity"]).unwrap_or(0.0),
            fifo_price: f64_at(v, &["fifo_price"]),
            mid: f64_at(v, &["quote_mid_price"]),
            valuation: f64_at(v, &["valuation"]),
            currency: str_at(v, &["valuation_currency"])
                .or_else(|| str_at(v, &["quote_currency"]))
                .unwrap_or_default(),
            outdated: bool_at(v, "quote_is_outdated"),
            timestamp: str_at(v, &["quote_timestamp_utc"]).unwrap_or_default(),
        }
    }

    /// Shares actually free to sell.
    ///
    /// `blocked_quantity` does NOT account for resting sell orders — a position
    /// entirely committed to a working sell still reports `blocked: 0`. So the
    /// resting sells have to be subtracted here, or the ticket offers shares that
    /// are already on the market.
    pub fn free_quantity(&self, working: &[PendingOrder]) -> f64 {
        let resting: f64 = working
            .iter()
            .filter(|o| o.isin == self.isin && o.side.eq_ignore_ascii_case("SELL"))
            .filter_map(|o| o.quantity)
            .sum();
        (self.quantity - self.blocked - resting).max(0.0)
    }

    pub fn cost_basis(&self) -> Option<f64> {
        self.fifo_price.map(|p| p * self.quantity)
    }

    pub fn unrealized(&self) -> Option<f64> {
        match (self.valuation, self.cost_basis()) {
            (Some(v), Some(c)) => Some(v - c),
            _ => None,
        }
    }

    pub fn unrealized_pct(&self) -> Option<f64> {
        match (self.unrealized(), self.cost_basis()) {
            (Some(u), Some(c)) if c.abs() > 1e-9 => Some(u / c * 100.0),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Account {
    pub cash: Option<f64>,
    pub buying_power: Option<f64>,
    pub pending_buy_orders: Option<f64>,
    pub possible_taxes: Option<f64>,
    pub loaned: Option<f64>,
    pub securities: Option<f64>,
    pub crypto: Option<f64>,
    pub total: Option<f64>,
    /// `simpleAbsoluteReturn` per timeframe, in account currency.
    pub performance: Vec<(String, f64)>,
    pub valuation_ts: String,
    pub currency: String,
}

impl Account {
    pub fn apply_overview(&mut self, v: &Value) {
        self.securities = f64_at(v, &["valuation/securities"]);
        self.crypto = f64_at(v, &["valuation/crypto"]);
        self.total = f64_at(v, &["valuation/total"]);
        self.valuation_ts = str_at(v, &["timestamps/valuation_timestamp_utc"]).unwrap_or_default();
        self.performance = pick(v, &["performance"])
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        Some((
                            str_at(p, &["timeframe"])?,
                            f64_at(p, &["simpleAbsoluteReturn"])?,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if self.currency.is_empty() {
            self.currency = "EUR".into();
        }
    }

    pub fn apply_cash(&mut self, v: &Value) {
        self.cash = f64_at(v, &["cash_balance"]);
        self.buying_power = f64_at(v, &["buying_power"]);
        self.pending_buy_orders = f64_at(v, &["pending_buy_orders_amount"]);
        self.possible_taxes = f64_at(v, &["possible_taxes"]);
        self.loaned = f64_at(v, &["loaned"]);
    }

    /// Timeframes come back unordered; render them shortest-first.
    pub fn performance_ordered(&self) -> Vec<(String, f64)> {
        const ORDER: [&str; 8] = [
            "INTRADAY",
            "TWO_DAYS",
            "ONE_WEEK",
            "ONE_MONTH",
            "THREE_MONTHS",
            "SIX_MONTHS",
            "ONE_YEAR",
            "MAX",
        ];
        let mut out: Vec<(String, f64)> = self.performance.clone();
        out.sort_by_key(|(k, _)| ORDER.iter().position(|o| o == k).unwrap_or(usize::MAX));
        out
    }
}

/// A working order. Sourced from `broker transactions` filtered to PENDING —
/// `broker overview` carries no order list and the CLI exposes no orders command.
#[derive(Debug, Clone, Default)]
pub struct PendingOrder {
    pub id: String,
    pub isin: String,
    pub description: String,
    pub side: String,
    pub status: String,
    pub quantity: Option<f64>,
    pub amount: Option<f64>,
    pub limit_price: Option<f64>,
    pub stop_price: Option<f64>,
    pub currency: String,
    pub last_event: String,
}

impl PendingOrder {
    pub fn pending_from_transactions(v: &Value) -> Vec<PendingOrder> {
        pick(v, &["items"])
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|t| {
                        str_at(t, &["status"]).as_deref() == Some("PENDING")
                            && str_at(t, &["type"]).as_deref() == Some("SECURITY_TRANSACTION")
                            && !bool_at(t, "is_cancellation")
                    })
                    .map(|t| PendingOrder {
                        id: str_at(t, &["id"]).unwrap_or_default(),
                        isin: str_at(t, &["isin"]).unwrap_or_default(),
                        description: str_at(t, &["description"]).unwrap_or_default(),
                        side: str_at(t, &["side"]).unwrap_or_default(),
                        status: str_at(t, &["status"]).unwrap_or_default(),
                        quantity: f64_at(t, &["quantity"]),
                        amount: f64_at(t, &["amount"]),
                        limit_price: f64_at(t, &["limit_price"]),
                        stop_price: f64_at(t, &["stop_price"]),
                        currency: str_at(t, &["currency"]).unwrap_or_default(),
                        last_event: str_at(t, &["last_event_datetime"]).unwrap_or_default(),
                    })
                    .filter(|o| !o.id.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChartPoint {
    pub t: f64,
    pub mid: f64,
    pub ts: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Candle {
    /// Bucket start, unix seconds.
    pub t: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub ticks: usize,
}

impl Candle {
    pub fn up(&self) -> bool {
        self.close >= self.open
    }
}

#[derive(Debug, Clone, Default)]
pub struct Chart {
    pub isin: String,
    pub timeframe: String,
    pub currency: String,
    pub source: String,
    pub points: Vec<ChartPoint>,
    /// Previous close — the baseline the broker itself uses for intraday change.
    pub reference: Option<f64>,
    pub reference_ts: String,
}

impl Chart {
    /// Simple moving average over a trailing window of `days` **calendar days**.
    ///
    /// Not an n-observation window. The endpoint downsamples by span — 10-minute
    /// ticks on `1d`, daily on `3m`/`6m`/`ytd`, two-day on `1y`, monthly on `max`,
    /// and never more than ~190 points — so an n-point SMA would mean something
    /// different on every timeframe, and a 200-point window would never exist at
    /// all. A time window means the same thing everywhere: SMA 200 is 200 days.
    ///
    /// Emitted only from the first point whose trailing window is fully covered,
    /// so the head of the line is never a short average masquerading as a long one.
    pub fn sma_days(&self, days: f64) -> Vec<[f64; 2]> {
        if days <= 0.0 || self.points.len() < 2 {
            return Vec::new();
        }
        let window = days * 86_400.0;
        let first_t = self.points[0].t;

        let mut out = Vec::new();
        let mut lo = 0usize;
        let mut sum = 0.0;
        for hi in 0..self.points.len() {
            sum += self.points[hi].mid;
            let cutoff = self.points[hi].t - window;
            while self.points[lo].t < cutoff {
                sum -= self.points[lo].mid;
                lo += 1;
            }
            // Require the full window to have elapsed since the series began.
            if self.points[hi].t - first_t >= window {
                out.push([self.points[hi].t, sum / (hi - lo + 1) as f64]);
            }
        }
        out
    }

    /// Aggregate the mid-tick series into OHLC bars of `bucket` seconds.
    ///
    /// The endpoint publishes no OHLC — only mid ticks — so candles have to be
    /// built here. Open/close are the first and last tick in the bucket, high/low
    /// its extremes. Empty buckets are skipped rather than carried forward, so a
    /// gap in the series stays a gap instead of becoming a flat bar that was
    /// never traded.
    pub fn candles(&self, bucket: f64) -> Vec<Candle> {
        if bucket <= 0.0 || self.points.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<Candle> = Vec::new();
        for p in &self.points {
            let start = (p.t / bucket).floor() * bucket;
            match out.last_mut() {
                Some(c) if c.t == start => {
                    c.high = c.high.max(p.mid);
                    c.low = c.low.min(p.mid);
                    c.close = p.mid;
                    c.ticks += 1;
                }
                _ => out.push(Candle {
                    t: start,
                    open: p.mid,
                    high: p.mid,
                    low: p.mid,
                    close: p.mid,
                    ticks: 1,
                }),
            }
        }
        out
    }

    /// Bucket width that yields roughly `target` bars, snapped to a duration a
    /// trader recognises rather than an arbitrary fraction of the span.
    pub fn auto_bucket_secs(&self, target: usize) -> f64 {
        const STEPS: [f64; 14] = [
            60.0,        // 1m
            300.0,       // 5m
            900.0,       // 15m
            1_800.0,     // 30m
            3_600.0,     // 1h
            7_200.0,     // 2h
            14_400.0,    // 4h
            43_200.0,    // 12h
            86_400.0,    // 1d
            172_800.0,   // 2d
            604_800.0,   // 1w
            1_209_600.0, // 2w
            2_592_000.0, // 30d
            7_776_000.0, // 90d
        ];
        let span = self.span_days() * 86_400.0;
        if span <= 0.0 || target == 0 {
            return 86_400.0;
        }
        let ideal = span / target as f64;
        // Never bucket finer than the data itself: that just makes one-tick bars.
        let floor = self.median_spacing_secs().unwrap_or(0.0);
        *STEPS
            .iter()
            .find(|s| **s >= ideal && **s >= floor)
            .unwrap_or(STEPS.last().unwrap())
    }

    /// Total span of the series in days.
    pub fn span_days(&self) -> f64 {
        match (self.points.first(), self.points.last()) {
            (Some(a), Some(b)) => (b.t - a.t) / 86_400.0,
            _ => 0.0,
        }
    }

    /// Whether the series is long enough for a `days`-day average to exist.
    pub fn supports_sma(&self, days: f64) -> bool {
        self.points.len() >= 2 && self.span_days() >= days
    }

    /// Median gap between consecutive points, in seconds.
    pub fn median_spacing_secs(&self) -> Option<f64> {
        if self.points.len() < 2 {
            return None;
        }
        let mut gaps: Vec<f64> = self.points.windows(2).map(|w| w[1].t - w[0].t).collect();
        gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Some(gaps[gaps.len() / 2])
    }

    /// Roughly how many observations land in a `days`-day window — the real
    /// resolution behind the average.
    pub fn points_per_window(&self, days: f64) -> Option<f64> {
        let g = self.median_spacing_secs()?;
        (g > 0.0).then(|| days * 86_400.0 / g)
    }

    pub fn from_json(v: &Value) -> Chart {
        let points = pick(v, &["data_points"])
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        let ts = str_at(p, &["timestamp_utc"]).unwrap_or_default();
                        Some(ChartPoint {
                            t: parse_iso8601(&ts).unwrap_or(0.0),
                            mid: f64_at(p, &["mid_price"])?,
                            ts,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Chart {
            isin: str_at(v, &["isin"]).unwrap_or_default(),
            timeframe: str_at(v, &["timeframe"]).unwrap_or_default(),
            currency: str_at(v, &["currency"]).unwrap_or_default(),
            source: str_at(v, &["source"]).unwrap_or_default(),
            points,
            reference: f64_at(v, &["closing_reference_point/mid_price"]),
            reference_ts: str_at(v, &["closing_reference_point/timestamp_utc"]).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AllocSlice {
    pub name: String,
    pub valuation: f64,
    pub weight: f64,
    pub subs: Vec<AllocSlice>,
}

#[derive(Debug, Clone, Default)]
pub struct Analytics {
    /// (PRODUCT_TYPE | ASSET_CLASS | EQUITY_SECTOR | REGION, slices)
    pub allocations: Vec<(String, Vec<AllocSlice>)>,
    /// (type, score, state, items_held, max_items)
    pub health: Vec<(String, f64, String, i64, i64)>,
    /// (scenario, portfolio_performance, benchmark_performance)
    pub scenarios: Vec<(String, f64, f64)>,
    pub last_updated: String,
}

impl Analytics {
    pub fn from_json(v: &Value) -> Analytics {
        let slice = |p: &Value| AllocSlice {
            name: str_at(p, &["name"]).unwrap_or_default(),
            valuation: f64_at(p, &["valuation"]).unwrap_or(0.0),
            weight: f64_at(p, &["weight"]).unwrap_or(0.0),
            subs: pick(p, &["subpositions"])
                .and_then(Value::as_array)
                .map(|s| {
                    s.iter()
                        .map(|q| AllocSlice {
                            name: str_at(q, &["name"]).unwrap_or_default(),
                            valuation: f64_at(q, &["valuation"]).unwrap_or(0.0),
                            weight: f64_at(q, &["weight"]).unwrap_or(0.0),
                            subs: Vec::new(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };

        Analytics {
            allocations: pick(v, &["allocations"])
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|g| {
                            (
                                str_at(g, &["type"]).unwrap_or_default(),
                                pick(g, &["positions"])
                                    .and_then(Value::as_array)
                                    .map(|p| p.iter().map(&slice).collect())
                                    .unwrap_or_default(),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            health: pick(v, &["health_checks"])
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|h| {
                            (
                                str_at(h, &["type"]).unwrap_or_default(),
                                f64_at(h, &["health_score"]).unwrap_or(0.0),
                                str_at(h, &["state"]).unwrap_or_default(),
                                f64_at(h, &["number_of_items_in_portfolio"]).unwrap_or(0.0) as i64,
                                f64_at(h, &["max_items"]).unwrap_or(0.0) as i64,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            scenarios: pick(v, &["scenarios"])
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|s| {
                            (
                                str_at(s, &["type"]).unwrap_or_default(),
                                f64_at(s, &["portfolio_performance"]).unwrap_or(0.0) * 100.0,
                                f64_at(s, &["benchmark_performance"]).unwrap_or(0.0) * 100.0,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
            last_updated: str_at(v, &["last_updated_utc"]).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn cmd(self) -> &'static str {
        match self {
            Side::Buy => "buy",
            Side::Sell => "sell",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
    Market,
    Limit,
    Stop,
}

impl OrderType {
    pub fn cmd(self) -> &'static str {
        match self {
            OrderType::Market => "market",
            OrderType::Limit => "limit",
            OrderType::Stop => "stop",
        }
    }
}

/// Phase-1 pre-trade disclosure. Paths are contractual: they come from
/// `sc capabilities --json`, rule `pre_trade_full_disclosure_v1`.
#[derive(Debug, Clone)]
pub struct TradePreview {
    pub confirmation_id: String,
    pub expires_at_epoch: Option<f64>,
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub mid: Option<f64>,
    pub currency: String,
    pub quote_outdated: bool,
    pub quote_ts: String,
    pub shares: Option<f64>,
    pub est_volume: Option<f64>,
    pub tradable: bool,
    pub venue: String,
    pub venue_status: String,
    pub requires_accept_unsuitable: bool,
    pub suitability_status: String,
    pub entry_cost: Option<f64>,
    pub entry_cost_pct: Option<f64>,
    pub ongoing_cost: Option<f64>,
    pub exit_cost: Option<f64>,
    pub warning_title: String,
    pub warning_body: String,
    pub raw: Value,
}

impl TradePreview {
    pub fn from_json(v: &Value) -> Option<Self> {
        let id = str_at(v, &["confirmation/id"])?;
        Some(TradePreview {
            confirmation_id: id,
            expires_at_epoch: f64_at(v, &["confirmation/expires_at_epoch"]),
            bid: f64_at(v, &["result/market_quote/bid_price"]),
            ask: f64_at(v, &["result/market_quote/ask_price"]),
            mid: f64_at(v, &["result/market_quote/mid_price"]),
            currency: str_at(v, &["result/market_quote/currency"]).unwrap_or_default(),
            quote_outdated: bool_at(v, "result/market_quote/is_outdated"),
            quote_ts: str_at(v, &["result/market_quote/timestamp_utc"]).unwrap_or_default(),
            shares: f64_at(v, &["result/calculation/shares"]),
            est_volume: f64_at(v, &["result/calculation/estimated_order_volume_raw"])
                .or_else(|| f64_at(v, &["result/calculation/estimated_order_volume"])),
            tradable: bool_at(v, "result/tradability/tradable"),
            venue: str_at(v, &["result/tradability/selected_venue_label"])
                .or_else(|| str_at(v, &["result/tradability/selected_venue"]))
                .unwrap_or_default(),
            venue_status: str_at(v, &["result/tradability/selected_venue_status"])
                .unwrap_or_default(),
            requires_accept_unsuitable: bool_at(v, "result/suitability/requires_accept_unsuitable"),
            suitability_status: str_at(v, &["result/suitability/status"]).unwrap_or_default(),
            entry_cost: f64_at(v, &["result/ex_ante_costs/entryCosts/total/amount"]),
            entry_cost_pct: f64_at(v, &["result/ex_ante_costs/entryCosts/total/percentage"]),
            ongoing_cost: f64_at(v, &["result/ex_ante_costs/ongoingCosts/total/amount"]),
            exit_cost: f64_at(v, &["result/ex_ante_costs/exitCosts/total/amount"]),
            warning_title: str_at(v, &["result/warning/title"]).unwrap_or_default(),
            warning_body: str_at(v, &["result/warning/body"]).unwrap_or_default(),
            raw: v.clone(),
        })
    }

    /// Seconds left on the confirmation id. Phase-2 is rejected once this lapses,
    /// so the UI disarms itself rather than letting a stale submit fail at the broker.
    pub fn seconds_left(&self) -> Option<i64> {
        let exp = self.expires_at_epoch?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs_f64();
        Some((exp - now).round() as i64)
    }

    pub fn expired(&self) -> bool {
        matches!(self.seconds_left(), Some(s) if s <= 0)
    }

    /// Cost of crossing the spread now, in bps.
    pub fn spread_bps(&self) -> Option<f64> {
        match (self.bid, self.ask, self.mid) {
            (Some(b), Some(a), Some(m)) if m > 0.0 => Some((a - b) / m * 10_000.0),
            _ => None,
        }
    }
}

/// One row from `broker derivatives search`. The payload carries metrics only —
/// no name and no live quote; those arrive once the instrument is selected and
/// the normal quote round picks it up.
#[derive(Debug, Clone, Default)]
pub struct Derivative {
    pub isin: String,
    pub issuer: String,
    pub strategy: String,
    pub subcategory: String,
    pub leverage: Option<f64>,
    pub factor: Option<f64>,
    pub strike: Option<f64>,
    pub strike_currency: String,
    pub knockout_barrier: Option<f64>,
    pub distance_to_knockout: Option<f64>,
    pub premium_pct: Option<f64>,
    pub expiry: String,
    pub open_end: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DerivativesPage {
    pub underlying: String,
    pub derivative_type: String,
    pub total_available: u64,
    pub items: Vec<Derivative>,
}

impl DerivativesPage {
    pub fn from_json(v: &Value) -> DerivativesPage {
        let items = pick(v, &["items"])
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|d| Derivative {
                        isin: str_at(d, &["isin"]).unwrap_or_default(),
                        issuer: str_at(d, &["issuer"]).unwrap_or_default(),
                        strategy: str_at(d, &["strategy"]).unwrap_or_default(),
                        subcategory: str_at(d, &["product_subcategory"]).unwrap_or_default(),
                        leverage: f64_at(d, &["leverage"]),
                        factor: f64_at(d, &["factor"]),
                        strike: f64_at(d, &["strike/value"]),
                        strike_currency: str_at(d, &["strike/currency_iso_code"])
                            .unwrap_or_default(),
                        knockout_barrier: f64_at(d, &["knockout_barrier/value"]),
                        distance_to_knockout: f64_at(d, &["distance_to_knockout"]),
                        premium_pct: f64_at(d, &["premium_percentage"]),
                        expiry: str_at(d, &["expiry_date"]).unwrap_or_default(),
                        open_end: pick(d, &["expiry_is_open_end"])
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    })
                    .filter(|d| !d.isin.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        DerivativesPage {
            underlying: str_at(v, &["underlying_isin"]).unwrap_or_default(),
            derivative_type: str_at(v, &["derivative_type"]).unwrap_or_default(),
            total_available: f64_at(v, &["total_available"]).unwrap_or(0.0) as u64,
            items,
        }
    }
}

/// A client side trailing stop.
///
/// `sc` has no trailing order type and no amend command, so a trail can only be
/// expressed as: watch the price, and when it ratchets, cancel the resting stop
/// and place a new one higher. Everything except the high water mark is derived
/// from live broker state, so an order cancelled or moved elsewhere is picked up
/// rather than silently disagreed with.
#[derive(Debug, Clone, PartialEq)]
pub struct Trail {
    pub isin: String,
    /// Fraction below the high water mark when `percent`, else an absolute price.
    pub distance: f64,
    pub percent: bool,
    /// Best mid seen since the trail was armed. Never decreases.
    pub high_water: f64,
    /// Smallest improvement worth a replacement, as a fraction of price.
    /// Replacing costs three calls and opens an unprotected window, so tiny
    /// ratchets are not worth taking.
    pub min_step: f64,
}

impl Trail {
    pub const DEFAULT_MIN_STEP: f64 = 0.001;

    pub fn new(isin: impl Into<String>, distance: f64, percent: bool, mid: f64) -> Self {
        Trail {
            isin: isin.into(),
            distance,
            percent,
            high_water: mid,
            min_step: Self::DEFAULT_MIN_STEP,
        }
    }

    /// Feed a new mid. Returns true when the high water mark advanced.
    pub fn observe(&mut self, mid: f64) -> bool {
        if mid.is_finite() && mid > self.high_water {
            self.high_water = mid;
            true
        } else {
            false
        }
    }

    /// Where the stop should sit given the high water mark.
    ///
    /// Rounded down to four decimals, so rounding can only ever place the stop
    /// further from the market, never closer to triggering. The epsilon matters:
    /// `190 * 0.97` is `184.29999999999998` in binary floating point, and a bare
    /// floor would turn that into `184.2999`, throwing away a whole tick every
    /// time the arithmetic lands a hair under a round number.
    pub fn suggested_stop(&self) -> Option<f64> {
        if !self.valid() || self.high_water <= 0.0 {
            return None;
        }
        let raw = if self.percent {
            self.high_water * (1.0 - self.distance)
        } else {
            self.high_water - self.distance
        };
        if raw <= 0.0 {
            return None;
        }
        Some((raw * 10_000.0 + 1e-6).floor() / 10_000.0)
    }

    pub fn valid(&self) -> bool {
        self.distance > 0.0 && (!self.percent || self.distance < 1.0)
    }

    /// Whether the resting stop is far enough below the suggestion to be worth
    /// replacing. A trail never moves a stop down.
    pub fn should_move(&self, current_stop: Option<f64>) -> bool {
        let Some(sug) = self.suggested_stop() else {
            return false;
        };
        match current_stop {
            None => true,
            Some(cur) => sug - cur >= (sug * self.min_step).max(1e-6),
        }
    }

    /// How far the current price sits above the stop, as a fraction.
    pub fn cushion(&self, mid: f64, current_stop: Option<f64>) -> Option<f64> {
        let stop = current_stop.or_else(|| self.suggested_stop())?;
        (mid > 0.0 && stop > 0.0).then(|| (mid - stop) / mid)
    }
}

/// A broker side price alert.
///
/// Direction is not something you choose: the broker derives UP or DOWN from
/// where the price sits relative to the market when the alert is created.
#[derive(Debug, Clone, Default)]
pub struct PriceAlert {
    pub id: String,
    pub isin: String,
    pub name: String,
    pub security_type: String,
    pub price: Option<f64>,
    pub direction: String,
    pub active: bool,
    pub triggered: String,
}

impl PriceAlert {
    pub fn list_from(v: &Value) -> Vec<PriceAlert> {
        pick(v, &["items"])
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|i| PriceAlert {
                        id: str_at(i, &["alert_id"]).unwrap_or_default(),
                        isin: str_at(i, &["isin"]).unwrap_or_default(),
                        name: str_at(i, &["name"]).unwrap_or_default(),
                        security_type: str_at(i, &["security_type"]).unwrap_or_default(),
                        price: f64_at(i, &["price"]),
                        direction: str_at(i, &["direction"]).unwrap_or_default(),
                        active: bool_at(i, "is_active"),
                        triggered: str_at(i, &["triggered_timestamp_utc"]).unwrap_or_default(),
                    })
                    .filter(|a| !a.id.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn has_triggered(&self) -> bool {
        !self.triggered.is_empty()
    }

    /// How far the market still has to travel, as a fraction of the mid.
    pub fn distance(&self, mid: f64) -> Option<f64> {
        let p = self.price?;
        (mid > 0.0).then(|| (p - mid) / mid)
    }
}

#[derive(Debug, Clone, Default)]
pub struct NewsItem {
    pub headline: String,
    pub source: String,
    pub published: String,
}

/// News and an editorial summary for one instrument.
///
/// Note the payload sits directly under `data`, not `data.result`, the same as
/// `broker.chart`. Coverage is uneven: large caps carry a summary, small ones
/// often return nothing at all, so absence is normal rather than an error.
#[derive(Debug, Clone, Default)]
pub struct SecurityNews {
    pub isin: String,
    pub locale: String,
    pub short: String,
    pub long: String,
    pub last_updated: String,
    pub sources: Vec<NewsItem>,
}

impl SecurityNews {
    pub fn from_json(v: &Value) -> SecurityNews {
        SecurityNews {
            isin: str_at(v, &["isin"]).unwrap_or_default(),
            locale: str_at(v, &["locale"]).unwrap_or_default(),
            short: str_at(v, &["summary/short"]).unwrap_or_default(),
            long: str_at(v, &["summary/long"]).unwrap_or_default(),
            last_updated: str_at(v, &["summary/last_updated"]).unwrap_or_default(),
            sources: pick(v, &["sources"])
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(|i| NewsItem {
                            headline: str_at(i, &["headline"]).unwrap_or_default(),
                            source: str_at(i, &["source_name"]).unwrap_or_default(),
                            published: str_at(i, &["publication_time_utc"]).unwrap_or_default(),
                        })
                        .filter(|i| !i.headline.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.short.is_empty() && self.long.is_empty() && self.sources.is_empty()
    }
}
