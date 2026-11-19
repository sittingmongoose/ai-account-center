//! A `tracing` subscriber that writes the service's events as log entries. Spans are not
//! recorded. The event's `message` field is the entry message, an `event` field names the event
//! (otherwise the target does), and every other field joins the context.
use crate::{Extra, Level, Log};
use serde_json::{Map, Value, json};
use std::{fmt::Debug, sync::Arc};
use tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    span,
};

struct Fields {
    message: String,
    event: Option<String>,
    context: Map<String, Value>,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_owned(),
            "event" => self.event = Some(value.to_owned()),
            name => {
                self.context.insert(name.to_owned(), json!(value));
            }
        }
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.context.insert(field.name().to_owned(), json!(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.context.insert(field.name().to_owned(), json!(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.context.insert(field.name().to_owned(), json!(value));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.context.insert(field.name().to_owned(), json!(value));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        self.record_str(field, &format!("{value:?}"));
    }
}

struct LogSubscriber {
    log: Arc<Log>,
    source: &'static str,
}

impl Subscriber for LogSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}
    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}
    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        let level = match *metadata.level() {
            tracing::Level::ERROR => Level::Error,
            tracing::Level::WARN => Level::Warn,
            tracing::Level::INFO => Level::Info,
            _ => Level::Debug,
        };
        let mut fields = Fields {
            message: String::new(),
            event: None,
            context: Map::new(),
        };
        event.record(&mut fields);
        let name = fields.event.unwrap_or_else(|| metadata.target().to_owned());
        self.log.write(
            self.source,
            level,
            &name,
            &fields.message,
            &fields.context,
            &Extra::default(),
        );
    }
    fn enter(&self, _span: &span::Id) {}
    fn exit(&self, _span: &span::Id) {}
}

/// Send this process's `tracing` events to `log` as `source`. The first call wins; later calls
/// (a second boot in one test process) keep the installed subscriber.
pub fn install(log: Arc<Log>, source: &'static str) -> bool {
    tracing::subscriber::set_global_default(LogSubscriber { log, source }).is_ok()
}
