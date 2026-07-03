//! Test-only in-process log capture for asserting that important paths emit the
//! logs we expect. No new dependency — a tiny `tracing_subscriber::Layer` that
//! records each event's level + message into a shared buffer, installed for the
//! duration of a closure via `tracing::subscriber::with_default`.
//!
//! Usage:
//! ```ignore
//! let (_, logs) = log_capture::capture(|| do_the_thing());
//! assert!(logs.has(tracing::Level::WARN, "save failed"));
//! ```

#![cfg(test)]

use std::fmt;
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// A thread-safe handle to the captured `(level, message)` events.
#[derive(Clone, Default)]
pub(crate) struct Captured(Arc<Mutex<Vec<(Level, String)>>>);

impl Captured {
    /// True if any captured event at exactly `level` contains `substr`.
    pub(crate) fn has(&self, level: Level, substr: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .iter()
            .any(|(l, m)| *l == level && m.contains(substr))
    }

    /// True if any captured event (any level) contains `substr`.
    pub(crate) fn any(&self, substr: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .iter()
            .any(|(_, m)| m.contains(substr))
    }

    /// All captured events (for assertion diagnostics).
    pub(crate) fn events(&self) -> Vec<(Level, String)> {
        self.0.lock().unwrap().clone()
    }
}

/// Visitor that pulls the `message` field out of an event.
struct MsgVisitor(String);
impl Visit for MsgVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

struct CaptureLayer(Captured);
impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut v = MsgVisitor(String::new());
        event.record(&mut v);
        self.0
             .0
            .lock()
            .unwrap()
            .push((*event.metadata().level(), v.0));
    }
}

/// Run `f` with log capture active and return its result plus the captured log.
pub(crate) fn capture<R>(f: impl FnOnce() -> R) -> (R, Captured) {
    use tracing_subscriber::prelude::*;
    // ROOT CAUSE of the intermittent updater::tests::* flake: in a test binary no
    // global default subscriber is ever installed (main.rs installs one, but that
    // is not run under `cargo test`). tracing caches each callsite's `Interest`
    // the FIRST time it is hit; with no subscriber present that interest is cached
    // as `never` — permanently, process-wide, on every thread. So if any
    // non-capturing test happens to hit a `tracing::warn!`/`error!` callsite
    // before a capturing test does, that callsite is disabled forever and the
    // capture silently misses its expected line. Which test loses the race varies
    // run-to-run (hence "a different updater test fails each time; all pass in
    // isolation").
    //
    // Fix: install a permanent, SILENT, TRACE-level global default once. It emits
    // nothing (a bare registry + LevelFilter, no fmt layer), but it keeps every
    // callsite's interest enabled so the per-capture thread-local collector below
    // always receives events. The serial lock then keeps concurrent captures from
    // interleaving on the shared interest cache; poison is recovered so a panic
    // under capture doesn't cascade.
    static GLOBAL_INIT: std::sync::Once = std::sync::Once::new();
    GLOBAL_INIT.call_once(|| {
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(tracing_subscriber::filter::LevelFilter::TRACE),
        );
    });
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cap = Captured::default();
    let subscriber = tracing_subscriber::registry().with(CaptureLayer(cap.clone()));
    let r = tracing::subscriber::with_default(subscriber, f);
    (r, cap)
}

#[cfg(test)]
mod self_tests {
    use super::*;

    #[test]
    fn captures_level_and_message_substring() {
        let (_, logs) = capture(|| {
            tracing::warn!("save failed: disk full");
            tracing::info!("session restored 3 tabs");
        });
        assert!(
            logs.has(Level::WARN, "save failed"),
            "events: {:?}",
            logs.events()
        );
        assert!(logs.has(Level::INFO, "session restored"));
        // Wrong-level / absent assertions are correctly false.
        assert!(!logs.has(Level::ERROR, "save failed"));
        assert!(!logs.any("never logged this"));
    }
}
