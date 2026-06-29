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
