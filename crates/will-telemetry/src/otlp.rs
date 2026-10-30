//! Codificação **OTLP/HTTP em Protobuf** com os tipos oficiais do
//! `opentelemetry-proto` (`ExportLogsServiceRequest`, `ExportTraceServiceRequest`).
//! É o formato padrão do OpenTelemetry: qualquer backend OTLP (Axiom, Grafana
//! Cloud, New Relic, OpenObserve, SigNoz, Collector…) aceita sem adaptação.
//! Objetos aninhados viram atributos com ponto (`events_by_level.warn`).

use crate::{InstanceInfo, Record, SpanRecord};
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value as AV;
use opentelemetry_proto::tonic::common::v1::{
    AnyValue, ArrayValue, InstrumentationScope, KeyValue,
};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs, SeverityNumber};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::span::SpanKind;
use opentelemetry_proto::tonic::trace::v1::status::StatusCode;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};
use prost::Message;
use serde_json::{Map, Value};

pub const CONTENT_TYPE: &str = "application/x-protobuf";

fn any_value(v: &Value) -> AnyValue {
    let value = match v {
        Value::Bool(b) => Some(AV::BoolValue(*b)),
        Value::Number(n) if n.is_i64() => n.as_i64().map(AV::IntValue),
        Value::Number(n) if n.is_u64() => n.as_u64().map(|u| {
            i64::try_from(u)
                .map(AV::IntValue)
                .unwrap_or(AV::StringValue(u.to_string()))
        }),
        Value::Number(n) => n.as_f64().map(AV::DoubleValue),
        Value::String(s) => Some(AV::StringValue(s.clone())),
        Value::Array(a) => Some(AV::ArrayValue(ArrayValue {
            values: a.iter().map(any_value).collect(),
        })),
        Value::Object(_) | Value::Null => Some(AV::StringValue(v.to_string())),
    };
    AnyValue { value }
}

fn push_attrs(out: &mut Vec<KeyValue>, prefix: &str, v: &Value) {
    match v {
        Value::Object(m) => {
            for (k, v) in m {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                push_attrs(out, &key, v);
            }
        }
        Value::Null => {}
        _ => out.push(KeyValue {
            key: prefix.to_string(),
            value: Some(any_value(v)),
            ..Default::default()
        }),
    }
}

pub(crate) fn attributes(m: &Map<String, Value>) -> Vec<KeyValue> {
    let mut out = Vec::with_capacity(m.len());
    for (k, v) in m {
        push_attrs(&mut out, k, v);
    }
    out
}

/// `Resource` com as convenções semânticas (`service.*`, `host.*`, `os.*`).
pub(crate) fn resource(inst: &InstanceInfo) -> Resource {
    let mut m = Map::new();
    m.insert(
        "service.name".into(),
        format!("will-ia-{}", inst.process).into(),
    );
    m.insert("service.namespace".into(), "will-ia".into());
    m.insert("service.version".into(), inst.version.into());
    m.insert(
        "service.instance.id".into(),
        inst.instance_id.clone().into(),
    );
    m.insert("host.name".into(), inst.host.clone().into());
    m.insert("os.type".into(), inst.os.into());
    m.insert("host.arch".into(), inst.arch.into());
    m.insert("process".into(), inst.process.into());
    let label = inst.label.read();
    if !label.is_empty() {
        m.insert("label".into(), label.clone().into());
    }
    if let Some(p) = inst.peer.read().as_ref() {
        m.insert("server.instance.id".into(), p.clone().into());
    }
    for (k, v) in inst.extra.read().iter() {
        m.insert(k.clone(), v.clone().into());
    }
    Resource {
        attributes: attributes(&m),
        ..Default::default()
    }
}

fn scope(inst: &InstanceInfo) -> InstrumentationScope {
    InstrumentationScope {
        name: "will-ia".into(),
        version: inst.version.into(),
        ..Default::default()
    }
}

fn severity(level: &str) -> (SeverityNumber, &'static str) {
    match level {
        "error" | "fatal" => (SeverityNumber::Error, "ERROR"),
        "warn" => (SeverityNumber::Warn, "WARN"),
        "info" => (SeverityNumber::Info, "INFO"),
        "debug" => (SeverityNumber::Debug, "DEBUG"),
        _ => (SeverityNumber::Trace, "TRACE"),
    }
}

fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .filter_map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

fn take_str(r: &mut Record, k: &str) -> Option<String> {
    r.remove(k).and_then(|v| v.as_str().map(|s| s.to_string()))
}

pub(crate) fn logs_request(inst: &InstanceInfo, recs: &[Record]) -> ExportLogsServiceRequest {
    let log_records = recs
        .iter()
        .map(|r| {
            let mut r = r.clone();
            let time = r
                .remove("time_ns")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(crate::layer::now_ns);
            let level = take_str(&mut r, "level").unwrap_or_else(|| "info".into());
            let (num, text) = severity(&level);
            let body = take_str(&mut r, "message").unwrap_or_default();
            let trace_id = take_str(&mut r, "trace_id")
                .map(|h| hex_bytes(&h))
                .unwrap_or_default();
            let span_id = take_str(&mut r, "span_id")
                .map(|h| hex_bytes(&h))
                .unwrap_or_default();
            LogRecord {
                time_unix_nano: time,
                observed_time_unix_nano: time,
                severity_number: num as i32,
                severity_text: text.into(),
                body: Some(AnyValue {
                    value: Some(AV::StringValue(body)),
                }),
                attributes: attributes(&r),
                trace_id,
                span_id,
                ..Default::default()
            }
        })
        .collect();
    ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource: Some(resource(inst)),
            scope_logs: vec![ScopeLogs {
                scope: Some(scope(inst)),
                log_records,
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    }
}

pub(crate) fn traces_request(
    inst: &InstanceInfo,
    spans: &[SpanRecord],
) -> ExportTraceServiceRequest {
    let spans = spans
        .iter()
        .map(|s| {
            let mut attrs = s.fields.clone();
            attrs.insert("module".into(), s.module.clone().into());
            attrs.insert("target".into(), s.target.clone().into());
            Span {
                trace_id: hex_bytes(&s.trace_id),
                span_id: hex_bytes(&s.span_id),
                parent_span_id: s
                    .parent_span_id
                    .as_deref()
                    .map(hex_bytes)
                    .unwrap_or_default(),
                name: s.name.clone(),
                kind: SpanKind::Internal as i32,
                start_time_unix_nano: s.start_ns,
                end_time_unix_nano: s.end_ns,
                attributes: attributes(&attrs),
                status: Some(Status {
                    code: if s.error {
                        StatusCode::Error as i32
                    } else {
                        StatusCode::Ok as i32
                    },
                    message: String::new(),
                }),
                ..Default::default()
            }
        })
        .collect();
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(resource(inst)),
            scope_spans: vec![ScopeSpans {
                scope: Some(scope(inst)),
                spans,
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    }
}

pub(crate) fn encode_logs(inst: &InstanceInfo, recs: &[Record]) -> Vec<u8> {
    logs_request(inst, recs).encode_to_vec()
}

pub(crate) fn encode_traces(inst: &InstanceInfo, spans: &[SpanRecord]) -> Vec<u8> {
    traces_request(inst, spans).encode_to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_nested_attributes_and_maps_ids() {
        let inst = InstanceInfo::new("id-1".into(), "server", "9.9.9");
        let mut r = Record::new();
        r.insert("time_ns".into(), 42u64.into());
        r.insert("level".into(), "warn".into());
        r.insert("message".into(), "oi".into());
        r.insert("trace_id".into(), "0a0b".into());
        r.insert("thread_id".into(), "t1".into());
        r.insert("m".into(), json!({ "a": { "b": 2 }, "c": true }));
        let req = logs_request(&inst, &[r]);
        let rec = &req.resource_logs[0].scope_logs[0].log_records[0];
        assert_eq!(rec.severity_number, SeverityNumber::Warn as i32);
        assert_eq!(rec.time_unix_nano, 42);
        assert_eq!(rec.trace_id, vec![0x0a, 0x0b]);
        assert_eq!(
            rec.body.as_ref().and_then(|b| b.value.clone()),
            Some(AV::StringValue("oi".into()))
        );
        let keys: Vec<&str> = rec.attributes.iter().map(|k| k.key.as_str()).collect();
        assert!(keys.contains(&"m.a.b"));
        assert!(keys.contains(&"m.c"));
        assert!(keys.contains(&"thread_id"));
        let res = req.resource_logs[0].resource.as_ref().expect("resource");
        assert!(res
            .attributes
            .iter()
            .any(|k| k.key == "service.instance.id"));
        // Round-trip pelo protobuf.
        let bytes = req.encode_to_vec();
        let back = ExportLogsServiceRequest::decode(bytes.as_slice()).expect("decode");
        assert_eq!(back.resource_logs[0].scope_logs[0].log_records.len(), 1);
    }

    #[test]
    fn span_status_and_parent() {
        let inst = InstanceInfo::new("id-1".into(), "server", "9.9.9");
        let s = SpanRecord {
            trace_id: "ff".repeat(16),
            span_id: "01".repeat(8),
            parent_span_id: Some("02".repeat(8)),
            name: "turn".into(),
            target: "will_server::x".into(),
            module: "server".into(),
            start_ns: 1,
            end_ns: 2,
            fields: Map::new(),
            error: true,
        };
        let req = traces_request(&inst, &[s]);
        let sp = &req.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(sp.parent_span_id.len(), 8);
        assert_eq!(
            sp.status.as_ref().map(|s| s.code),
            Some(StatusCode::Error as i32)
        );
    }
}
