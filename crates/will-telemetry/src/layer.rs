//! `tracing_subscriber::Layer` que transforma eventos em registros de log e
//! spans fechados em spans OTLP, herdando os campos dos spans abertos
//! (`thread_id`, `turn_id`, `tool`…). Cada span raiz ganha um `trace_id` e
//! cada span um `span_id` (hex, como o OTLP pede), então logs e spans se
//! correlacionam em qualquer backend OpenTelemetry.

use crate::{Item, Metrics, Record, SpanRecord};
use serde_json::Value;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Estado de um span, guardado nas extensões do registro.
#[derive(Clone)]
pub struct SpanState {
    pub fields: serde_json::Map<String, Value>,
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub start_ns: u64,
    pub started: std::time::Instant,
}

struct JsonVisitor<'a>(&'a mut serde_json::Map<String, Value>);

impl Visit for JsonVisitor<'_> {
    fn record_f64(&mut self, f: &Field, v: f64) {
        self.0.insert(f.name().into(), v.into());
    }
    fn record_i64(&mut self, f: &Field, v: i64) {
        self.0.insert(f.name().into(), v.into());
    }
    fn record_u64(&mut self, f: &Field, v: u64) {
        self.0.insert(f.name().into(), v.into());
    }
    fn record_i128(&mut self, f: &Field, v: i128) {
        self.0.insert(f.name().into(), v.to_string().into());
    }
    fn record_u128(&mut self, f: &Field, v: u128) {
        self.0.insert(f.name().into(), v.to_string().into());
    }
    fn record_bool(&mut self, f: &Field, v: bool) {
        self.0.insert(f.name().into(), v.into());
    }
    fn record_str(&mut self, f: &Field, v: &str) {
        self.0.insert(f.name().into(), crate::redact(v).into());
    }
    fn record_error(&mut self, f: &Field, v: &(dyn std::error::Error + 'static)) {
        self.0
            .insert(f.name().into(), crate::redact(&v.to_string()).into());
    }
    fn record_debug(&mut self, f: &Field, v: &dyn std::fmt::Debug) {
        let s = format!("{v:?}");
        // Strings via `%`/`?` chegam como Debug com aspas.
        let s = s
            .strip_prefix('"')
            .and_then(|t| t.strip_suffix('"'))
            .map(|t| t.to_string())
            .unwrap_or(s);
        self.0.insert(f.name().into(), crate::redact(&s).into());
    }
}

pub struct TelemetryLayer {
    tx: mpsc::Sender<Item>,
    metrics: Arc<Metrics>,
    /// Nível máximo enviado (código de `level_code`).
    level: Arc<AtomicU8>,
}

impl TelemetryLayer {
    pub(crate) fn new(tx: mpsc::Sender<Item>, metrics: Arc<Metrics>, level: Arc<AtomicU8>) -> Self {
        Self { tx, metrics, level }
    }
}

pub(crate) fn level_code(l: tracing::Level) -> u8 {
    match l {
        tracing::Level::ERROR => 0,
        tracing::Level::WARN => 1,
        tracing::Level::INFO => 2,
        tracing::Level::DEBUG => 3,
        tracing::Level::TRACE => 4,
    }
}

pub(crate) fn module_of(target: &str) -> String {
    target
        .split("::")
        .next()
        .unwrap_or(target)
        .trim_start_matches("will_")
        .to_string()
}

pub(crate) fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn hex_id(bytes: usize) -> String {
    let u = uuid::Uuid::new_v4();
    let b = u.as_bytes();
    b[..bytes].iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn base_record(level: tracing::Level, target: &str, message: &str) -> Record {
    let mut rec = Record::new();
    rec.insert("time_ns".into(), now_ns().into());
    rec.insert("level".into(), level.as_str().to_lowercase().into());
    rec.insert("target".into(), target.into());
    rec.insert("module".into(), module_of(target).into());
    rec.insert("message".into(), crate::redact(message).into());
    rec
}

impl<S> Layer<S> for TelemetryLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let parent = span.parent().and_then(|p| {
            p.extensions()
                .get::<SpanState>()
                .map(|s| (s.trace_id.clone(), s.span_id.clone()))
        });
        let (trace_id, parent_span_id) = match parent {
            Some((t, p)) => (t, Some(p)),
            None => (hex_id(16), None),
        };
        let mut st = SpanState {
            fields: serde_json::Map::new(),
            trace_id,
            span_id: hex_id(8),
            parent_span_id,
            start_ns: now_ns(),
            started: std::time::Instant::now(),
        };
        attrs.record(&mut JsonVisitor(&mut st.fields));
        span.extensions_mut().insert(st);
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut ext = span.extensions_mut();
        if let Some(st) = ext.get_mut::<SpanState>() {
            values.record(&mut JsonVisitor(&mut st.fields));
        }
    }

    fn on_close(&self, id: tracing::span::Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };
        let target = span.metadata().target();
        if !target.starts_with("will_") {
            return;
        }
        let module = module_of(target);
        self.metrics.record_span(span.name(), &module);
        let Some(st) = span.extensions().get::<SpanState>().cloned() else {
            return;
        };
        let error = st
            .fields
            .get("ok")
            .and_then(|v| v.as_bool())
            .map(|ok| !ok)
            .unwrap_or(false)
            || st.fields.contains_key("error");
        let rec = SpanRecord {
            trace_id: st.trace_id,
            span_id: st.span_id,
            parent_span_id: st.parent_span_id,
            name: span.name().to_string(),
            target: target.to_string(),
            module,
            start_ns: st.start_ns,
            end_ns: st.start_ns + st.started.elapsed().as_nanos() as u64,
            fields: st.fields,
            error,
        };
        if self.tx.try_send(Item::Span(rec)).is_err() {
            self.metrics.record_dropped();
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level = *meta.level();
        let target = meta.target();
        let ours = target.starts_with("will_");
        // Métricas: tudo que é nosso, ou qualquer aviso/erro de dependência.
        if !ours && level > tracing::Level::WARN {
            return;
        }
        let module = module_of(target);
        self.metrics.record_event(level, &module);
        if level_code(level) > self.level.load(Ordering::Relaxed) {
            return;
        }
        let mut fields = serde_json::Map::new();
        event.record(&mut JsonVisitor(&mut fields));
        let message = fields
            .remove("message")
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        let mut rec = base_record(level, target, &message);
        if let Some(file) = meta.file() {
            rec.insert(
                "src".into(),
                format!("{file}:{}", meta.line().unwrap_or(0)).into(),
            );
        }
        // Campos herdados dos spans (raiz → folha; a folha vence) + ids OTLP.
        if let Some(scope) = ctx.event_scope(event) {
            let mut names = Vec::new();
            let mut ids = None;
            for span in scope.from_root() {
                names.push(span.name());
                if let Some(st) = span.extensions().get::<SpanState>() {
                    for (k, v) in &st.fields {
                        rec.insert(k.clone(), v.clone());
                    }
                    ids = Some((st.trace_id.clone(), st.span_id.clone()));
                }
            }
            if let Some(leaf) = names.last() {
                rec.insert("span".into(), (*leaf).into());
            }
            if names.len() > 1 {
                rec.insert("spans".into(), names.join(">").into());
            }
            if let Some((t, s)) = ids {
                rec.insert("trace_id".into(), t.into());
                rec.insert("span_id".into(), s.into());
            }
        }
        for (k, v) in fields {
            rec.insert(k, v);
        }
        if self.tx.try_send(Item::Log(rec)).is_err() {
            self.metrics.record_dropped();
        }
    }
}
