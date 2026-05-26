//! SCR1B3 — standalone cross-platform code/text editor.
//!
//! A "better Notepad++": fast, telemetry-free, not bloated, modern. This binary
//! is the egui/eframe shell over `scribe-core` (engine) + `scribe-render`
//! (theme/CRT mapping). Frameless window with a custom brand titlebar.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod editor_features;
mod filetree;
mod settings;

use tracing_subscriber::EnvFilter;

fn main() -> eframe::Result<()> {
    // Local-only structured logging. OFF-by-default verbosity; honors RUST_LOG.
    // No remote telemetry — ever.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let (config, config_err) = scribe_core::Config::load_or_default();

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1100.0, 720.0])
        .with_min_inner_size([520.0, 360.0])
        .with_app_id("com.itashacorp.scr1b3")
        .with_title(scribe_core::PRODUCT_NAME);
    if config.appearance.frameless {
        viewport = viewport.with_decorations(false);
    }
    // A transparent surface is required for frameless rounded corners AND for
    // any translucent/glass window mode (so the OS blur / desktop shows through).
    if config.appearance.frameless || config.window.mode.is_translucent() {
        viewport = viewport.with_transparent(true);
    }

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let cli_path = std::env::args().nth(1);

    eframe::run_native(
        scribe_core::PRODUCT_NAME,
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(app::ScribeApp::new(
                cc, config, config_err, cli_path,
            )))
        }),
    )
}
