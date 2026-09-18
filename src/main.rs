#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod model;
mod sc;
mod shortcuts;
mod worker;

#[cfg(test)]
mod tests;

fn main() -> eframe::Result<()> {
    // `--screenshot <path> [secs]` renders, waits for data, writes a PNG and
    // exits. Used to verify what the UI actually renders; the app cannot be
    // focused from a script.
    let argv: Vec<String> = std::env::args().collect();
    let shot = argv.iter().position(|a| a == "--screenshot").map(|i| app::Shot {
        path: argv
            .get(i + 1)
            .cloned()
            .unwrap_or_else(|| "shot.png".into())
            .into(),
        warmup_secs: argv
            .get(i + 2)
            .and_then(|f| f.parse().ok())
            .unwrap_or(6.0),
        tab: argv
            .iter()
            .position(|a| a == "--tab")
            .and_then(|i| argv.get(i + 1))
            .cloned(),
        select: argv
            .iter()
            .position(|a| a == "--select")
            .and_then(|i| argv.get(i + 1))
            .cloned(),
    });

    // `--redact` hides the account holder's name, for screenshots and sharing.
    let redact = argv.iter().any(|a| a == "--redact");

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1720.0, 1020.0])
            .with_min_inner_size([1200.0, 700.0])
            .with_title("Scalable Terminal"),
        // Panel widths live in egui memory, which eframe persists by default —
        // so a once-dragged sidebar keeps its width forever and `default_size`
        // never takes effect.
        persist_window: false,
        ..Default::default()
    };
    eframe::run_native(
        "Scalable Terminal",
        opts,
        Box::new(move |cc| Ok(Box::new(app::App::with_shot(cc, shot, redact)))),
    )
}
