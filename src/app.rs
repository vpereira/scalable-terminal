use crate::model::*;
use crate::worker::{self, Cmd, Handle, TradeIntent, TIMEFRAMES};
use egui::{Color32, RichText};
use egui_extras::{Column, TableBuilder};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const GREEN: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
const RED: Color32 = Color32::from_rgb(0xe5, 0x53, 0x4a);
const AMBER: Color32 = Color32::from_rgb(0xd2, 0x9d, 0x2b);
const DIM: Color32 = Color32::from_rgb(0x8b, 0x8b, 0x8b);
const BLUE: Color32 = Color32::from_rgb(0x58, 0x9b, 0xd6);

pub const SMA_PERIODS: [usize; 3] = [20, 50, 200];
const SMA_COLOURS: [Color32; 3] = [
    Color32::from_rgb(0xf2, 0xc4, 0x4c),
    Color32::from_rgb(0x8b, 0x7f, 0xe8),
    Color32::from_rgb(0xe8, 0x7f, 0xb8),
];

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Portfolio,
    Chart,
    Log,
    Raw,
}

/// Headless capture: render a few frames so data lands, snapshot the viewport,
/// write a PNG and exit. The app runs as a background process on macOS and will
/// not take focus, so `screencapture` grabs whatever is in front instead — this
/// is the only reliable way to see what the terminal actually renders.
pub struct Shot {
    pub path: std::path::PathBuf,
    /// Delay before capturing, so the data has actually arrived.
    pub warmup_secs: f32,
    pub tab: Option<String>,
}

pub struct App {
    io: Handle,
    shot: Option<Shot>,
    shot_started: Option<std::time::Instant>,
    shot_sent: bool,
    poll: Arc<Mutex<Duration>>,
    poll_secs: f32,
    selected: Option<String>,
    new_isin: String,
    search_query: String,
    tab: Tab,
    timeframe: String,
    sma: [bool; 3],
    candles: bool,
    bars_target: usize,
    side: Side,
    order_type: OrderType,
    size_by_shares: bool,
    amount: f64,
    shares: f64,
    limit_price: f64,
    stop_price: f64,
    venue: String,
    accept_unsuitable: bool,
    confirm_typed: String,
}

impl App {
    pub fn with_shot(cc: &eframe::CreationContext<'_>, shot: Option<Shot>) -> Self {
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);
        cc.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        });

        let poll = Arc::new(Mutex::new(Duration::from_secs(10)));
        let io = worker::spawn(cc.egui_ctx.clone(), poll.clone());
        let _ = io.tx.send(Cmd::RefreshAll);

        let tab = shot
            .as_ref()
            .and_then(|s| s.tab.as_deref())
            .map(|t| match t {
                "chart" => Tab::Chart,
                "log" => Tab::Log,
                "raw" => Tab::Raw,
                _ => Tab::Portfolio,
            })
            .unwrap_or(Tab::Portfolio);

        App {
            io,
            shot,
            shot_started: None,
            shot_sent: false,
            poll,
            poll_secs: 10.0,
            selected: None,
            new_isin: String::new(),
            search_query: String::new(),
            tab,
            timeframe: "1d".into(),
            sma: [false, false, false],
            candles: true,
            bars_target: 90,
            side: Side::Buy,
            order_type: OrderType::Limit,
            size_by_shares: false,
            amount: 500.0,
            shares: 1.0,
            limit_price: 0.0,
            stop_price: 0.0,
            venue: String::new(),
            accept_unsuitable: false,
            confirm_typed: String::new(),
        }
    }

    fn intent(&self) -> TradeIntent {
        TradeIntent {
            isin: self.selected.clone().unwrap_or_default(),
            side: self.side,
            order_type: self.order_type,
            amount: (!self.size_by_shares && self.side == Side::Buy).then_some(self.amount),
            shares: (self.size_by_shares || self.side == Side::Sell).then_some(self.shares),
            limit_price: (self.order_type == OrderType::Limit).then_some(self.limit_price),
            stop_price: (self.order_type == OrderType::Stop).then_some(self.stop_price),
            venue: (!self.venue.trim().is_empty()).then(|| self.venue.trim().to_string()),
        }
    }

    fn select(&mut self, isin: String) {
        self.selected = Some(isin.clone());
        self.load_chart(isin, false);
    }

    fn drive_screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &self.shot else { return };
        if self.shot_started.is_none() {
            self.shot_started = Some(std::time::Instant::now());
        }
        // A capture that never arrives must not hang the process.
        if let Some(t0) = self.shot_started {
            if t0.elapsed() > std::time::Duration::from_secs(60) {
                eprintln!("screenshot timed out after 60s");
                std::process::exit(2);
            }
        }
        // Keep repainting: without input egui would idle before the data arrives.
        ctx.request_repaint();

        let warmed_up = self
            .shot_started
            .is_some_and(|t0| t0.elapsed().as_secs_f32() >= shot.warmup_secs);
        if warmed_up && !self.shot_sent {
            self.shot_sent = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }

        let captured = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });

        if let Some(img) = captured {
            let [w, h] = [img.width() as u32, img.height() as u32];
            let buf: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
            match image::RgbaImage::from_raw(w, h, buf) {
                Some(rgba) => match rgba.save(&shot.path) {
                    Ok(()) => eprintln!("screenshot written: {} ({w}x{h})", shot.path.display()),
                    Err(e) => eprintln!("screenshot save failed: {e}"),
                },
                None => eprintln!("screenshot buffer mismatch"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Pick something the moment data lands, so the chart tab is not an empty
    /// panel that reads as "unimplemented".
    fn autoselect(&mut self) {
        if self.selected.is_some() {
            return;
        }
        let first = {
            let s = self.io.state.lock().unwrap();
            s.holdings
                .first()
                .map(|h| h.isin.clone())
                .or_else(|| s.watchlist.first().cloned())
        };
        if let Some(isin) = first {
            self.select(isin);
        }
    }

    fn load_chart(&self, isin: String, force: bool) {
        let _ = self.io.tx.send(Cmd::LoadChart { isin, timeframe: self.timeframe.clone(), force });
    }
}

fn num(v: Option<f64>, dp: usize) -> String {
    match v {
        Some(x) => format!("{x:.dp$}"),
        None => "—".into(),
    }
}

fn signed_text(v: Option<f64>, dp: usize, suffix: &str) -> RichText {
    match v {
        Some(x) => {
            let c = if x > 0.0 { GREEN } else if x < 0.0 { RED } else { DIM };
            RichText::new(format!("{x:+.dp$}{suffix}")).color(c).monospace()
        }
        None => RichText::new("—").color(DIM).monospace(),
    }
}

fn signed(ui: &mut egui::Ui, v: Option<f64>, dp: usize, suffix: &str) {
    ui.label(signed_text(v, dp, suffix));
}

/// Big number with a caption. Fixed width so a row of these wraps as whole
/// cells rather than breaking each number across lines.
fn stat(ui: &mut egui::Ui, caption: &str, value: String, color: Color32) {
    ui.allocate_ui(egui::vec2(170.0, 46.0), |ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(caption).color(DIM).small());
            ui.label(
                RichText::new(value).color(color).monospace().size(19.0),
            );
        });
    });
}

/// `SIX_MONTHS` is too wide for a stat row; traders read `6M`.
fn short_timeframe(tf: &str) -> &str {
    match tf {
        "INTRADAY" => "1D",
        "TWO_DAYS" => "2D",
        "ONE_WEEK" => "1W",
        "ONE_MONTH" => "1M",
        "THREE_MONTHS" => "3M",
        "SIX_MONTHS" => "6M",
        "ONE_YEAR" => "1Y",
        "MAX" => "MAX",
        other => other,
    }
}

impl eframe::App for App {
    /// Panel widths live in egui memory. Persisting it means a sidebar dragged
    /// once keeps that width forever and `default_size` never applies again.
    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn on_exit(&mut self) {
        let _ = self.io.tx.send(Cmd::Shutdown);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.drive_screenshot(&ctx);
        // Nothing completes during a backoff, so nothing would request a repaint
        // and the countdown would sit frozen until the mouse moved.
        if self.io.state.lock().unwrap().backoff_secs_left().is_some() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        self.autoselect();
        self.top_bar(ui);
        self.left_panel(ui);
        self.right_panel(ui);
        self.central(ui);
        self.preview_modal(&ctx);
    }
}

impl App {
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("SCALABLE TERMINAL").strong().monospace());
                ui.separator();

                let s = self.io.state.lock().unwrap();
                match (&s.session, &s.session_error) {
                    (Some(who), _) => {
                        ui.label(RichText::new("●").color(GREEN));
                        ui.label(RichText::new(who).monospace());
                    }
                    (None, Some(err)) => {
                        ui.label(RichText::new("●").color(RED));
                        ui.label(RichText::new(format!("{err} — run `sc login`")).color(RED));
                    }
                    _ => {
                        ui.label(RichText::new("●").color(AMBER));
                        ui.label("connecting…");
                    }
                }
                let (round, calls, avg) = (s.last_round_ms, s.last_round_calls, s.quote_ms_avg);
                drop(s);

                ui.separator();
                // Polling is the whole latency story here — keep it on screen.
                ui.label(
                    RichText::new(format!("{calls} quotes / {round} ms ({avg:.0} ms per call)"))
                        .color(if round > 3000 { AMBER } else { DIM })
                        .monospace(),
                );

                ui.separator();
                ui.label("every");
                if ui
                    .add(egui::DragValue::new(&mut self.poll_secs).speed(0.5).range(0.0..=300.0).suffix(" s"))
                    .changed()
                {
                    *self.poll.lock().unwrap() = Duration::from_secs_f32(self.poll_secs);
                }
                if self.poll_secs == 0.0 {
                    ui.label(RichText::new("paused").color(AMBER));
                }

                if let Some(left) = { self.io.state.lock().unwrap().backoff_secs_left() } {
                    ui.separator();
                    ui.label(
                        RichText::new(format!("⏸ rate limited, resuming in {left}s"))
                            .color(AMBER)
                            .monospace(),
                    );
                }

                ui.separator();
                if ui.button("Refresh all").clicked() {
                    let _ = self.io.tx.send(Cmd::RefreshAll);
                }
                if ui.button("Quotes").clicked() {
                    let _ = self.io.tx.send(Cmd::RefreshQuotes);
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.selectable_value(&mut self.tab, Tab::Raw, "Raw");
                    ui.selectable_value(&mut self.tab, Tab::Log, "Log");
                    ui.selectable_value(&mut self.tab, Tab::Chart, "Chart");
                    ui.selectable_value(&mut self.tab, Tab::Portfolio, "Portfolio");
                });
            });
        });
    }

    fn left_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("watchlist").resizable(true).default_size(470.0).max_size(900.0).show(ui, |ui| {
            let (watchlist, quotes) = {
                let s = self.io.state.lock().unwrap();
                (s.watchlist.clone(), s.quotes.clone())
            };

            ui.horizontal(|ui| {
                ui.heading("Watchlist");
                ui.label(RichText::new("synced with your Scalable account").color(DIM).small());
            });

            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_isin).hint_text("ISIN").desired_width(150.0));
                if ui.button("Add").clicked() && self.new_isin.trim().len() >= 6 {
                    let isin = self.new_isin.trim().to_uppercase();
                    let _ = self.io.tx.send(Cmd::WatchlistAdd(isin.clone()));
                    self.select(isin);
                    self.new_isin.clear();
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.search_query)
                        .hint_text("search name or ticker")
                        .desired_width(200.0),
                );
                if ui.button("Search").clicked() && !self.search_query.trim().is_empty() {
                    let _ = self.io.tx.send(Cmd::Search(self.search_query.trim().to_string()));
                }
            });
            ui.separator();

            let rows: Vec<Quote> = watchlist
                .iter()
                .map(|i| quotes.get(i).cloned().unwrap_or(Quote { isin: i.clone(), ..Default::default() }))
                .collect();

            let mut remove: Option<String> = None;
            let mut pick: Option<String> = None;

            TableBuilder::new(ui)
                .striped(true)
                .cell_layout(egui::Layout::right_to_left(egui::Align::Center))
                .column(Column::exact(12.0))
                .column(Column::exact(118.0))
                .column(Column::remainder().at_least(120.0))
                .columns(Column::exact(74.0), 3)
                .column(Column::exact(66.0))
                .column(Column::exact(64.0))
                .column(Column::exact(22.0))
                .header(20.0, |mut h| {
                    for t in ["", "ISIN", "Name", "Bid", "Ask", "Mid", "Chg %", "Spr bps", ""] {
                        h.col(|ui| {
                            if !t.is_empty() {
                                ui.label(RichText::new(t).strong());
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(20.0, rows.len(), |mut row| {
                        let q = &rows[row.index()];
                        let is_sel = self.selected.as_deref() == Some(q.isin.as_str());

                        row.col(|ui| {
                            if q.outdated {
                                ui.label(RichText::new("!").color(AMBER)).on_hover_text("broker flags this quote stale");
                            } else if q.bid.is_some() {
                                ui.label(RichText::new("·").color(GREEN));
                            } else {
                                ui.label(RichText::new("·").color(DIM));
                            }
                        });
                        row.col(|ui| {
                            let t = RichText::new(&q.isin).monospace();
                            let t = if is_sel { t.color(Color32::WHITE).strong() } else { t };
                            if ui.selectable_label(is_sel, t).clicked() {
                                pick = Some(q.isin.clone());
                            }
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(if q.name.is_empty() { "—" } else { &q.name }).color(DIM))
                                .on_hover_text(&q.security_type);
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(num(q.bid, 4)).monospace().color(RED));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(num(q.ask, 4)).monospace().color(GREEN));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(num(q.mid, 4)).monospace().strong()).on_hover_text(format!(
                                "{} {}\nprev close {}\nchange {}\nas of {}",
                                num(q.mid, 4),
                                if q.currency.is_empty() { "—" } else { &q.currency },
                                num(q.prev_close, 4),
                                num(q.change_abs, 4),
                                if q.timestamp.is_empty() { "unknown" } else { &q.timestamp }
                            ));
                        });
                        row.col(|ui| signed(ui, q.change_pct, 2, "%"));
                        row.col(|ui| {
                            // Spread in bps: the execution cost the broker UI never states.
                            let c = match q.spread_bps() {
                                Some(b) if b > 100.0 => RED,
                                Some(b) if b > 30.0 => AMBER,
                                Some(_) => GREEN,
                                None => DIM,
                            };
                            ui.label(RichText::new(num(q.spread_bps(), 1)).color(c).monospace())
                                .on_hover_text(format!("absolute spread {}", num(q.spread_abs(), 4)));
                        });
                        row.col(|ui| {
                            if ui.small_button("x").clicked() {
                                remove = Some(q.isin.clone());
                            }
                        });
                    });
                });

            if let Some(p) = pick {
                self.select(p);
            }
            if let Some(r) = remove {
                let _ = self.io.tx.send(Cmd::WatchlistRemove(r));
            }

            let results = { self.io.state.lock().unwrap().search_results.clone() };
            if !results.is_empty() {
                ui.separator();
                ui.label(RichText::new("Search results").strong());
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    for r in results.iter().take(50) {
                        let isin = crate::sc::str_at(r, &["isin"]).unwrap_or_default();
                        let name = crate::sc::str_at(r, &["name"]).unwrap_or_default();
                        let mid = crate::sc::f64_at(r, &["quote_mid_price"]);
                        let stype = crate::sc::str_at(r, &["security_type"]).unwrap_or_default();
                        ui.horizontal(|ui| {
                            if ui.small_button("+").clicked() && !isin.is_empty() {
                                let _ = self.io.tx.send(Cmd::WatchlistAdd(isin.clone()));
                            }
                            if ui.selectable_label(false, RichText::new(&isin).monospace()).clicked() {
                                self.select(isin.clone());
                            }
                            ui.label(RichText::new(num(mid, 4)).monospace());
                            ui.label(RichText::new(&stype).color(BLUE).small());
                            ui.label(RichText::new(&name).color(DIM));
                        });
                    }
                });
            }
        });
    }

    fn right_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::right("book").resizable(true).default_size(430.0).max_size(900.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let acct = { self.io.state.lock().unwrap().account.clone() };

                ui.horizontal(|ui| {
                    ui.heading("Account");
                    ui.label(RichText::new(&acct.currency).color(DIM).monospace());
                });
                egui::Grid::new("acct").num_columns(2).spacing([18.0, 2.0]).show(ui, |ui| {
                    let r = |ui: &mut egui::Ui, k: &str, v: String| {
                        ui.label(RichText::new(k).color(DIM));
                        ui.label(RichText::new(v).monospace());
                        ui.end_row();
                    };
                    r(ui, "Total", num(acct.total, 2));
                    r(ui, "Securities", num(acct.securities, 2));
                    r(ui, "Cash", num(acct.cash, 2));
                    r(ui, "Buying power", num(acct.buying_power, 2));
                    if acct.pending_buy_orders.unwrap_or(0.0) != 0.0 {
                        r(ui, "Reserved by orders", num(acct.pending_buy_orders, 2));
                    }
                    if acct.possible_taxes.unwrap_or(0.0) != 0.0 {
                        r(ui, "Possible taxes", num(acct.possible_taxes, 2));
                    }
                });

                ui.separator();
                self.positions(ui);
                ui.separator();
                self.orders(ui);
                ui.separator();
                self.ticket(ui);
            });
        });
    }

    fn positions(&mut self, ui: &mut egui::Ui) {
        let (holdings, quotes, working) = {
            let s = self.io.state.lock().unwrap();
            (s.holdings.clone(), s.quotes.clone(), s.orders.clone())
        };

        ui.horizontal(|ui| {
            ui.heading(format!("Positions ({})", holdings.len()));
            let total: Option<f64> = {
                let v: Vec<f64> = holdings.iter().filter_map(|h| h.unrealized()).collect();
                (!v.is_empty()).then(|| v.iter().sum())
            };
            ui.label(RichText::new("unrealized").color(DIM).small());
            signed(ui, total, 2, "");
        });

        let mut pick: Option<String> = None;
        TableBuilder::new(ui)
            .striped(true)
            .cell_layout(egui::Layout::right_to_left(egui::Align::Center))
            .column(Column::exact(116.0))
            .column(Column::exact(52.0))
            .column(Column::exact(66.0))
            .column(Column::exact(66.0))
            .column(Column::exact(70.0))
            .column(Column::remainder().at_least(62.0))
            .header(20.0, |mut h| {
                for t in ["ISIN", "Qty", "Avg", "Mid", "Value", "P&L %"] {
                    h.col(|ui| {
                        ui.label(RichText::new(t).strong());
                    });
                }
            })
            .body(|body| {
                body.rows(20.0, holdings.len(), |mut row| {
                    let h = &holdings[row.index()];
                    // Prefer the live polled mid over the snapshot the holdings call returned.
                    let live = quotes.get(&h.isin).and_then(|q| q.mid).or(h.mid);
                    row.col(|ui| {
                        let r = ui
                            .selectable_label(
                                self.selected.as_deref() == Some(h.isin.as_str()),
                                RichText::new(&h.isin).monospace(),
                            )
                            .on_hover_text(format!(
                                "{}\n{} · cost basis {} {}",
                                if h.name.is_empty() { "—" } else { &h.name },
                                h.security_type,
                                num(h.cost_basis(), 2),
                                h.currency
                            ));
                        if r.clicked() {
                            pick = Some(h.isin.clone());
                        }
                    });
                    row.col(|ui| {
                        let free = h.free_quantity(&working);
                        let t = RichText::new(format!("{:.4}", h.quantity)).monospace();
                        // Amber when some or all of the position is already on the market.
                        let t = if free < h.quantity { t.color(AMBER) } else { t };
                        ui.label(t).on_hover_text(format!(
                            "free to sell {free:.4}\nblocked {:.4} · pending {:.4}\nresting sells {:.4}",
                            h.blocked,
                            h.pending,
                            (h.quantity - h.blocked - free).max(0.0)
                        ));
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(num(h.fifo_price, 4)).monospace());
                    });
                    row.col(|ui| {
                        let t = RichText::new(num(live, 4)).monospace();
                        let t = if h.outdated { t.color(AMBER) } else { t };
                        ui.label(t).on_hover_text(if h.timestamp.is_empty() {
                            "no quote timestamp".to_string()
                        } else if h.outdated {
                            format!("broker flags this mark stale\n{}", h.timestamp)
                        } else {
                            h.timestamp.clone()
                        });
                    });
                    row.col(|ui| {
                        ui.label(RichText::new(num(h.valuation, 2)).monospace());
                    });
                    row.col(|ui| {
                        ui.label(signed_text(h.unrealized_pct(), 2, "%"))
                            .on_hover_text(format!("{} {}", num(h.unrealized(), 2), h.currency));
                    });
                });
            });

        if let Some(p) = pick {
            self.select(p);
        }
    }

    fn orders(&mut self, ui: &mut egui::Ui) {
        let orders = { self.io.state.lock().unwrap().orders.clone() };
        ui.heading(format!("Working orders ({})", orders.len()));
        if orders.is_empty() {
            ui.label(RichText::new("none").color(DIM));
            return;
        }
        let mut cancel: Option<String> = None;
        let mut pick: Option<String> = None;
        for o in &orders {
            ui.horizontal(|ui| {
                let c = if o.side.eq_ignore_ascii_case("SELL") { RED } else { GREEN };
                ui.label(RichText::new(&o.side).color(c).monospace().strong());
                if ui
                    .selectable_label(
                        self.selected.as_deref() == Some(o.isin.as_str()),
                        RichText::new(&o.isin).monospace(),
                    )
                    .clicked()
                {
                    pick = Some(o.isin.clone());
                }
                match o.quantity {
                    Some(q) => {
                        ui.label(RichText::new(format!("{q:.4}")).monospace());
                    }
                    None => {
                        ui.label(
                            RichText::new(format!("{} {}", num(o.amount, 2), o.currency)).monospace(),
                        );
                    }
                }
                if let Some(l) = o.limit_price {
                    ui.label(RichText::new(format!("lmt {l:.4}")).monospace().color(BLUE));
                }
                if let Some(sp) = o.stop_price {
                    ui.label(RichText::new(format!("stp {sp:.4}")).monospace().color(AMBER));
                }
                if ui.small_button("cancel").clicked() {
                    cancel = Some(o.id.clone());
                }
            });
            ui.label(
                RichText::new(format!("   {} · {} · {}", o.description, o.status, o.last_event))
                    .color(DIM)
                    .small(),
            );
        }
        if let Some(p) = pick {
            self.select(p);
        }
        if let Some(id) = cancel {
            let _ = self.io.tx.send(Cmd::CancelOrder(id));
        }
    }

    fn ticket(&mut self, ui: &mut egui::Ui) {
        ui.heading("Order ticket");
        let isin = self.selected.clone().unwrap_or_default();
        if isin.is_empty() {
            ui.label(RichText::new("select an instrument").color(DIM));
            return;
        }
        let (q, held, working) = {
            let s = self.io.state.lock().unwrap();
            (
                s.quotes.get(&isin).cloned(),
                s.holdings.iter().find(|h| h.isin == isin).cloned(),
                s.orders.clone(),
            )
        };

        ui.horizontal(|ui| {
            ui.label(RichText::new(&isin).monospace().strong());
            if let Some(q) = &q {
                ui.label(RichText::new(format!("bid {}", num(q.bid, 4))).monospace().color(RED));
                ui.label(RichText::new(format!("ask {}", num(q.ask, 4))).monospace().color(GREEN));
                if let Some(b) = q.spread_bps() {
                    ui.label(RichText::new(format!("{b:.1} bps")).color(DIM).monospace());
                }
            }
        });

        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.side, Side::Buy, RichText::new("BUY").color(GREEN).strong());
            ui.selectable_value(&mut self.side, Side::Sell, RichText::new("SELL").color(RED).strong());
            ui.separator();
            ui.selectable_value(&mut self.order_type, OrderType::Market, "Market");
            ui.selectable_value(&mut self.order_type, OrderType::Limit, "Limit");
            ui.selectable_value(&mut self.order_type, OrderType::Stop, "Stop");
        });

        if self.side == Side::Sell {
            self.size_by_shares = true;
            match &held {
                Some(h) => {
                    let free = h.free_quantity(&working);
                    let committed = h.quantity - free;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("holding {:.4}", h.quantity)).color(DIM));
                        if committed > 0.0 {
                            ui.label(
                                RichText::new(format!("· {committed:.4} already working"))
                                    .color(AMBER)
                                    .small(),
                            );
                        }
                        ui.label(RichText::new(format!("· free {free:.4}")).color(if free > 0.0 {
                            GREEN
                        } else {
                            RED
                        }));
                        if ui.add_enabled(free > 0.0, egui::Button::new("all").small()).clicked() {
                            self.shares = free;
                        }
                        if ui.add_enabled(free > 0.0, egui::Button::new("half").small()).clicked() {
                            self.shares = (free / 2.0 * 10_000.0).floor() / 10_000.0;
                        }
                    });
                    if self.shares > free {
                        ui.label(
                            RichText::new(format!(
                                "sizing {:.4} but only {free:.4} free — the rest is committed to working orders",
                                self.shares
                            ))
                            .color(AMBER),
                        );
                    }
                }
                None => {
                    ui.label(RichText::new("no position in this instrument").color(AMBER));
                }
            }
        } else {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.size_by_shares, false, "€ amount");
                ui.selectable_value(&mut self.size_by_shares, true, "shares");
            });
        }

        ui.horizontal(|ui| {
            if self.size_by_shares {
                ui.label("Shares");
                ui.add(egui::DragValue::new(&mut self.shares).speed(0.1).range(0.0..=1e9));
                if let Some(p) = q.as_ref().and_then(|q| q.mid) {
                    ui.label(RichText::new(format!("≈ {:.2}", p * self.shares)).color(DIM).monospace());
                }
            } else {
                ui.label("Amount");
                ui.add(egui::DragValue::new(&mut self.amount).speed(10.0).range(0.0..=1e9).suffix(" €"));
                if let Some(p) = q.as_ref().and_then(|q| q.ask.or(q.mid)) {
                    if p > 0.0 {
                        ui.label(RichText::new(format!("≈ {:.4} sh", self.amount / p)).color(DIM).monospace());
                    }
                }
            }
        });

        if self.order_type == OrderType::Limit {
            ui.horizontal(|ui| {
                ui.label("Limit");
                ui.add(egui::DragValue::new(&mut self.limit_price).speed(0.01).range(0.0..=1e9));
                if ui.small_button("bid").clicked() {
                    if let Some(b) = q.as_ref().and_then(|q| q.bid) {
                        self.limit_price = b;
                    }
                }
                if ui.small_button("mid").clicked() {
                    if let Some(m) = q.as_ref().and_then(|q| q.mid) {
                        self.limit_price = m;
                    }
                }
                if ui.small_button("ask").clicked() {
                    if let Some(a) = q.as_ref().and_then(|q| q.ask) {
                        self.limit_price = a;
                    }
                }
            });
        }
        if self.order_type == OrderType::Stop {
            ui.horizontal(|ui| {
                ui.label("Stop");
                ui.add(egui::DragValue::new(&mut self.stop_price).speed(0.01).range(0.0..=1e9));
            });
        }

        ui.horizontal(|ui| {
            ui.label("Venue");
            ui.add(egui::TextEdit::singleline(&mut self.venue).hint_text("default").desired_width(120.0));
        });

        let pending = { self.io.state.lock().unwrap().preview_pending };
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let label = if self.side == Side::Buy { "Preview BUY" } else { "Preview SELL" };
            if ui.add_enabled(!pending, egui::Button::new(RichText::new(label).strong())).clicked() {
                self.confirm_typed.clear();
                self.accept_unsuitable = false;
                let _ = self.io.tx.send(Cmd::PreviewTrade(self.intent()));
            }
            if pending {
                ui.spinner();
            }
        });

        let s = self.io.state.lock().unwrap();
        if let Some(e) = &s.preview_error {
            ui.label(RichText::new(e.clone()).color(RED));
        }
        if let Some(r) = &s.order_result {
            ui.label(RichText::new(r.clone()).color(GREEN));
        }
        if let Some(e) = &s.order_error {
            ui.label(RichText::new(e.clone()).color(RED));
        }
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Portfolio => self.portfolio_view(ui),
            Tab::Chart => self.chart_view(ui),
            Tab::Log => self.log_view(ui),
            Tab::Raw => self.raw_view(ui),
        });
    }

    fn portfolio_view(&mut self, ui: &mut egui::Ui) {
        let (acct, holdings, analytics) = {
            let s = self.io.state.lock().unwrap();
            (s.account.clone(), s.holdings.clone(), s.analytics.clone())
        };

        let unrealized: Option<f64> = {
            let v: Vec<f64> = holdings.iter().filter_map(|h| h.unrealized()).collect();
            (!v.is_empty()).then(|| v.iter().sum())
        };
        let cost: f64 = holdings.iter().filter_map(|h| h.cost_basis()).sum();
        let unrealized_pct = (cost.abs() > 1e-9).then(|| unrealized.unwrap_or(0.0) / cost * 100.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                stat(ui, "TOTAL", num(acct.total, 2), Color32::WHITE);
                stat(ui, "SECURITIES", num(acct.securities, 2), BLUE);
                stat(ui, "CASH", num(acct.cash, 2), DIM);
                let c = match unrealized {
                    Some(x) if x > 0.0 => GREEN,
                    Some(x) if x < 0.0 => RED,
                    _ => DIM,
                };
                stat(
                    ui,
                    "UNREALIZED",
                    match (unrealized, unrealized_pct) {
                        (Some(u), Some(p)) => format!("{u:+.2} ({p:+.1}%)"),
                        _ => "—".into(),
                    },
                    c,
                );
            });
            if !acct.valuation_ts.is_empty() {
                ui.label(RichText::new(format!("valued {}", acct.valuation_ts)).color(DIM).small());
            }

            ui.add_space(10.0);
            ui.separator();
            ui.heading("Performance");
            ui.label(
                RichText::new("absolute return per timeframe, in account currency")
                    .color(DIM)
                    .small(),
            );
            let perf = acct.performance_ordered();
            if perf.is_empty() {
                ui.label(RichText::new("—").color(DIM));
            } else {
                ui.horizontal_wrapped(|ui| {
                    for (tf, v) in perf {
                        ui.allocate_ui(egui::vec2(78.0, 34.0), |ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(short_timeframe(&tf)).color(DIM).small());
                                ui.label(signed_text(Some(v), 2, ""));
                            });
                        })
                        .response
                        .on_hover_text(tf.replace('_', " "));
                    }
                });
            }

            ui.add_space(10.0);
            ui.separator();
            ui.heading("Holdings");
            let total_sec = acct.securities.unwrap_or(0.0);
            egui::ScrollArea::horizontal().id_salt("holdings_h").show(ui, |ui| {
            TableBuilder::new(ui)
                .striped(true)
                .cell_layout(egui::Layout::right_to_left(egui::Align::Center))
                .column(Column::exact(118.0))
                .column(Column::initial(170.0).at_least(90.0).clip(true))
                .column(Column::exact(62.0))
                .column(Column::exact(56.0))
                .column(Column::exact(74.0))
                .column(Column::exact(74.0))
                .column(Column::exact(80.0))
                .column(Column::exact(86.0))
                .column(Column::exact(70.0))
                .header(20.0, |mut h| {
                    for t in ["ISIN", "Name", "Weight", "Qty", "Avg", "Mid", "Value", "P&L", "P&L %"] {
                        h.col(|ui| {
                            ui.label(RichText::new(t).strong());
                        });
                    }
                })
                .body(|body| {
                    body.rows(20.0, holdings.len(), |mut row| {
                        let h = &holdings[row.index()];
                        row.col(|ui| {
                            ui.label(RichText::new(&h.isin).monospace());
                        });
                        row.col(|ui| {
                            ui.label(&h.name).on_hover_text(&h.security_type);
                        });
                        row.col(|ui| {
                            let w = if total_sec > 0.0 {
                                h.valuation.map(|v| v / total_sec * 100.0)
                            } else {
                                None
                            };
                            ui.label(RichText::new(num(w, 1)).color(BLUE).monospace());
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(format!("{:.4}", h.quantity)).monospace());
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(num(h.fifo_price, 4)).monospace());
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(num(h.mid, 4)).monospace());
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(num(h.valuation, 2)).monospace());
                        });
                        row.col(|ui| { ui.label(signed_text(h.unrealized(), 2, "")); });
                        row.col(|ui| { ui.label(signed_text(h.unrealized_pct(), 2, "%")); });
                    });
                });
            });

            ui.add_space(10.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.heading("Allocation");
                if !analytics.last_updated.is_empty() {
                    ui.label(
                        RichText::new(format!("analytics as of {}", analytics.last_updated))
                            .color(DIM)
                            .small(),
                    );
                }
            });
            for (kind, slices) in &analytics.allocations {
                ui.collapsing(kind.replace('_', " "), |ui| {
                    for s in slices {
                        alloc_bar(ui, &s.name, s.weight, s.valuation, 0);
                        for sub in &s.subs {
                            alloc_bar(ui, &sub.name, sub.weight, sub.valuation, 1);
                        }
                    }
                });
            }

            if !analytics.health.is_empty() {
                ui.add_space(10.0);
                ui.separator();
                ui.heading("Diversification");
                for (kind, score, state, held, max) in &analytics.health {
                    ui.horizontal(|ui| {
                        let c = match state.as_str() {
                            "HIGH" => GREEN,
                            "MID" => AMBER,
                            _ => RED,
                        };
                        ui.label(RichText::new(format!("{kind:<12}")).monospace().color(DIM));
                        ui.add(egui::ProgressBar::new(*score as f32).desired_width(220.0).fill(c));
                        ui.label(RichText::new(format!("{held}/{max} · {state}")).color(DIM).small());
                    });
                }
            }

            if !analytics.scenarios.is_empty() {
                ui.add_space(10.0);
                ui.separator();
                ui.heading("Stress scenarios");
                ui.label(RichText::new("modelled move of your book vs its benchmark").color(DIM).small());
                egui::Grid::new("scen").num_columns(3).spacing([20.0, 3.0]).show(ui, |ui| {
                    ui.label(RichText::new("Scenario").strong());
                    ui.label(RichText::new("Portfolio").strong());
                    ui.label(RichText::new("Benchmark").strong());
                    ui.end_row();
                    for (name, port, bench) in &analytics.scenarios {
                        ui.label(RichText::new(name.replace('_', " ")).color(DIM));
                        ui.label(signed_text(Some(*port), 2, "%"));
                        ui.label(signed_text(Some(*bench), 2, "%"));
                        ui.end_row();
                    }
                });
            }
            ui.add_space(20.0);
        });
    }

    fn chart_view(&mut self, ui: &mut egui::Ui) {
        let chart = { self.io.state.lock().unwrap().chart.clone() };

        ui.horizontal(|ui| {
            ui.label(
                RichText::new(self.selected.clone().unwrap_or_else(|| "—".into()))
                    .monospace()
                    .strong(),
            );
            for tf in TIMEFRAMES {
                if ui.selectable_label(self.timeframe == tf, tf).clicked() {
                    self.timeframe = tf.to_string();
                    if let Some(isin) = self.selected.clone() {
                        self.load_chart(isin, false);
                    }
                }
            }
            ui.separator();
            ui.selectable_value(&mut self.candles, true, "Candles");
            ui.selectable_value(&mut self.candles, false, "Line");
            if self.candles {
                ui.add(
                    egui::DragValue::new(&mut self.bars_target)
                        .speed(1.0)
                        .range(20..=400)
                        .prefix("~")
                        .suffix(" bars"),
                )
                .on_hover_text("target bar count; the bucket snaps to the nearest standard interval");
            }
            if ui.small_button("reload").on_hover_text("bypass the chart cache").clicked() {
                if let Some(isin) = self.selected.clone() {
                    self.load_chart(isin, true);
                }
            }
            if !chart.source.is_empty() {
                ui.label(
                    RichText::new(format!(
                        "{} · {} · {} · {}",
                        chart.isin, chart.timeframe, chart.source, chart.currency
                    ))
                    .color(DIM)
                    .small(),
                );
            }
        });
        ui.separator();

        // SMA windows are calendar days, not observation counts — see
        // Chart::sma_days. A window longer than the loaded series is disabled
        // rather than drawn short.
        ui.horizontal(|ui| {
            ui.label(RichText::new("SMA (days)").color(DIM));
            for (i, n) in SMA_PERIODS.iter().enumerate() {
                let days = *n as f64;
                let ok = chart.supports_sma(days);
                let r = ui.add_enabled(ok, egui::Button::selectable(self.sma[i] && ok, format!("{n}")));
                if r.clicked() {
                    self.sma[i] = !self.sma[i];
                }
                r.on_hover_text(if ok {
                    format!(
                        "{n}-day average · ~{:.0} observations per window",
                        chart.points_per_window(days).unwrap_or(0.0)
                    )
                } else {
                    format!(
                        "{n}-day window needs {n} days of data; this timeframe spans {:.0}. Try a longer timeframe.",
                        chart.span_days()
                    )
                });
            }
            if let Some(g) = chart.median_spacing_secs() {
                ui.label(
                    RichText::new(format!(
                        "· {} points over {:.0}d, one every {}",
                        chart.points.len(),
                        chart.span_days(),
                        human_secs(g)
                    ))
                    .color(DIM)
                    .small(),
                );
            }
        });

        let (err, loading, backoff) = {
            let s = self.io.state.lock().unwrap();
            (s.chart_error.clone(), s.chart_loading, s.backoff_secs_left())
        };
        if let Some(left) = backoff {
            ui.label(
                RichText::new(format!(
                    "backend rate limit hit — all polling paused, resuming in {left}s"
                ))
                .color(AMBER),
            );
        }
        if loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("loading chart…").color(DIM));
            });
        }

        if chart.points.is_empty() {
            if let Some(e) = err {
                ui.label(RichText::new(e).color(RED));
            } else if !loading {
                ui.label(RichText::new("no chart data — pick an instrument on the left").color(DIM));
            }
            return;
        }

        // The endpoint returns mid ticks only, no OHLC, so this is a line — not
        // candles pretending to be candles.
        let pts: Vec<[f64; 2]> = chart.points.iter().map(|p| [p.t, p.mid]).collect();
        let last = chart.points.last().map(|p| p.mid);
        let up = match (last, chart.reference) {
            (Some(l), Some(r)) => l >= r,
            _ => true,
        };
        let colour = if up { GREEN } else { RED };

        ui.horizontal(|ui| {
            ui.label(RichText::new(num(last, 4)).monospace().size(20.0).color(colour));
            if let (Some(l), Some(r)) = (last, chart.reference) {
                if r.abs() > 1e-9 {
                    ui.label(signed_text(Some(l - r), 4, ""));
                    ui.label(signed_text(Some((l / r - 1.0) * 100.0), 2, "%"));
                }
            }
            if let Some(r) = chart.reference {
                ui.label(RichText::new(format!("prev close {r:.4}")).color(DIM).monospace())
                    .on_hover_text(&chart.reference_ts);
            }
            if let Some(p) = chart.points.last() {
                ui.label(RichText::new(&p.ts).color(DIM).small());
            }
        });

        let candles_spec: Option<(f64, Vec<Candle>)> = if self.candles {
            let bucket = chart.auto_bucket_secs(self.bars_target);
            let bars = chart.candles(bucket);
            (!bars.is_empty()).then_some((bucket, bars))
        } else {
            None
        };
        if let Some((bucket, bars)) = &candles_spec {
            ui.label(
                RichText::new(format!(
                    "{} candles of {} each, built from {} mid ticks (the API publishes no OHLC)",
                    bars.len(),
                    human_secs(*bucket),
                    chart.points.len()
                ))
                .color(DIM)
                .small(),
            );
        }

        // Axis density: a full date stamp per mark is unreadable on an intraday span.
        let span_secs = chart.span_days() * 86_400.0;

        egui_plot::Plot::new("px")
            .allow_scroll(true)
            .legend(egui_plot::Legend::default())
            .show_axes([true, true])
            .x_axis_formatter(move |mark, _| axis_time(mark.value, span_secs))
                        .label_formatter(|pos| {
                let p = match pos {
                    egui_plot::HoverPosition::NearDataPoint { position, .. } => position,
                    egui_plot::HoverPosition::Elsewhere { position } => position,
                };
                Some(format!("{}\n{:.4}", hhmm(p.x), p.y))
            })
            .show(ui, |p| {
                if let Some(r) = chart.reference {
                    p.hline(
                        egui_plot::HLine::new("prev close", r)
                            .stroke(egui::Stroke::new(1.0, DIM))
                            .style(egui_plot::LineStyle::dashed_dense()),
                    );
                }
                if let Some((bucket, bars)) = candles_spec.as_ref() {
                    // Two box plots, not one per bar: 90 separate plot items is
                    // needless work and clutters the legend.
                    let body = bucket * 0.62;
                    let elem = |c: &Candle| {
                        let (lo, hi) = if c.up() { (c.open, c.close) } else { (c.close, c.open) };
                        egui_plot::BoxElem::new(
                            c.t + bucket / 2.0,
                            egui_plot::BoxSpread::new(c.low, lo, c.close, hi, c.high),
                        )
                        .box_width(body)
                        .whisker_width(0.0)
                    };
                    let (up, down): (Vec<_>, Vec<_>) = bars.iter().partition(|c| c.up());
                    if !up.is_empty() {
                        p.box_plot(
                            egui_plot::BoxPlot::new("", up.iter().map(|c| elem(c)).collect())
                                .color(GREEN),
                        );
                    }
                    if !down.is_empty() {
                        p.box_plot(
                            egui_plot::BoxPlot::new("", down.iter().map(|c| elem(c)).collect())
                                .color(RED),
                        );
                    }
                } else {
                    p.line(
                        egui_plot::Line::new("mid", egui_plot::PlotPoints::from(pts))
                            .stroke(egui::Stroke::new(1.5, colour)),
                    );
                }
                for (i, n) in SMA_PERIODS.iter().enumerate() {
                    if !self.sma[i] {
                        continue;
                    }
                    let series = chart.sma_days(*n as f64);
                    if series.is_empty() {
                        continue;
                    }
                    p.line(
                        egui_plot::Line::new(
                            format!("SMA {n}d"),
                            egui_plot::PlotPoints::from(series),
                        )
                        .stroke(egui::Stroke::new(1.2, SMA_COLOURS[i])),
                    );
                }
            });
    }

    fn log_view(&mut self, ui: &mut egui::Ui) {
        let log = { self.io.state.lock().unwrap().log.clone() };
        ui.label(RichText::new("every `sc` invocation, timed, newest first").color(DIM));
        ui.separator();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for e in log.iter().rev().take(300) {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{:>6} ms", e.ms))
                            .monospace()
                            .color(if e.ms > 1500 { AMBER } else { DIM }),
                    );
                    ui.label(RichText::new(&e.cmd).monospace().color(if e.ok { GREEN } else { RED }));
                    ui.label(RichText::new(&e.detail).color(DIM).small());
                });
            }
        });
    }

    fn raw_view(&mut self, ui: &mut egui::Ui) {
        let (ov, hd, ch, q) = {
            let s = self.io.state.lock().unwrap();
            let q = self
                .selected
                .as_ref()
                .and_then(|i| s.quotes.get(i))
                .map(|q| q.raw.clone())
                .unwrap_or(serde_json::Value::Null);
            (s.overview_raw.clone(), s.holdings_raw.clone(), s.chart_raw.clone(), q)
        };
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (name, v) in [
                ("quote (selected)", &q),
                ("broker.overview", &ov),
                ("broker.holdings", &hd),
                ("broker.chart", &ch),
            ] {
                ui.collapsing(name, |ui| {
                    let txt = serde_json::to_string_pretty(v).unwrap_or_default();
                    ui.add(
                        egui::TextEdit::multiline(&mut txt.as_str())
                            .code_editor()
                            .desired_width(f32::INFINITY),
                    );
                });
            }
        });
    }

    /// Phase-2 gate. `sc capabilities` contractually requires the full phase-1
    /// disclosure to be shown and an explicit separate confirmation before submit.
    fn preview_modal(&mut self, ctx: &egui::Context) {
        let preview = { self.io.state.lock().unwrap().preview.clone() };
        let Some(p) = preview else { return };
        // The confirmation id ages in real time; keep the countdown honest.
        ctx.request_repaint_after(Duration::from_millis(500));

        let mut close = false;
        let mut submit = false;

        egui::Window::new("Confirm order")
            .collapsible(false)
            .resizable(true)
            .default_width(640.0)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let side_txt = if self.side == Side::Buy {
                    RichText::new("BUY").color(GREEN).strong()
                } else {
                    RichText::new("SELL").color(RED).strong()
                };
                ui.horizontal(|ui| {
                    ui.label(side_txt);
                    ui.label(RichText::new(self.selected.clone().unwrap_or_default()).monospace().strong());
                    ui.label(RichText::new(self.order_type.cmd()).color(DIM));
                });
                ui.separator();

                egui::Grid::new("pv").num_columns(2).spacing([20.0, 3.0]).show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, k: &str, v: String| {
                        ui.label(RichText::new(k).color(DIM));
                        ui.label(RichText::new(v).monospace());
                        ui.end_row();
                    };
                    row(ui, "Shares", num(p.shares, 6));
                    row(ui, "Est. volume", format!("{} {}", num(p.est_volume, 2), p.currency));
                    row(ui, "Bid / Ask", format!("{} / {}", num(p.bid, 4), num(p.ask, 4)));
                    row(ui, "Mid", num(p.mid, 4));
                    row(ui, "Spread", format!("{} bps", num(p.spread_bps(), 1)));
                    row(ui, "Quote time", p.quote_ts.clone());
                    row(ui, "Venue", format!("{} ({})", p.venue, p.venue_status));
                    row(ui, "Tradable", p.tradable.to_string());
                    row(ui, "Entry cost", format!("{} ({}%)", num(p.entry_cost, 2), num(p.entry_cost_pct, 3)));
                    row(ui, "Ongoing cost", num(p.ongoing_cost, 2));
                    row(ui, "Exit cost", num(p.exit_cost, 2));
                    row(ui, "Suitability", p.suitability_status.clone());
                    row(ui, "Confirmation id", p.confirmation_id.clone());
                    row(
                        ui,
                        "Valid for",
                        match p.seconds_left() {
                            Some(s) if s > 0 => format!("{s} s"),
                            Some(_) => "EXPIRED".into(),
                            None => "—".into(),
                        },
                    );
                });

                if p.quote_outdated {
                    ui.label(RichText::new("⚠ quote is flagged outdated by the broker").color(AMBER));
                }
                if !p.warning_title.is_empty() {
                    ui.separator();
                    ui.label(RichText::new(&p.warning_title).color(AMBER).strong());
                    ui.label(RichText::new(&p.warning_body).small());
                }
                if p.requires_accept_unsuitable {
                    ui.checkbox(
                        &mut self.accept_unsuitable,
                        RichText::new("Instrument marked unsuitable — accept and proceed").color(AMBER),
                    );
                }

                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Type CONFIRM to arm:");
                    ui.add(egui::TextEdit::singleline(&mut self.confirm_typed).desired_width(120.0));
                });

                let armed = self.confirm_typed.trim().eq_ignore_ascii_case("confirm")
                    && p.tradable
                    && !p.expired()
                    && (!p.requires_accept_unsuitable || self.accept_unsuitable);
                let pending = { self.io.state.lock().unwrap().order_pending };

                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(armed && !pending, egui::Button::new(RichText::new("SUBMIT").strong()))
                        .clicked()
                    {
                        submit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if pending {
                        ui.spinner();
                    }
                    if !p.tradable {
                        ui.label(RichText::new("not tradable right now").color(RED));
                    }
                    if p.expired() {
                        ui.label(RichText::new("confirmation expired — re-preview").color(RED));
                    }
                });

                ui.collapsing("raw phase-1 payload", |ui| {
                    let txt = serde_json::to_string_pretty(&p.raw).unwrap_or_default();
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut txt.as_str())
                                .code_editor()
                                .desired_width(f32::INFINITY),
                        );
                    });
                });

                let s = self.io.state.lock().unwrap();
                if let Some(e) = &s.order_error {
                    ui.label(RichText::new(e.clone()).color(RED));
                }
            });

        if submit {
            let _ = self.io.tx.send(Cmd::SubmitTrade {
                intent: self.intent(),
                confirmation_id: p.confirmation_id.clone(),
                accept_unsuitable: self.accept_unsuitable,
            });
            self.confirm_typed.clear();
        }
        if close {
            let _ = self.io.tx.send(Cmd::ClearPreview);
            self.confirm_typed.clear();
        }
    }
}

fn alloc_bar(ui: &mut egui::Ui, name: &str, weight: f64, valuation: f64, depth: usize) {
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * 16.0);
        ui.label(RichText::new(format!("{name:<18}")).monospace().color(if depth > 0 { DIM } else { Color32::GRAY }));
        ui.add(
            egui::ProgressBar::new(weight as f32)
                .desired_width(200.0)
                .fill(if depth > 0 { DIM } else { BLUE }),
        );
        ui.label(RichText::new(format!("{:>5.1}%", weight * 100.0)).monospace());
        ui.label(RichText::new(format!("{valuation:>9.2}")).monospace().color(DIM));
    });
}

fn human_secs(s: f64) -> String {
    if s >= 86_400.0 {
        format!("{:.1} d", s / 86_400.0)
    } else if s >= 3600.0 {
        format!("{:.1} h", s / 3600.0)
    } else {
        format!("{:.0} min", s / 60.0)
    }
}

/// Axis label scaled to the visible span: clock time intraday, dates beyond that.
fn axis_time(t: f64, span_secs: f64) -> String {
    let secs = t as i64;
    if secs < 1_000_000_000 {
        return String::new();
    }
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    if span_secs <= 2.0 * 86_400.0 {
        format!("{:02}:{:02}", rem / 3600, (rem % 3600) / 60)
    } else if span_secs <= 400.0 * 86_400.0 {
        format!("{d:02} {}", MONTHS[(m as usize).clamp(1, 12) - 1])
    } else {
        format!("{} {y}", MONTHS[(m as usize).clamp(1, 12) - 1])
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Axis/hover label for unix-second x values.
fn hhmm(t: f64) -> String {
    let secs = t as i64;
    if secs < 1_000_000_000 {
        return format!("{secs}");
    }
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, (rem % 3600) / 60)
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
