//! SCR1B3 editor core.
//!
//! Engine for a fast, telemetry-free, not-bloated code/text editor:
//! rope-backed buffers, large-file `mmap` browsing, encoding/EOL handling,
//! TOML config + theming, syntect syntax highlighting, and regex search.
//!
//! This crate has NO UI dependency — it is the replaceable engine behind the
//! `scribe-render` + `scribe-app` shell.

pub mod config;
pub mod document;
pub mod encoding;
pub mod eol;
pub mod error;
pub mod lsp;
pub mod plugin;
pub mod search;
pub mod spell;
pub mod syntax;
pub mod theme;
pub mod update;

pub use config::Config;
pub use document::Document;
pub use error::{CoreError, Result};
pub use theme::Theme;

/// Product identity constants (public-repo-safe; no internal references).
pub const PRODUCT_NAME: &str = "SCR1B3";
pub const PRODUCT_TAGLINE: &str = "present day, present text";
pub const CONFIG_DIR_NAME: &str = "scr1b3";
