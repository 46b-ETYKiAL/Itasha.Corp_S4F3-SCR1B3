//! SCR1B3 — standalone cross-platform code/text editor.
//!
//! A "better Notepad++": fast, telemetry-free, not bloated, modern. This binary
//! is the egui/eframe shell over `scribe-core` (engine) + `scribe-render`
//! (theme/CRT mapping). Frameless window with a custom brand titlebar.
//!
//! Phase 21 T21.2 P1 — `#![forbid(unsafe_code)]`. The egui shell is pure-safe
//! Rust over eframe; no `unsafe` is ever needed at this layer.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

mod app;
mod cli;
mod editor_features;
mod filetree;
mod fuzzy;
mod grid;
mod plugin_manager;
mod settings;

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

    // F-020 from docs/audits/overlooked-surfaces-2026-05-29.md: restore the
    // last known window geometry. Fall back to the hard-coded default size
    // when no geometry was persisted (first launch).
    let (init_x, init_w, init_h) = match config.window.last_geometry {
        Some((x, y, w, h)) if w >= 200.0 && h >= 150.0 => (Some((x, y)), w, h),
        _ => (None, 1100.0, 720.0),
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([init_w, init_h])
        .with_min_inner_size([520.0, 360.0])
        .with_app_id("com.itashacorp.scr1b3")
        .with_title(scribe_core::PRODUCT_NAME);
    if let Some((x, y)) = init_x {
        viewport = viewport.with_position([x, y]);
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
