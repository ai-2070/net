//! A `tracing::Layer` that records every event's fields as (name, value)
//! pairs, so a test can assert an emit site's *structured* fields by key
//! and value rather than by substring on a formatted line.
//!
//! Shared because three suites need it and the interesting property is
//! the visitor's **completeness**. `flow_log_hygiene` asserts a negative
//! — "no field carries the quote id" — and a visitor with a hole would
//! let a regression that logs it through an uncovered value type pass
//! silently. Three private copies drift; the one that drifts is the one
//! that stops catching things.

#![allow(dead_code)]

use std::sync::Arc;

/// Records every event's fields into a shared buffer.
pub struct FieldCapture {
    pub fields: Arc<parking_lot::Mutex<Vec<(String, String)>>>,
}

impl FieldCapture {
    /// The layer plus the buffer it fills.
    pub fn new() -> (Self, Arc<parking_lot::Mutex<Vec<(String, String)>>>) {
        let fields = Arc::new(parking_lot::Mutex::new(Vec::new()));
        (
            Self {
                fields: fields.clone(),
            },
            fields,
        )
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for FieldCapture {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut buf = self.fields.lock();
        event.record(&mut Collector(&mut buf));
    }
}

/// Every value type `tracing` can hand a visitor, not just str/debug.
///
/// `record_debug` is the catch-all `tracing` falls back to, but the typed
/// methods are called in preference to it wherever one applies — so each
/// has to be implemented, or a field logged as (say) a `u64` never
/// reaches the buffer and an assertion about "every field" quietly stops
/// covering it.
struct Collector<'a>(&'a mut Vec<(String, String)>);

impl Collector<'_> {
    fn put(&mut self, field: &tracing::field::Field, value: impl std::fmt::Display) {
        self.0.push((field.name().to_string(), value.to_string()));
    }
}

impl tracing::field::Visit for Collector<'_> {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.put(field, value);
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.put(field, value);
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.put(field, value);
    }
    fn record_i128(&mut self, field: &tracing::field::Field, value: i128) {
        self.put(field, value);
    }
    fn record_u128(&mut self, field: &tracing::field::Field, value: u128) {
        self.put(field, value);
    }
    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.put(field, value);
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.put(field, value);
    }
    fn record_bytes(&mut self, field: &tracing::field::Field, value: &[u8]) {
        self.0
            .push((field.name().to_string(), format!("{value:?}")));
    }
    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.put(field, value);
    }
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0
            .push((field.name().to_string(), format!("{value:?}")));
    }
}
