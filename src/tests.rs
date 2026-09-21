//! Extractor tests against payloads captured from a live account (2026-09-17),
//! account identifiers redacted. These pin the JSON shapes so a change in the
//! undocumented CLI surface fails here rather than silently blanking the UI.

use crate::model::*;
use crate::sc;
use serde_json::Value;

fn envelope(raw: &str) -> Value {
    let v: Value = serde_json::from_str(raw).expect("fixture parses");
    assert_eq!(v["ok"], Value::Bool(true), "fixture is a success envelope");
    v["data"].clone()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

const QUOTE: &str = include_str!("../tests/fixtures/quote.json");
const HOLDINGS: &str = include_str!("../tests/fixtures/holdings.json");
const OVERVIEW: &str = include_str!("../tests/fixtures/overview.json");
const CASH: &str = include_str!("../tests/fixtures/cash-breakdown.json");
const TRANSACTIONS: &str = include_str!("../tests/fixtures/transactions.json");
const WATCHLIST: &str = include_str!("../tests/fixtures/watchlist.json");
const CHART: &str = include_str!("../tests/fixtures/chart.json");
const ANALYTICS: &str = include_str!("../tests/fixtures/analytics.json");
const PREVIEW: &str = include_str!("../tests/fixtures/trade-preview.json");
const CHART_YTD: &str = include_str!("../tests/fixtures/chart-ytd.json");

#[test]
fn iso8601_matches_unix_epoch() {
    assert_eq!(
        parse_iso8601("2026-09-16T05:30:07.000Z"),
        Some(1_789_536_607.0)
    );
    assert_eq!(
        parse_iso8601("2026-09-17T18:14:28.000Z"),
        Some(1_789_668_868.0)
    );
    assert!(parse_iso8601("").is_none());
    assert!(parse_iso8601("garbage").is_none());
}

/// Most endpoints nest under `result`; `broker.chart` does not. The unwrap must
/// tolerate both or every chart render comes back empty.
#[test]
fn result_unwrap_handles_both_shapes() {
    let quote = envelope(QUOTE);
    assert!(quote.get("result").is_some());
    assert!(sc::result(&quote).get("quote_mid_price").is_some());

    let chart = envelope(CHART);
    assert!(chart.get("result").is_none());
    assert!(sc::result(&chart).get("data_points").is_some());
}

#[test]
fn quote_extracts_two_sided_market() {
    let q = Quote::from_json("CA53056H1047", sc::result(&envelope(QUOTE)));
    assert_eq!(q.isin, "CA53056H1047");
    assert_eq!(q.name, "Liberty Gold");
    assert_eq!(q.security_type, "STOCK");
    assert_eq!(q.currency, "EUR");
    assert!(!q.outdated);
    assert!(close(q.bid.unwrap(), 1.296));
    assert!(close(q.ask.unwrap(), 1.338));
    assert!(close(q.mid.unwrap(), 1.317));
    // Intraday performance is the only reference-close the endpoint exposes.
    assert!(close(q.change_abs.unwrap(), 0.057));
    assert!(close(q.prev_close.unwrap(), 1.26));
    assert!((q.spread_bps().unwrap() - 318.9066).abs() < 1e-3);
    assert!(close(q.spread_abs().unwrap(), 0.042));
}

#[test]
fn watchlist_items_seed_rows_without_a_two_sided_quote() {
    let data = envelope(WATCHLIST);
    let items = sc::result(&data)["items"].as_array().expect("items");
    assert!(!items.is_empty());
    let q = Quote::from_watchlist_item(&items[0]);
    assert!(!q.isin.is_empty());
    assert!(q.mid.is_some());
    // No bid/ask on this endpoint — the quote round fills them in.
    assert!(q.bid.is_none() && q.ask.is_none());
}

#[test]
fn holdings_carry_cost_basis_and_pnl() {
    let hs = Holding::list_from(sc::result(&envelope(HOLDINGS)));
    assert_eq!(hs.len(), 3);

    let h = hs
        .iter()
        .find(|h| h.isin == "CA53056H1047")
        .expect("Liberty Gold");
    assert_eq!(h.name, "Liberty Gold");
    assert!(close(h.quantity, 10.0));
    assert!(close(h.fifo_price.unwrap(), 1.238));
    assert!(close(h.valuation.unwrap(), 13.17));
    assert!(close(h.cost_basis().unwrap(), 12.38));
    assert!(close(h.unrealized().unwrap(), 0.79));
    assert!((h.unrealized_pct().unwrap() - 6.381260).abs() < 1e-4);
    assert_eq!(h.currency, "EUR");

    // Every position must price, or the portfolio total is a lie.
    assert!(
        hs.iter()
            .all(|h| h.valuation.is_some() && h.fifo_price.is_some())
    );
}

#[test]
fn account_merges_overview_and_cash_breakdown() {
    let mut a = Account::default();
    a.apply_overview(sc::result(&envelope(OVERVIEW)));
    assert!(close(a.total.unwrap(), 361.37));
    assert!(close(a.securities.unwrap(), 68.0));
    assert!(close(a.crypto.unwrap(), 0.0));
    assert_eq!(a.performance.len(), 8);
    assert!(!a.valuation_ts.is_empty());

    a.apply_cash(sc::result(&envelope(CASH)));
    assert!(close(a.cash.unwrap(), 293.37));
    assert!(close(a.buying_power.unwrap(), 293.37));

    // securities + cash must reconcile to the reported total.
    assert!(close(
        a.securities.unwrap() + a.cash.unwrap() + a.crypto.unwrap(),
        a.total.unwrap()
    ));

    let ordered = a.performance_ordered();
    assert_eq!(ordered.first().unwrap().0, "INTRADAY");
    assert_eq!(ordered.last().unwrap().0, "MAX");
}

#[test]
fn working_orders_come_from_pending_transactions() {
    let os = PendingOrder::pending_from_transactions(sc::result(&envelope(TRANSACTIONS)));
    assert_eq!(os.len(), 3, "three resting sells in the captured account");
    assert!(os.iter().all(|o| o.status == "PENDING"));
    assert!(os.iter().all(|o| o.side == "SELL"));
    assert!(os.iter().all(|o| !o.id.is_empty()));

    let o = os
        .iter()
        .find(|o| o.isin == "CA53056H1047")
        .expect("Liberty Gold sell");
    assert!(close(o.limit_price.unwrap(), 1.42));
    assert!(close(o.quantity.unwrap(), 10.0));
    assert_eq!(o.description, "Liberty Gold");
}

#[test]
fn settled_transactions_are_not_treated_as_working_orders() {
    let data = envelope(TRANSACTIONS);
    let all = sc::result(&data)["items"].as_array().unwrap().len();
    let pending = PendingOrder::pending_from_transactions(sc::result(&data)).len();
    assert!(pending < all, "filter must drop non-PENDING rows");
}

#[test]
fn chart_parses_points_and_reference_close() {
    let c = Chart::from_json(sc::result(&envelope(CHART)));
    assert_eq!(c.isin, "CA53056H1047");
    assert_eq!(c.timeframe, "1d");
    assert_eq!(c.currency, "EUR");
    assert_eq!(c.points.len(), 69);
    assert!(close(c.reference.unwrap(), 1.26));

    let first = &c.points[0];
    assert!(close(first.mid, 1.239));
    assert!(close(first.t, 1_789_536_607.0));

    // Timestamps must be monotonic or the plot draws backwards.
    assert!(c.points.windows(2).all(|w| w[1].t >= w[0].t));
    assert!(c.points.iter().all(|p| p.t > 1_000_000_000.0));
}

#[test]
fn analytics_extracts_allocations_health_and_scenarios() {
    let a = Analytics::from_json(sc::result(&envelope(ANALYTICS)));

    let kinds: Vec<&str> = a.allocations.iter().map(|(k, _)| k.as_str()).collect();
    assert!(kinds.contains(&"PRODUCT_TYPE"));
    assert!(kinds.contains(&"ASSET_CLASS"));
    assert!(kinds.contains(&"EQUITY_SECTOR"));
    assert!(kinds.contains(&"REGION"));

    let (_, product) = a
        .allocations
        .iter()
        .find(|(k, _)| k == "PRODUCT_TYPE")
        .unwrap();
    let cash = product
        .iter()
        .find(|s| s.name == "cash")
        .expect("cash slice");
    assert!(close(cash.valuation, 293.37));
    assert!((cash.weight - 0.8118272131).abs() < 1e-9);
    // Weights within a group sum to 1.
    let sum: f64 = product.iter().map(|s| s.weight).sum();
    assert!((sum - 1.0).abs() < 1e-6);

    // Region slices nest one level deep.
    let (_, region) = a.allocations.iter().find(|(k, _)| k == "REGION").unwrap();
    assert!(region.iter().any(|s| !s.subs.is_empty()));

    assert_eq!(a.health.len(), 3);
    assert!(
        a.health
            .iter()
            .all(|(_, score, ..)| (0.0..=1.0).contains(score))
    );

    assert_eq!(a.scenarios.len(), 5);
    let world = a
        .scenarios
        .iter()
        .find(|(t, ..)| t == "WORLD_DOWN")
        .unwrap();
    assert!(
        (world.1 - -0.73).abs() < 1e-6,
        "portfolio performance scaled to percent"
    );
    assert!(
        (world.2 - -5.42).abs() < 1e-6,
        "benchmark performance scaled to percent"
    );
}

/// The 1d fixture is intraday: 69 ticks inside a single session. No multi-day
/// average exists there, and drawing one anyway would be a lie.
#[test]
fn sma_requires_the_series_to_cover_the_window() {
    let data = envelope(CHART);
    let c = Chart::from_json(sc::result(&data));

    assert!(c.span_days() < 2.0, "1d fixture spans under two days");
    for days in [20.0, 50.0, 200.0] {
        assert!(!c.supports_sma(days));
        assert!(
            c.sma_days(days).is_empty(),
            "{days}-day SMA must not be drawn"
        );
    }
    assert!(c.sma_days(0.0).is_empty());
}

/// A window the series does cover must equal a directly computed trailing mean.
#[test]
fn sma_days_matches_a_direct_trailing_mean() {
    let data = envelope(CHART);
    let c = Chart::from_json(sc::result(&data));

    let days = 0.25; // six hours — inside the fixture's span
    assert!(c.supports_sma(days));
    let sma = c.sma_days(days);
    assert!(!sma.is_empty());

    let window = days * 86_400.0;
    let first_t = c.points[0].t;

    // Nothing is emitted before the window is fully elapsed.
    assert!(sma[0][0] - first_t >= window - 1e-6);

    for &[t, v] in &sma {
        let members: Vec<f64> = c
            .points
            .iter()
            .filter(|p| p.t <= t + 1e-9 && p.t >= t - window - 1e-9)
            .map(|p| p.mid)
            .collect();
        assert!(!members.is_empty());
        let mean = members.iter().sum::<f64>() / members.len() as f64;
        assert!(
            (v - mean).abs() < 1e-9,
            "at t={t}: rolling {v} vs direct {mean}"
        );
    }

    // An average is bounded by the series it averages.
    let lo = c.points.iter().map(|p| p.mid).fold(f64::MAX, f64::min);
    let hi = c.points.iter().map(|p| p.mid).fold(f64::MIN, f64::max);
    assert!(sma.iter().all(|v| v[1] >= lo - 1e-9 && v[1] <= hi + 1e-9));
}

/// The payoff of day-windows: on a long timeframe all three classic averages
/// draw, including the 200-day that an observation-count window could never
/// reach — the endpoint never returns 200 points on any timeframe.
#[test]
fn long_timeframe_supports_all_three_classic_averages() {
    let data = envelope(CHART_YTD);
    let c = Chart::from_json(sc::result(&data));

    assert_eq!(c.timeframe, "ytd");
    assert!(
        c.points.len() < 200,
        "endpoint downsamples: {} points",
        c.points.len()
    );
    assert!(
        c.span_days() > 200.0,
        "but it still spans {:.0} days",
        c.span_days()
    );

    for days in [20.0, 50.0, 200.0] {
        assert!(c.supports_sma(days), "{days}-day window fits in the span");
        let sma = c.sma_days(days);
        assert!(!sma.is_empty(), "{days}-day SMA must draw");
        // Longer windows start later and so plot fewer points.
        assert!(sma.len() < c.points.len());
    }
    assert!(c.sma_days(20.0).len() > c.sma_days(200.0).len());

    // Daily bars here, so a 20-day window really is about 20 observations.
    let n = c.points_per_window(20.0).unwrap();
    assert!(
        (10.0..40.0).contains(&n),
        "expected ~daily resolution, got {n}"
    );

    // A window past the span still refuses.
    assert!(!c.supports_sma(500.0));
    assert!(c.sma_days(500.0).is_empty());
}

/// Spacing is what makes observation-count windows meaningless across
/// timeframes, and it is the number the UI reports.
#[test]
fn spacing_and_window_resolution_are_reported() {
    let data = envelope(CHART);
    let c = Chart::from_json(sc::result(&data));

    let gap = c.median_spacing_secs().expect("spacing");
    assert!(
        gap > 0.0 && gap < 3600.0,
        "1d fixture is intraday, got {gap}s"
    );

    let n = c.points_per_window(1.0).expect("resolution");
    assert!(n > 1.0, "a one-day window holds many intraday ticks");
    assert!((n - 86_400.0 / gap).abs() < 1e-6);

    assert!(Chart::default().median_spacing_secs().is_none());
    assert_eq!(Chart::default().span_days(), 0.0);
}

/// Phase-1 disclosure. These paths are contractual (`pre_trade_full_disclosure_v1`),
/// so a break here means the CLI changed its published contract.
#[test]
fn trade_preview_extracts_full_disclosure() {
    let data = envelope(PREVIEW);
    let p = TradePreview::from_json(&data).expect("confirmation id present");

    assert!(p.confirmation_id.starts_with("scb1_"));
    assert!(p.expires_at_epoch.unwrap() > 1_700_000_000.0);

    assert!(close(p.bid.unwrap(), 81.1));
    assert!(close(p.ask.unwrap(), 81.2));
    assert!(close(p.mid.unwrap(), 81.15));
    assert_eq!(p.currency, "EUR");
    assert!(!p.quote_outdated);
    assert!(!p.quote_ts.is_empty());

    assert!(close(p.shares.unwrap(), 1.0));
    assert!(close(p.est_volume.unwrap(), 75.0));
    assert!(p.tradable);
    assert_eq!(p.venue, "European Investor Exchange (EIX)");
    assert_eq!(p.venue_status, "TRADABLE_WITHOUT_APPROPRIATENESS");
    assert_eq!(p.suitability_status, "NOT_REQUIRED");
    assert!(!p.requires_accept_unsuitable);

    assert!(close(p.entry_cost.unwrap(), 0.99));
    assert!(close(p.entry_cost_pct.unwrap(), 0.0132));
    assert!(close(p.ongoing_cost.unwrap(), 0.21));
    assert!(close(p.exit_cost.unwrap(), 0.99));

    // Absent warning must degrade to empty, not panic the extractor.
    assert!(p.warning_title.is_empty());

    assert!((p.spread_bps().unwrap() - 12.3228).abs() < 1e-3);
}

/// A lapsed confirmation must disarm SUBMIT rather than fail at the broker.
#[test]
fn expired_confirmation_disarms_submit() {
    let data = envelope(PREVIEW);
    let mut p = TradePreview::from_json(&data).unwrap();

    p.expires_at_epoch = Some(0.0);
    assert!(p.expired());
    assert!(p.seconds_left().unwrap() < 0);

    let far = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        + 120.0;
    p.expires_at_epoch = Some(far);
    assert!(!p.expired());
    assert!(p.seconds_left().unwrap() > 100);

    // No expiry published: do not invent one.
    p.expires_at_epoch = None;
    assert!(!p.expired());
    assert!(p.seconds_left().is_none());
}

/// `blocked_quantity` is 0 on every position in the captured account even though
/// all three are fully committed to resting sell orders. Sizing a sell off
/// `quantity - blocked` would offer shares that are already on the market.
#[test]
fn free_quantity_subtracts_resting_sells() {
    let hd = envelope(HOLDINGS);
    let tx = envelope(TRANSACTIONS);
    let holdings = Holding::list_from(sc::result(&hd));
    let working = PendingOrder::pending_from_transactions(sc::result(&tx));

    // The broker itself reports nothing blocked.
    assert!(
        holdings
            .iter()
            .all(|h| h.blocked == 0.0 && h.pending == 0.0)
    );

    for h in &holdings {
        assert!(
            close(h.free_quantity(&working), 0.0),
            "{} is entirely committed to a resting sell, free must be 0, not {}",
            h.isin,
            h.free_quantity(&working)
        );
    }

    // With no working orders the whole position is free again.
    for h in &holdings {
        assert!(close(h.free_quantity(&[]), h.quantity));
    }
}

/// Only sells consume inventory; a resting buy must not reduce free quantity.
#[test]
fn free_quantity_ignores_buys_and_other_isins() {
    let hd = envelope(HOLDINGS);
    let h = Holding::list_from(sc::result(&hd))
        .into_iter()
        .find(|h| h.isin == "CA53056H1047")
        .unwrap();

    let buy = PendingOrder {
        isin: "CA53056H1047".into(),
        side: "BUY".into(),
        quantity: Some(5.0),
        ..Default::default()
    };
    let other = PendingOrder {
        isin: "JP3228600007".into(),
        side: "SELL".into(),
        quantity: Some(2.0),
        ..Default::default()
    };
    assert!(close(h.free_quantity(&[buy, other]), h.quantity));

    let partial = PendingOrder {
        isin: "CA53056H1047".into(),
        side: "SELL".into(),
        quantity: Some(4.0),
        ..Default::default()
    };
    assert!(close(h.free_quantity(&[partial]), 6.0));
}

/// Candles are derived, not published: the endpoint returns mid ticks only.
/// The aggregation must satisfy the OHLC invariants or the bars are fiction.
#[test]
fn candles_aggregate_ticks_into_valid_ohlc() {
    let data = envelope(CHART);
    let c = Chart::from_json(sc::result(&data));

    let bucket = 900.0; // 15 minutes
    let bars = c.candles(bucket);
    assert!(!bars.is_empty());
    assert!(
        bars.len() <= c.points.len(),
        "aggregation cannot invent bars"
    );

    // Every tick lands in exactly one bar.
    assert_eq!(bars.iter().map(|b| b.ticks).sum::<usize>(), c.points.len());

    for b in &bars {
        assert!(b.low <= b.open && b.low <= b.close, "low is the floor");
        assert!(b.high >= b.open && b.high >= b.close, "high is the ceiling");
        assert!(b.low <= b.high);
        assert!(b.ticks >= 1);
        assert!(close(b.t % bucket, 0.0), "bar starts on a bucket boundary");
    }

    // Bars are ordered and never overlap.
    assert!(bars.windows(2).all(|w| w[1].t > w[0].t));

    // The series endpoints survive aggregation.
    assert!(close(
        bars.first().unwrap().open,
        c.points.first().unwrap().mid
    ));
    assert!(close(
        bars.last().unwrap().close,
        c.points.last().unwrap().mid
    ));

    let lo = c.points.iter().map(|p| p.mid).fold(f64::MAX, f64::min);
    let hi = c.points.iter().map(|p| p.mid).fold(f64::MIN, f64::max);
    assert!(close(
        bars.iter().map(|b| b.low).fold(f64::MAX, f64::min),
        lo
    ));
    assert!(close(
        bars.iter().map(|b| b.high).fold(f64::MIN, f64::max),
        hi
    ));
}

/// A bucket wide enough to hold everything collapses to a single bar; a
/// degenerate bucket produces nothing rather than panicking.
#[test]
fn candle_bucketing_handles_the_extremes() {
    let data = envelope(CHART);
    let c = Chart::from_json(sc::result(&data));

    let one = c.candles(86_400.0 * 365.0);
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].ticks, c.points.len());

    assert!(c.candles(0.0).is_empty());
    assert!(c.candles(-5.0).is_empty());
    assert!(Chart::default().candles(900.0).is_empty());
}

/// The bucket must track the requested bar count and never go finer than the
/// data, which would just yield a row of one-tick bars.
#[test]
fn auto_bucket_tracks_the_requested_density() {
    let intraday = Chart::from_json(sc::result(&envelope(CHART)));
    let ytd = Chart::from_json(sc::result(&envelope(CHART_YTD)));

    for c in [&intraday, &ytd] {
        let spacing = c.median_spacing_secs().unwrap();
        for target in [30usize, 90, 250] {
            let b = c.auto_bucket_secs(target);
            assert!(b >= spacing, "bucket {b} finer than data spacing {spacing}");
            assert!(c.candles(b).len() <= c.points.len());
        }
        // Asking for fewer bars must not produce a finer bucket.
        assert!(c.auto_bucket_secs(30) >= c.auto_bucket_secs(250));
    }

    // A long series buckets coarser than an intraday one.
    assert!(ytd.auto_bucket_secs(90) > intraday.auto_bucket_secs(90));
    assert_eq!(Chart::default().auto_bucket_secs(90), 86_400.0);
}

/// Backoff bookkeeping. A lapsed hold must report itself clear, or polling never
/// resumes and the deferred chart retry never fires.
#[test]
fn backoff_reports_itself_clear_once_it_lapses() {
    use crate::worker::Shared;
    use std::time::{Duration, Instant};

    let mut s = Shared::default();
    assert!(s.backoff_secs_left().is_none(), "no hold by default");

    s.backoff_until = Some(Instant::now() + Duration::from_secs(90));
    let left = s.backoff_secs_left().expect("held off");
    assert!(
        (80..=91).contains(&left),
        "countdown reports ~90s, got {left}"
    );

    s.backoff_until = Some(Instant::now() - Duration::from_secs(1));
    assert!(
        s.backoff_secs_left().is_none(),
        "a lapsed hold must read clear"
    );

    // The measured recovery on this backend was 46s; the constant must exceed it.
    assert!(crate::worker::RATE_LIMIT_BACKOFF >= Duration::from_secs(60));
}

/// A rate-limited response is a distinct kind, because retrying it immediately
/// makes things worse. It must not be lumped in with generic failures.
#[test]
fn rate_limited_is_classified_separately() {
    use crate::sc::ScErrorKind;
    let kinds = [
        ScErrorKind::RateLimited,
        ScErrorKind::Auth,
        ScErrorKind::Network,
        ScErrorKind::Validation,
        ScErrorKind::Generic,
    ];
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == ScErrorKind::RateLimited)
            .count(),
        1
    );
    assert_ne!(ScErrorKind::RateLimited, ScErrorKind::Network);
    assert_ne!(ScErrorKind::RateLimited, ScErrorKind::Generic);
}

/// The error envelope arrives on stdout with exit 0, so it must be detected from
/// the `ok` flag rather than the process status.
#[test]
fn logical_failure_is_read_from_the_envelope() {
    let raw = r#"{"ok":false,"command":"whoami","error":{"code":"no_session","message":"No active session. Run 'sc login'."},"hints":["Run 'sc login' first to create a session."]}"#;
    let v: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(v["ok"], Value::Bool(false));
    assert_eq!(v["error"]["code"], "no_session");
}

const DERIVATIVES: &str = include_str!("../tests/fixtures/derivatives.json");

/// Derivatives search rows carry metrics only — no name, no quote. The
/// extractor must surface the risk numbers (leverage, barrier, distance to
/// knockout) exactly, since the table sorts trading decisions by them.
#[test]
fn derivatives_page_extracts_risk_metrics() {
    let data = envelope(DERIVATIVES);
    let p = DerivativesPage::from_json(sc::result(&data));

    assert_eq!(p.underlying, "US67066G1040");
    assert_eq!(p.derivative_type, "knockout");
    assert_eq!(p.items.len(), 5);
    assert_eq!(p.total_available, 8223);

    let d = p
        .items
        .iter()
        .find(|d| d.isin == "DE000CJ8P2E4")
        .expect("SocGen mini");
    assert_eq!(d.issuer, "SOC_GEN");
    assert_eq!(d.strategy, "LONG");
    assert_eq!(d.subcategory, "MINI_FUTURE");
    assert!((d.leverage.unwrap() - 1.0121381506).abs() < 1e-9);
    assert!(close(d.strike.unwrap(), 2.6022));
    assert_eq!(d.strike_currency, "USD");
    assert!(close(d.knockout_barrier.unwrap(), 2.7425));
    assert!(close(d.distance_to_knockout.unwrap(), 0.9875));
    assert!(close(d.premium_pct.unwrap(), -0.0001));
    assert!(d.open_end);
    assert!(
        d.expiry.is_empty(),
        "open-end products have a null expiry_date"
    );
    assert!(d.factor.is_none(), "knockouts carry leverage, not a factor");

    // Rows without an ISIN would be untradable — none may survive extraction.
    assert!(p.items.iter().all(|d| !d.isin.is_empty()));
}

/// A trailing stop must never loosen. The high water mark only rises, so the
/// suggested stop can only rise with it.
#[test]
fn trail_high_water_and_stop_only_rise() {
    let mut t = Trail::new("X", 0.03, true, 190.0);
    assert!(close(t.high_water, 190.0));
    assert!(close(t.suggested_stop().unwrap(), 184.3));

    assert!(t.observe(194.20), "a higher mid advances the mark");
    assert!(close(t.high_water, 194.20));
    assert!(close(t.suggested_stop().unwrap(), 188.374));

    // Falling prices change nothing at all.
    for p in [191.0, 185.0, 150.0, 1.0] {
        assert!(!t.observe(p), "{p} must not move the mark");
        assert!(close(t.high_water, 194.20));
        assert!(close(t.suggested_stop().unwrap(), 188.374));
    }

    // Garbage input must not poison the mark.
    assert!(!t.observe(f64::NAN));
    assert!(!t.observe(f64::INFINITY));
    assert!(close(t.high_water, 194.20));
}

/// Rounding is downward, so it can only ever move the stop further from the
/// market. Rounding up could trigger an exit that should not have happened.
#[test]
fn trail_rounds_the_stop_away_from_the_market() {
    let t = Trail::new("X", 0.037, true, 123.4567);
    let raw = 123.4567 * (1.0 - 0.037);
    let s = t.suggested_stop().unwrap();
    assert!(s <= raw, "rounded stop {s} must not exceed raw {raw}");
    assert!(raw - s < 1e-4);

    let abs = Trail::new("X", 2.5, false, 100.0);
    assert!(close(abs.suggested_stop().unwrap(), 97.5));
}

/// Replacing costs three calls and opens an unprotected window, so a trail must
/// not churn for a rounding error.
#[test]
fn trail_only_moves_for_a_worthwhile_improvement() {
    let mut t = Trail::new("X", 0.03, true, 200.0);
    let stop = t.suggested_stop().unwrap();
    assert!(close(stop, 194.0));

    // No resting stop at all: always worth placing one.
    assert!(t.should_move(None));

    // Already at the suggestion: nothing to do.
    assert!(!t.should_move(Some(stop)));

    // A resting stop above the suggestion is never dragged back down.
    assert!(!t.should_move(Some(stop + 5.0)));

    // Just under the minimum step is not worth the exposure.
    let tiny = stop - stop * (t.min_step * 0.5);
    assert!(!t.should_move(Some(tiny)));

    // A full step is.
    let worth = stop - stop * (t.min_step * 2.0);
    assert!(t.should_move(Some(worth)));

    // After a real rally the old stop is clearly stale.
    t.observe(240.0);
    assert!(t.should_move(Some(stop)));
    assert!(close(t.suggested_stop().unwrap(), 232.8));
}

/// A misconfigured trail must produce no suggestion rather than a nonsense one.
#[test]
fn trail_rejects_impossible_distances() {
    for (d, pct) in [
        (0.0, true),
        (-0.05, true),
        (1.0, true),
        (1.5, true),
        (0.0, false),
    ] {
        let t = Trail::new("X", d, pct, 100.0);
        assert!(!t.valid(), "distance {d} percent {pct} must be rejected");
        assert!(t.suggested_stop().is_none());
        assert!(!t.should_move(None));
    }

    // An absolute distance wider than the price leaves no stop to place.
    let t = Trail::new("X", 150.0, false, 100.0);
    assert!(t.valid());
    assert!(t.suggested_stop().is_none());
    assert!(!t.should_move(None));
}

/// The cushion is what the user actually reads: how far price can fall before
/// the stop is hit.
#[test]
fn trail_cushion_measures_distance_to_the_stop() {
    let t = Trail::new("X", 0.03, true, 200.0);
    // Against a resting stop, not the suggestion.
    let c = t.cushion(200.0, Some(190.0)).unwrap();
    assert!((c - 0.05).abs() < 1e-9);
    // With no resting stop it falls back to where the stop would go.
    let c = t.cushion(200.0, None).unwrap();
    assert!((c - 0.03).abs() < 1e-9);
    assert!(t.cushion(0.0, Some(190.0)).is_none());
}

/// `device_locked` arrives with exit code 20, the same as a real auth failure,
/// but the session is intact and the fix is to unlock the Mac. Classifying it as
/// an auth error would tell the user to log in again, which does nothing.
#[test]
fn device_locked_is_not_an_auth_failure() {
    use crate::sc::ScErrorKind;

    let raw = r#"{"ok":false,"command":"whoami","error":{"code":"device_locked","message":"The Mac is locked, so the Secure Enclave signing key cannot be used. Unlock the Mac and retry."},"hints":["If the Mac is already unlocked, check Secure Enclave and keychain access."]}"#;
    let v: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(v["ok"], Value::Bool(false));
    assert_eq!(v["error"]["code"], "device_locked");

    assert_ne!(ScErrorKind::DeviceLocked, ScErrorKind::Auth);
    assert_ne!(ScErrorKind::DeviceLocked, ScErrorKind::RateLimited);
    assert_ne!(ScErrorKind::DeviceLocked, ScErrorKind::Generic);
}

/// `watchlist add` answers `ok: true` even when the broker declines, and reports
/// the real outcome in `is_on_watchlist`. It refuses anything already held.
/// Trusting the envelope alone makes a failed add look like a success.
#[test]
fn watchlist_add_reports_refusal_inside_a_success_envelope() {
    let declined = r#"{"ok":true,"command":"broker.watchlist.add","data":{"result":{"action":"add","is_on_watchlist":false,"isin":"JP3228600007"}}}"#;
    let accepted = r#"{"ok":true,"command":"broker.watchlist.add","data":{"result":{"action":"add","is_on_watchlist":true,"isin":"US67066G1040"}}}"#;

    for (raw, expected) in [(declined, false), (accepted, true)] {
        let v: Value = serde_json::from_str(raw).unwrap();
        // The envelope says success in both cases.
        assert_eq!(v["ok"], Value::Bool(true));
        let on = sc::pick(sc::result(&v["data"]), &["is_on_watchlist"]).and_then(Value::as_bool);
        assert_eq!(on, Some(expected));
    }
}

/// Two actions on the same chord means one of them silently never fires, and
/// the help window would list both as working.
#[test]
fn shortcut_chords_are_unique() {
    use crate::shortcuts::BINDINGS;

    let mut seen: Vec<(egui::Key, bool, bool, bool)> = Vec::new();
    for b in BINDINGS {
        let chord = (b.key, b.mods.shift, b.mods.command, b.mods.alt);
        assert!(
            !seen.contains(&chord),
            "{:?} is bound twice: {} / {}",
            b.key,
            b.label,
            b.shown
        );
        seen.push(chord);
    }
    assert_eq!(seen.len(), BINDINGS.len());
}

/// The help window renders by group, so a binding in no known group would be
/// invisible while still working.
#[test]
fn every_shortcut_appears_in_the_help_window() {
    use crate::shortcuts::{BINDINGS, GROUPS};

    for b in BINDINGS {
        assert!(
            GROUPS.contains(&b.group),
            "{} is in group {:?}, which the help window does not render",
            b.label,
            b.group
        );
        assert!(!b.shown.is_empty(), "{} has no printable chord", b.label);
        assert!(!b.label.is_empty());
    }
    for g in GROUPS {
        assert!(
            BINDINGS.iter().any(|b| b.group == *g),
            "group {g:?} is rendered but empty"
        );
    }
}

/// Submitting an order must never be reachable from the keyboard. The only
/// order related binding is Preview, which runs phase one and places nothing.
#[test]
fn no_shortcut_can_place_an_order() {
    use crate::shortcuts::{Act, BINDINGS, UNBOUND};

    let order_acts: Vec<Act> = BINDINGS
        .iter()
        .map(|b| b.act)
        .filter(|a| matches!(a, Act::Preview | Act::CancelOrder | Act::ArmTrail))
        .collect();
    assert!(order_acts.contains(&Act::Preview));

    // Cancelling is destructive, so it must carry a modifier rather than sit on
    // a bare key next to the arrows used for navigation.
    let cancel = BINDINGS.iter().find(|b| b.act == Act::CancelOrder).unwrap();
    assert!(cancel.mods.command, "cancel must require a modifier");

    // Nothing in the table describes itself as submitting or confirming.
    for b in BINDINGS {
        let l = b.label.to_lowercase();
        assert!(
            !(l.contains("submit") || l.contains("confirm") || l.contains("move stop")),
            "{} must not be bound to a key",
            b.label
        );
    }

    // And the omission is documented rather than accidental.
    assert!(UNBOUND.iter().any(|(what, _)| what.contains("Submit")));
    assert!(
        UNBOUND
            .iter()
            .any(|(what, _)| what.contains("trailing stop"))
    );
    assert!(UNBOUND.iter().all(|(_, why)| !why.is_empty()));
}

/// A fixed pause is not enough when the limit is a rolling quota: retrying at
/// full rate after every pause trips it again and the app never recovers.
#[test]
fn backoff_doubles_and_then_caps() {
    use crate::worker::{RATE_LIMIT_BACKOFF, RATE_LIMIT_BACKOFF_MAX, backoff_for};
    use std::time::Duration;

    assert_eq!(backoff_for(0), RATE_LIMIT_BACKOFF);
    assert_eq!(backoff_for(1), RATE_LIMIT_BACKOFF * 2);
    assert_eq!(backoff_for(2), RATE_LIMIT_BACKOFF * 4);

    // Never shrinks as the level rises.
    for level in 0..12 {
        assert!(
            backoff_for(level + 1) >= backoff_for(level),
            "level {level}"
        );
    }

    // And never grows without bound, however many refusals arrive.
    for level in 0..64 {
        assert!(
            backoff_for(level) <= RATE_LIMIT_BACKOFF_MAX,
            "level {level}"
        );
    }
    assert_eq!(backoff_for(32), RATE_LIMIT_BACKOFF_MAX);

    // The first pause has to be longer than the 46 seconds recovery measured
    // against the live backend, or the retry lands while still refused.
    assert!(backoff_for(0) >= Duration::from_secs(60));
}

/// After a pause the app must spend one call finding out whether the limit has
/// lifted. Retrying the whole watchlist is what kept it in a refusal loop.
#[test]
fn probe_round_polls_a_single_instrument() {
    use crate::worker::poll_list;

    let all: Vec<String> = ["A", "B", "C", "D"].iter().map(|s| s.to_string()).collect();

    assert_eq!(
        poll_list(all.clone(), false).len(),
        4,
        "normal rounds poll everything"
    );
    assert_eq!(
        poll_list(all.clone(), true).len(),
        1,
        "a probe costs one call"
    );
    assert_eq!(poll_list(all, true)[0], "A");

    // Nothing to poll stays nothing, in either mode.
    assert!(poll_list(Vec::new(), true).is_empty());
    assert!(poll_list(Vec::new(), false).is_empty());

    // A single instrument is already its own probe.
    let one = vec!["X".to_string()];
    assert_eq!(poll_list(one.clone(), true), one);
    assert_eq!(poll_list(one.clone(), false), one);
}

/// Local lists exist so a held instrument can be tracked anywhere: the broker
/// refuses those, which is the whole reason this store is separate.
#[test]
fn local_lists_accept_anything_and_stay_deduplicated() {
    use crate::workspace::WatchList;

    let mut l = WatchList::new("Momentum");
    assert!(l.add("de000a2e4t77"), "new entry");
    assert_eq!(l.isins, vec!["DE000A2E4T77"], "normalised to upper case");

    assert!(!l.add("DE000A2E4T77"), "already present");
    assert!(!l.add("  de000a2e4t77  "), "same after trimming");
    assert_eq!(l.isins.len(), 1);

    assert!(!l.add("   "), "blank is not an instrument");
    assert!(!l.add(""));
    assert_eq!(l.isins.len(), 1);

    // A held instrument is fine here, unlike on the broker list.
    assert!(l.add("JP3228600007"));
    l.remove("DE000A2E4T77");
    assert_eq!(l.isins, vec!["JP3228600007"]);
    l.remove("NOT_THERE");
    assert_eq!(l.isins.len(), 1);
}

/// Order is the user's ranking, so it has to be data rather than incidental.
#[test]
fn reordering_is_clamped_and_order_preserving() {
    use crate::workspace::WatchList;

    let mut l = WatchList::new("L");
    for i in ["A", "B", "C", "D"] {
        l.add(i);
    }

    l.move_by("C", -1);
    assert_eq!(l.isins, vec!["A", "C", "B", "D"]);

    // Past either end clamps rather than wrapping or panicking.
    l.move_by("A", -5);
    assert_eq!(l.isins, vec!["A", "C", "B", "D"]);
    l.move_by("D", 9);
    assert_eq!(l.isins, vec!["A", "C", "B", "D"]);

    l.move_by("A", 3);
    assert_eq!(l.isins, vec!["C", "B", "D", "A"]);

    l.move_by("MISSING", 1);
    assert_eq!(l.isins.len(), 4, "unknown entry changes nothing");
}

/// Deleting a list shifts every index after it. Leaving the active selection
/// alone would silently switch the user to a different list.
#[test]
fn deleting_a_list_rewrites_the_active_selection() {
    use crate::workspace::{ListId, Workspace};

    let mut w = Workspace::default();
    assert_eq!(w.active, ListId::Broker);
    w.create("Momentum");
    w.create("CAN SLIM");
    w.create("Income");

    // Deleting a list before the active one shifts it down.
    w.active = ListId::Local(2);
    w.delete(0);
    assert_eq!(w.active, ListId::Local(1));
    assert_eq!(w.name_of(w.active), "Income");

    // Deleting the active list falls back rather than dangling.
    w.delete(1);
    assert_eq!(w.active, ListId::Broker);

    // Out of range deletes are ignored.
    let before = w.lists.len();
    w.delete(99);
    assert_eq!(w.lists.len(), before);
}

/// A stored file can name a list that no longer exists. Without repair the
/// strip would come up empty with no way back.
#[test]
fn a_stale_active_list_falls_back_to_the_broker() {
    use crate::workspace::{ListId, Workspace};

    let mut w = Workspace {
        active: ListId::Local(7),
        ..Default::default()
    };
    w.repair();
    assert_eq!(w.active, ListId::Broker);

    w.create("Only");
    w.active = ListId::Local(0);
    w.repair();
    assert_eq!(w.active, ListId::Local(0), "a valid selection survives");
}

/// Holdings must always be priced, whichever list is on screen, or position
/// profit is marked against a stale quote.
#[test]
fn rows_and_poll_set_cover_the_right_instruments() {
    use crate::workspace::{ListId, Workspace};

    let broker = vec!["AAA".to_string(), "BBB".to_string()];
    let held = vec!["HELD1".to_string(), "HELD2".to_string()];

    let mut w = Workspace::default();

    // The broker list shows holdings too, since they cannot be stored on it.
    let rows = w.rows(&broker, &held);
    assert_eq!(rows, vec!["AAA", "BBB", "HELD1", "HELD2"]);

    w.active = ListId::Positions;
    assert_eq!(w.rows(&broker, &held), held);

    let id = w.create("Momentum");
    w.local_mut(id).unwrap().add("AAA");
    w.active = id;
    assert_eq!(
        w.rows(&broker, &held),
        vec!["AAA"],
        "only what the list holds"
    );

    // But polling still covers holdings, and never duplicates.
    let poll = w.poll_set(&broker, &held);
    assert_eq!(poll, vec!["AAA", "HELD1", "HELD2"]);

    w.local_mut(id).unwrap().add("HELD1");
    let poll = w.poll_set(&broker, &held);
    assert_eq!(
        poll.iter().filter(|i| *i == "HELD1").count(),
        1,
        "deduplicated"
    );
}

/// A row with no quote yet must still say what it is. Names come from whichever
/// endpoint answered first, and an instrument's name does not change, so the
/// cache is safe to keep and reuse.
#[test]
fn names_are_learned_from_every_endpoint() {
    let mut names: std::collections::HashMap<String, String> = Default::default();

    // From the broker watchlist.
    let wl = envelope(WATCHLIST);
    for i in sc::result(&wl)["items"].as_array().unwrap() {
        let q = Quote::from_watchlist_item(i);
        if !q.name.is_empty() {
            names.insert(q.isin.clone(), q.name.clone());
        }
    }
    assert!(names.contains_key("IE00B8GKDB10"));
    assert_eq!(
        names["IE00B8GKDB10"],
        "Vanguard FTSE All-World High Dividend Yield (Dist)"
    );

    // From holdings.
    let hd = envelope(HOLDINGS);
    for h in Holding::list_from(sc::result(&hd)) {
        names.insert(h.isin.clone(), h.name.clone());
    }
    assert_eq!(names["JP3228600007"], "Kansai El. Power");

    // From a full quote.
    let q = Quote::from_json("CA53056H1047", sc::result(&envelope(QUOTE)));
    names.insert(q.isin.clone(), q.name.clone());
    assert_eq!(names["CA53056H1047"], "Liberty Gold");

    // Every name learned is non empty, or the fallback is pointless.
    assert!(names.values().all(|n| !n.is_empty()));
}

/// Scalable exposes no sector for an instrument, so tags are the only way to
/// group. Free text that is not normalised would split "AI" and "ai" into two
/// groups that never rank against each other.
#[test]
fn tags_are_normalised_and_deduplicated() {
    use crate::workspace::Workspace;

    let mut w = Workspace::default();
    assert!(w.add_tag("AAA", "AI"));
    assert!(!w.add_tag("AAA", "ai"), "same tag in another case");
    assert!(!w.add_tag("AAA", "  Ai  "), "same tag with padding");
    assert_eq!(w.tags_of("AAA"), ["ai"]);

    assert!(!w.add_tag("AAA", "   "), "blank is not a tag");
    assert!(!w.add_tag("", "ai"), "needs an instrument");

    // Kept sorted, so the column reads the same way every time.
    w.add_tag("AAA", "uranium");
    w.add_tag("AAA", "cyber");
    assert_eq!(w.tags_of("AAA"), ["ai", "cyber", "uranium"]);

    assert!(w.has_tag("AAA", "cyber"));
    assert!(!w.has_tag("AAA", "gold"));
    assert!(!w.has_tag("BBB", "ai"));

    // Removing the last tag drops the entry rather than leaving an empty list.
    w.add_tag("BBB", "gold");
    w.remove_tag("BBB", "gold");
    assert!(w.tags_of("BBB").is_empty());
    assert!(!w.tags.contains_key("BBB"));

    // The filter row needs the union across instruments, deduplicated.
    w.add_tag("CCC", "ai");
    w.add_tag("CCC", "semis");
    assert_eq!(w.all_tags(), ["ai", "cyber", "semis", "uranium"]);
}

/// Relative strength is the point: every window the quote carries must survive
/// extraction, not just the intraday one the header uses.
#[test]
fn quote_keeps_every_performance_window() {
    use crate::model::Window;

    let q = Quote::from_json("CA53056H1047", sc::result(&envelope(QUOTE)));

    assert!(close(q.perf.day.unwrap(), 4.52));
    assert!(close(q.perf.week.unwrap(), 4.19));
    assert!(close(q.perf.month.unwrap(), 10.67));
    assert!(close(q.perf.quarter.unwrap(), 36.34));
    assert!(close(q.perf.half.unwrap(), 77.02));
    assert!(close(q.perf.year.unwrap(), 269.94));

    // Lookup by window matches the field, for all of them.
    assert_eq!(q.perf.get(Window::Week), q.perf.week);
    assert_eq!(q.perf.get(Window::Quarter), q.perf.quarter);
    for w in Window::ALL {
        assert!(q.perf.get(w).is_some(), "{} missing", w.label());
    }

    // A quote with no performance block yields no windows rather than zeros,
    // so an unranked instrument cannot masquerade as flat.
    let seeded = Quote::from_watchlist_item(&serde_json::json!({"isin": "X"}));
    assert!(Window::ALL.iter().all(|w| seeded.perf.get(*w).is_none()));
}

const ALERTS: &str = include_str!("../tests/fixtures/price-alerts.json");

/// Direction is derived by the broker from where the price sits at creation, so
/// the extractor must carry it rather than the UI inferring its own.
#[test]
fn price_alerts_extract_direction_and_state() {
    let data = envelope(ALERTS);
    let alerts = PriceAlert::list_from(sc::result(&data));

    assert_eq!(alerts.len(), 2);
    assert!(
        alerts.iter().all(|a| !a.id.is_empty()),
        "an alert with no id cannot be removed"
    );
    assert!(alerts.iter().all(|a| a.isin == "IE00B8GKDB10"));
    assert!(alerts.iter().all(|a| a.active));
    assert!(alerts.iter().all(|a| !a.has_triggered()));

    // Above the market fires UP, below fires DOWN.
    let up = alerts
        .iter()
        .find(|a| a.direction == "UP")
        .expect("an up alert");
    let down = alerts
        .iter()
        .find(|a| a.direction == "DOWN")
        .expect("a down alert");
    assert!(close(up.price.unwrap(), 999.0));
    assert!(close(down.price.unwrap(), 10.0));
    assert!(up.price > down.price, "UP sits above DOWN");
    assert_eq!(up.security_type, "ETF");
    assert!(!up.name.is_empty());
}

/// The distance is what tells you whether an alert is near firing. Its sign has
/// to match the direction, or a far away alert could read as imminent.
#[test]
fn alert_distance_is_signed_towards_the_trigger() {
    let data = envelope(ALERTS);
    let alerts = PriceAlert::list_from(sc::result(&data));
    let mid = 80.9;

    let up = alerts.iter().find(|a| a.direction == "UP").unwrap();
    let down = alerts.iter().find(|a| a.direction == "DOWN").unwrap();

    let du = up.distance(mid).unwrap();
    let dd = down.distance(mid).unwrap();
    assert!(du > 0.0, "an UP alert is above the market");
    assert!(dd < 0.0, "a DOWN alert is below it");

    // An alert sitting at the market has no distance left to travel.
    let at = PriceAlert {
        price: Some(mid),
        ..Default::default()
    };
    assert!(close(at.distance(mid).unwrap(), 0.0));

    // No usable mid yields no distance rather than a divide by zero.
    assert!(up.distance(0.0).is_none());
    assert!(PriceAlert::default().distance(mid).is_none());
}

/// A stored file can hold anything, including values a newer build would never
/// write. Trusting it would divide by zero or poll at an impossible rate.
#[test]
fn prefs_are_clamped_on_load() {
    use crate::workspace::Prefs;

    let mut p = Prefs {
        poll_secs: -5.0,
        bars_target: 0,
        timeframe: "fortnight".into(),
        ..Default::default()
    };
    p.repair();
    assert_eq!(
        p.poll_secs,
        Prefs::default().poll_secs,
        "negative interval rejected"
    );
    assert_eq!(p.bars_target, 20, "a zero bar target would divide by zero");
    assert_eq!(p.timeframe, "1d", "unknown timeframe falls back");

    // Absurd values are bounded rather than rejected outright.
    let mut p = Prefs {
        poll_secs: 1e9,
        bars_target: 100_000,
        ..Default::default()
    };
    p.repair();
    assert_eq!(p.poll_secs, 3600.0);
    assert_eq!(p.bars_target, 400);

    // Not a number cannot survive, or the poll loop never runs again.
    let mut p = Prefs {
        poll_secs: f32::NAN,
        ..Default::default()
    };
    p.repair();
    assert!(p.poll_secs.is_finite());

    // Zero is legitimate: it means paused.
    let mut p = Prefs {
        poll_secs: 0.0,
        ..Default::default()
    };
    p.repair();
    assert_eq!(p.poll_secs, 0.0);

    // Every timeframe the chart offers must survive a round trip.
    for tf in crate::worker::TIMEFRAMES {
        let mut p = Prefs {
            timeframe: tf.into(),
            ..Default::default()
        };
        p.repair();
        assert_eq!(p.timeframe, tf);
    }
}

/// Preferences must survive a write and read, or nothing is actually persisted.
#[test]
fn prefs_round_trip_through_json() {
    use crate::model::Window;
    use crate::workspace::{Prefs, Workspace};

    let mut w = Workspace {
        prefs: Prefs {
            poll_secs: 42.0,
            timeframe: "3m".into(),
            candles: false,
            sma: [true, false, true],
            bars_target: 120,
            sort_by: Some(Window::Quarter),
            sort_desc: false,
        },
        ..Default::default()
    };
    w.create("Momentum");
    w.add_tag("AAA", "ai");

    let text = serde_json::to_string(&w).unwrap();
    let back: Workspace = serde_json::from_str(&text).unwrap();

    assert_eq!(back.prefs, w.prefs);
    assert_eq!(back.prefs.sort_by, Some(Window::Quarter));
    assert_eq!(back.lists.len(), 1);
    assert_eq!(back.tags_of("AAA"), ["ai"]);

    // A file written before prefs existed must still load, with defaults.
    let old = r#"{"lists":[],"active":"Broker"}"#;
    let back: Workspace = serde_json::from_str(old).unwrap();
    assert_eq!(back.prefs, Prefs::default());
}

const NEWS: &str = include_str!("../tests/fixtures/security-news.json");

/// Like `broker.chart`, this payload sits directly under `data` rather than
/// `data.result`. Unwrapping the wrong level yields an empty panel.
#[test]
fn security_news_extracts_summary_and_headlines() {
    let data = envelope(NEWS);
    assert!(
        data.get("result").is_none(),
        "no result wrapper on this endpoint"
    );

    let n = SecurityNews::from_json(sc::result(&data));
    assert_eq!(n.isin, "US0231351067");
    assert_eq!(n.locale, "en_DE");
    assert!(!n.is_empty());

    assert!(n.short.contains("Generac"));
    assert!(n.long.contains("Project Mercury"));
    assert!(!n.last_updated.is_empty());

    assert_eq!(n.sources.len(), 3);
    for item in &n.sources {
        assert!(
            !item.headline.is_empty(),
            "a headline with no text is not worth a row"
        );
        assert_eq!(item.source, "dpa-AFX");
        // The date prefix is sliced for display, so it must be long enough.
        assert!(item.published.len() >= 10, "{:?}", item.published);
    }
}

/// Coverage is uneven: smaller instruments return nothing. That is a normal
/// answer and must be distinguishable from a failed call.
#[test]
fn news_with_no_coverage_is_empty_not_an_error() {
    let raw = r#"{"ok":true,"command":"broker.security-news","data":{"isin":"CA53056H1047","locale":"en_DE","sources":[],"summary":null}}"#;
    let v: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(v["ok"], Value::Bool(true), "the call succeeded");

    let n = SecurityNews::from_json(sc::result(&v["data"]));
    assert_eq!(n.isin, "CA53056H1047");
    assert!(n.is_empty(), "no summary and no headlines");
    assert!(n.short.is_empty() && n.long.is_empty());
    assert!(n.sources.is_empty());

    // A fully absent payload must not panic either.
    assert!(SecurityNews::from_json(&serde_json::json!({})).is_empty());
}

/// The style grid is cap crossed with value, blend or growth. Contributors name
/// which holding sits in each cell, which is what makes the grid actionable
/// rather than decorative.
#[test]
fn analytics_extracts_the_style_grid() {
    use crate::model::{CAPS, STYLES};

    let data = envelope(ANALYTICS);
    let a = Analytics::from_json(sc::result(&data));

    assert_eq!(a.styles.len(), 3);
    for c in &a.styles {
        assert!(CAPS.contains(&c.cap.as_str()), "unexpected cap {:?}", c.cap);
        assert!(
            STYLES.contains(&c.style.as_str()),
            "unexpected style {:?}",
            c.style
        );
        assert!(c.weight > 0.0);
        assert!(
            !c.holdings.is_empty(),
            "a cell with weight must name its holdings"
        );
    }

    let large_value = a.style_cell("LARGE", "VALUE").expect("large value");
    assert!((large_value.weight - 0.4917647059).abs() < 1e-9);
    assert_eq!(large_value.holdings, ["Kansai El. Power"]);

    let small_growth = a.style_cell("SMALL", "GROWTH").expect("small growth");
    assert_eq!(small_growth.holdings, ["Liberty Gold"]);

    // Empty cells are absent rather than zero weight entries.
    assert!(a.style_cell("MID", "VALUE").is_none());
    assert!(a.style_cell("SMALL", "VALUE").is_none());

    // Margins agree with the cells, and the whole grid sums to the book.
    assert!((a.cap_weight("LARGE") - 0.8063235296).abs() < 1e-9);
    assert!((a.style_total("VALUE") - 0.4917647059).abs() < 1e-9);
    let total: f64 = a.styles.iter().map(|c| c.weight).sum();
    assert!(
        (total - 1.0).abs() < 1e-6,
        "grid covers the equity book, got {total}"
    );
}

/// Income and credit quality come from the same payload and were being thrown
/// away. Absent buckets must read as zero rather than as missing data.
#[test]
fn analytics_extracts_income_and_credit_buckets() {
    let data = envelope(ANALYTICS);
    let a = Analytics::from_json(sc::result(&data));

    assert_eq!(a.distributions, Some(0.0));
    assert_eq!(a.interest, Some(0.0));

    // This portfolio holds no bonds, so every bucket is empty.
    assert_eq!(a.investment_grade, 0);
    assert_eq!(a.speculative_grade, 0);
    assert_eq!(a.unrated_grade, 0);
    assert!(!a.speculative_warning);

    // A payload with no analytics at all must not panic or invent values.
    let empty = Analytics::from_json(&serde_json::json!({}));
    assert!(empty.styles.is_empty());
    assert_eq!(empty.distributions, None);
    assert_eq!(empty.investment_grade, 0);
}
