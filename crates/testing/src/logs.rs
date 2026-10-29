//! A `tracing` subscriber that keeps what production logs would show.

use std::{
    fmt::Debug,
    sync::{Arc, Mutex},
};

use tracing::{
    Event, Level, Metadata, Subscriber,
    field::{Field, Visit},
    span,
};

/// Every event at the info level or above, one line each with its level and fields, such as
/// `INFO message=worker started worker="in-memory-cache-purge-worker" store=app`. Install it on
/// the test's thread with `tracing::subscriber::set_default`.
#[derive(Clone, Default)]
pub struct Logs(Arc<Mutex<Vec<String>>>);

impl Logs {
    /// The lines logged so far.
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl Subscriber for Logs {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= Level::INFO
    }

    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        struct Line(String);

        impl Visit for Line {
            fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
                self.0.push_str(&format!(" {field}={value:?}"));
            }
        }

        let mut line = Line(event.metadata().level().to_string());

        event.record(&mut line);
        self.0.lock().unwrap().push(line.0);
    }

    fn enter(&self, _: &span::Id) {}

    fn exit(&self, _: &span::Id) {}
}
