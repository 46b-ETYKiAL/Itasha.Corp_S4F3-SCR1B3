//! Minimal, dependency-light `tracing` capture harness for the silent-failure
//! logging tests.
//!
//! scribe-core has no shared capture helper, so this installs a custom
//! [`tracing_subscriber::Layer`] for the duration of a closure that records
//! every event's level + a flattened `message + fields` string into a shared
//! buffer. Tests then assert that a given path emitted the expected level +
//! substring, and — critically — that no path/secret token leaks at `warn`+.
//!
//! Field flattening matters for the leak check: a secret could ride in a FIELD
//! (e.g. `error_kind = ...`) rather than the message, so the visitor records
//! every field's value, not just `message`.

use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

/// A captured event: its level and a flattened `message [field=value …]` string.
type Record = (Level, String);

/// Shared, thread-safe handle to the captured records.
#[derive(Clone, Default)]
pub(crate) struct CapturedLogs(Arc<Mutex<Vec<Record>>>);

impl CapturedLogs {
    /// All captured records (level + flattened text), in emission order.
    pub(crate) fn records(&self) -> Vec<Record> {
        self.0.lock().expect("captured-logs mutex").clone()
    }

    /// `true` when ANY record at `level` contains `needle`.
    pub(crate) fn has(&self, level: Level, needle: &str) -> bool {
        self.records()
            .iter()
            .any(|(lvl, text)| *lvl == level && text.contains(needle))
    }

    /// The concatenated text of every record at `warn` or MORE severe (i.e.
    /// `warn` + `error`; in `tracing`, `ERROR < WARN`). Used to assert that a
    /// secret/path token never appears at user-visible severity.
    pub(crate) fn warn_plus_text(&self) -> String {
        self.records()
            .iter()
            .filter(|(lvl, _)| *lvl <= Level::WARN)
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Visitor that flattens an event's `message` + all fields into one string.
struct FlattenVisitor {
    buf: String,
}

impl FlattenVisitor {
    fn push(&mut self, piece: &str) {
        if !self.buf.is_empty() {
            self.buf.push(' ');
        }
        self.buf.push_str(piece);
    }
}

impl Visit for FlattenVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            let piece = value.to_string();
            self.push(&piece);
        } else {
            let piece = format!("{}={value}", field.name());
            self.push(&piece);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let piece = format!("{value:?}");
            self.push(&piece);
        } else {
            let piece = format!("{}={value:?}", field.name());
            self.push(&piece);
        }
    }
}

/// The capturing layer.
struct CaptureLayer {
    logs: CapturedLogs,
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = FlattenVisitor { buf: String::new() };
        event.record(&mut visitor);
        let level = *event.metadata().level();
        self.logs
            .0
            .lock()
            .expect("captured-logs mutex")
            .push((level, visitor.buf));
    }
}

/// Run `f` with a capturing subscriber installed for the current thread, handing
/// it the [`CapturedLogs`] handle to assert against.
pub(crate) fn with_captured_logs<R>(f: impl FnOnce(&CapturedLogs) -> R) -> R {
    let logs = CapturedLogs::default();
    let layer = CaptureLayer { logs: logs.clone() };
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, || f(&logs))
}
