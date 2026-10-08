//! JSON log lines with flat, OpenTelemetry-named fields.
//!
//! tracing-subscriber's own JSON nests event fields under `fields` and span
//! fields under `span`/`spans`, so a request's method, path and key id sit
//! apart from its status. [`FlatJson`] writes one flat object per event:
//! `timestamp`, `level`, `target`, `message`, then the fields of every
//! enclosing span (outermost first) and the event's own, with their names
//! as written, e.g. `http.request.method` and `http.response.status_code`.
//! An inner field wins over an outer one of the same name.
//!
//! Install it with [`JsonFields`] as the field formatter, which stores each
//! span's fields as a JSON object for the event formatter to merge:
//!
//! ```no_run
//! use tracing_subscriber::fmt::format::JsonFields;
//! tracing_subscriber::fmt()
//!     .fmt_fields(JsonFields::new())
//!     .event_format(agentstategraph_mcp::logging::FlatJson)
//!     .init();
//! ```
//!
//! [`JsonFields`]: tracing_subscriber::fmt::format::JsonFields

use std::fmt;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields};
use tracing_subscriber::registry::LookupSpan;

/// One flat JSON object per event. See the module docs.
#[derive(Debug, Clone, Copy, Default)]
pub struct FlatJson;

impl<S, N> FormatEvent<S, N> for FlatJson
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();
        let mut out = Map::new();
        out.insert(
            "timestamp".into(),
            chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
                .into(),
        );
        out.insert("level".into(), meta.level().as_str().into());
        out.insert("target".into(), meta.target().into());
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                let extensions = span.extensions();
                // Spans formatted by JsonFields hold a JSON object; with any
                // other field formatter there is nothing to merge.
                if let Some(fields) = extensions.get::<FormattedFields<N>>()
                    && let Ok(Value::Object(fields)) = serde_json::from_str(fields.as_str())
                {
                    out.extend(fields);
                }
            }
        }
        event.record(&mut Fields(&mut out));
        writeln!(writer, "{}", Value::Object(out))
    }
}

/// Collects an event's fields into the output object.
struct Fields<'a>(&'a mut Map<String, Value>);

impl Visit for Fields<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0
            .insert(field.name().into(), format!("{value:?}").into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::format::JsonFields;

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Captured;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[test]
    fn span_and_event_fields_land_flat_in_one_object() {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .fmt_fields(JsonFields::new())
            .event_format(FlatJson)
            .with_writer(captured.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let outer = tracing::info_span!("outer", shared = "outer", only.outer = 1u64);
            let _outer = outer.enter();
            let span = tracing::info_span!(
                "http",
                http.request.method = "GET",
                shared = "inner",
                asg.api_key.id = tracing::field::Empty,
            );
            let _span = span.enter();
            // Recorded after the span opened, as the auth middleware does.
            span.record("asg.api_key.id", "key-1");
            tracing::info!(
                http.response.status_code = 200u64,
                http.server.request.duration = 0.25f64,
                "request"
            );
        });

        let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        let line: Value = serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("{text}"));
        assert_eq!(line["message"], "request");
        assert_eq!(line["level"], "INFO");
        assert_eq!(line["http.request.method"], "GET");
        assert_eq!(line["asg.api_key.id"], "key-1");
        assert_eq!(line["http.response.status_code"], 200);
        assert_eq!(line["http.server.request.duration"], 0.25);
        assert_eq!(line["only.outer"], 1);
        assert_eq!(line["shared"], "inner", "the inner span wins");
        assert!(line.get("fields").is_none() && line.get("span").is_none());
    }
}
