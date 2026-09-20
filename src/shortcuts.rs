//! Keyboard shortcuts.
//!
//! One table drives both the handler and the help window, so a binding can never
//! be listed and not work, or work and not be listed.

use egui::{Key, Modifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    ViewChart,
    ViewDerivatives,
    ViewPortfolio,
    ViewLog,
    ViewRaw,
    PrevInstrument,
    NextInstrument,
    FocusSearch,
    FocusAdd,
    Help,
    Refresh,
    RefreshQuotes,
    TogglePause,
    PrevTimeframe,
    NextTimeframe,
    ToggleCandles,
    Sma20,
    Sma50,
    Sma200,
    ResetZoom,
    SideBuy,
    SideSell,
    TypeMarket,
    TypeLimit,
    TypeStop,
    ToggleSizeMode,
    CyclePrice,
    Preview,
    CancelOrder,
    ArmTrail,
}

pub struct Binding {
    pub act: Act,
    pub key: Key,
    pub mods: Modifiers,
    /// How the chord is written in the help window.
    pub shown: &'static str,
    pub label: &'static str,
    pub group: &'static str,
}

const NAV: &str = "Navigation";
const DATA: &str = "Data";
const CHART: &str = "Chart";
const TICKET: &str = "Ticket";
const ORDERS: &str = "Positions and orders";

macro_rules! b {
    ($act:ident, $key:ident, $mods:expr, $shown:expr, $label:expr, $group:expr) => {
        Binding {
            act: Act::$act,
            key: Key::$key,
            mods: $mods,
            shown: $shown,
            label: $label,
            group: $group,
        }
    };
}

pub const BINDINGS: &[Binding] = &[
    b!(ViewChart, Num1, Modifiers::NONE, "1", "Chart", NAV),
    b!(
        ViewDerivatives,
        Num2,
        Modifiers::NONE,
        "2",
        "Derivatives",
        NAV
    ),
    b!(ViewPortfolio, Num3, Modifiers::NONE, "3", "Portfolio", NAV),
    b!(ViewLog, Num4, Modifiers::NONE, "4", "Log", NAV),
    b!(ViewRaw, Num5, Modifiers::NONE, "5", "Raw", NAV),
    b!(
        PrevInstrument,
        ArrowUp,
        Modifiers::NONE,
        "Up",
        "Previous instrument",
        NAV
    ),
    b!(
        NextInstrument,
        ArrowDown,
        Modifiers::NONE,
        "Down",
        "Next instrument",
        NAV
    ),
    b!(
        FocusSearch,
        Slash,
        Modifiers::NONE,
        "/",
        "Focus the search field",
        NAV
    ),
    b!(
        FocusAdd,
        A,
        Modifiers::NONE,
        "A",
        "Focus the add ISIN field",
        NAV
    ),
    b!(
        Help,
        Questionmark,
        Modifiers::SHIFT,
        "?",
        "This window",
        NAV
    ),
    b!(Refresh, R, Modifiers::NONE, "R", "Refresh everything", DATA),
    b!(
        RefreshQuotes,
        R,
        Modifiers::SHIFT,
        "Shift R",
        "Refresh quotes only",
        DATA
    ),
    b!(
        TogglePause,
        Space,
        Modifiers::NONE,
        "Space",
        "Pause or resume polling",
        DATA
    ),
    b!(
        PrevTimeframe,
        OpenBracket,
        Modifiers::NONE,
        "[",
        "Previous timeframe",
        CHART
    ),
    b!(
        NextTimeframe,
        CloseBracket,
        Modifiers::NONE,
        "]",
        "Next timeframe",
        CHART
    ),
    b!(
        ToggleCandles,
        C,
        Modifiers::NONE,
        "C",
        "Candles or line",
        CHART
    ),
    b!(Sma20, Z, Modifiers::NONE, "Z", "Toggle SMA 20", CHART),
    b!(Sma50, X, Modifiers::NONE, "X", "Toggle SMA 50", CHART),
    b!(Sma200, V, Modifiers::NONE, "V", "Toggle SMA 200", CHART),
    b!(ResetZoom, F, Modifiers::NONE, "F", "Reset zoom", CHART),
    b!(SideBuy, B, Modifiers::NONE, "B", "Side buy", TICKET),
    b!(SideSell, S, Modifiers::NONE, "S", "Side sell", TICKET),
    b!(TypeMarket, M, Modifiers::NONE, "M", "Market order", TICKET),
    b!(TypeLimit, L, Modifiers::NONE, "L", "Limit order", TICKET),
    b!(TypeStop, T, Modifiers::NONE, "T", "Stop order", TICKET),
    b!(
        ToggleSizeMode,
        Q,
        Modifiers::NONE,
        "Q",
        "Size by shares or amount",
        TICKET
    ),
    b!(
        CyclePrice,
        P,
        Modifiers::NONE,
        "P",
        "Limit price: bid, mid, ask",
        TICKET
    ),
    b!(
        Preview,
        Enter,
        Modifiers::NONE,
        "Enter",
        "Preview, places nothing",
        TICKET
    ),
    b!(
        CancelOrder,
        Backspace,
        Modifiers::COMMAND,
        "Cmd Backspace",
        "Cancel selected working order",
        ORDERS
    ),
    b!(
        ArmTrail,
        T,
        Modifiers::SHIFT,
        "Shift T",
        "Arm a trail on the selection",
        ORDERS
    ),
];

pub const GROUPS: &[&str] = &[NAV, DATA, CHART, TICKET, ORDERS];

/// Actions with no binding, and the reason. Shown in the help window so the
/// absence reads as a decision rather than an oversight.
pub const UNBOUND: &[(&str, &str)] = &[
    (
        "Submit an order",
        "stays behind typing CONFIRM, so a stray keypress can never fill",
    ),
    (
        "Move a trailing stop",
        "cancels a live stop, so it costs a deliberate click",
    ),
];

/// Which action, if any, was pressed this frame.
///
/// Returns nothing while a text field has focus, otherwise typing an ISIN
/// beginning with A would fight the shortcut for the ISIN field.
pub fn pressed(ctx: &egui::Context) -> Option<Act> {
    if ctx.egui_wants_keyboard_input() {
        return None;
    }
    ctx.input(|i| {
        BINDINGS
            .iter()
            .find(|b| i.key_pressed(b.key) && i.modifiers.matches_exact(b.mods))
            .map(|b| b.act)
    })
}
