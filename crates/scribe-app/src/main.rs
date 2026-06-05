//! SCR1B3 — standalone cross-platform code/text editor.
//!
//! A "better Notepad++": fast, telemetry-free, not bloated, modern. This binary
//! is the egui/eframe shell over `scribe-core` (engine) + `scribe-render`
//! (theme mapping + rope-editor widget). Frameless window with a custom brand
//! titlebar.
//!
//! Phase 21 T21.2 P1 — `#![forbid(unsafe_code)]`. The egui shell is pure-safe
//! Rust over eframe; no `unsafe` is ever needed at this layer.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

mod action_log;
mod app;
mod cli;
mod editor_features;
mod filetree;
mod fuzzy;
mod grid;
mod plugin_manager;
mod settings;
mod theme_editor;
mod updater;

use std::process::ExitCode;
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    // F-007 fix from docs/audits/overlooked-surfaces-2026-05-29.md: parse
    // --help / --version / PATH[:LINE[:COLUMN]] BEFORE we spin up eframe so
    // the binary behaves like a normal CLI for the "scr1b3 --help" / "scr1b3
    // --version" surfaces every shell user expects.
    let cli_action = cli::parse(std::env::args().skip(1));
    match cli_action {
        cli::Action::Help => {
            println!("{}", cli::help_text());
            return ExitCode::SUCCESS;
        }
        cli::Action::Version => {
            println!("{}", cli::version_text());
            return ExitCode::SUCCESS;
        }
        cli::Action::Error(msg) => {
            eprintln!("scr1b3: {msg}");
            eprintln!("try 'scr1b3 --help' for usage");
            return ExitCode::from(2);
        }
        cli::Action::Launch { .. } => {}
    }

    // Local-only structured logging. OFF-by-default verbosity; honors RUST_LOG.
    // No remote telemetry — ever.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let (config, config_err) = scribe_core::Config::load_or_default();

    // Window geometry (position + size) is persisted natively by eframe via
    // `NativeOptions.persist_window` + the `persistence` feature (stored under
    // the `with_app_id` folder). We set only the FIRST-RUN default size here;
    // eframe restores the user's last position + size on subsequent launches.
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1100.0, 720.0])
        .with_min_inner_size([520.0, 360.0])
        // Explicitly resizable: with decorations OFF the OS resize borders are
        // gone, so the in-app `ViewportCommand::BeginResize` handler is the ONLY
        // way to resize — and that command is a no-op unless the window is
        // resizable. (egui defaults this true, but a frameless window makes it
        // load-bearing, so we pin it.)
        .with_resizable(true)
        .with_app_id("com.itashacorp.scr1b3")
        .with_title(scribe_core::PRODUCT_NAME);
    // Runtime window + taskbar icon. The embedded .exe resource (build.rs +
    // winresource) covers Explorer / Alt-Tab / pre-launch on Windows; this sets
    // the live window + taskbar icon at runtime (and is the icon source on
    // Linux/Wayland, where there is no .exe resource). Non-fatal on decode error.
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../assets/scr1b3-256.png"))
    {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    // F-035: keep the window on top when the user has enabled it.
    if config.window.always_on_top {
        viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnTop);
    }
    if config.appearance.frameless {
        viewport = viewport.with_decorations(false);
    }
    // A transparent surface is required for frameless rounded corners AND for
    // any translucent/glass window mode (so the OS blur / desktop shows through).
    // egui-wgpu then selects a PreMultiplied/PostMultiplied composite-alpha-mode
    // (see egui-wgpu 0.29 winit.rs) — but only if the painted content is itself
    // non-opaque, which `effective_translucent()` drives in the shell.
    if config.appearance.frameless || config.window.effective_translucent() {
        viewport = viewport.with_transparent(true);
    }

    let native_options = eframe::NativeOptions {
        viewport,
        // Persist native window position + size across restarts (pairs with the
        // eframe `persistence` feature + the stable `with_app_id` above). eframe
        // also fires `App::save()` on exit/interval once persistence is on.
        persist_window: true,
        ..Default::default()
    };

    // Re-parse here so we can hand the path to ScribeApp::new. (Parsing is
    // pure and idempotent — same args, same Action.)
    let cli_path = match cli::parse(std::env::args().skip(1)) {
        cli::Action::Launch { path: Some(p), .. } => p.to_string_lossy().into_owned().into(),
        _ => None,
    };

    let result = eframe::run_native(
        scribe_core::PRODUCT_NAME,
        native_options,
        Box::new(move |cc| {
            Ok(Box::new(app::ScribeApp::new(
                cc, config, config_err, cli_path,
            )))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("scr1b3: fatal: {e}");
            ExitCode::FAILURE
        }
    }
}
