//! Parser tolerante do stream-json do `claude -p`.

use serde_json::Value;
use will_core::{EngineDelta, TurnResult};

#[derive(Debug, Clone)]
pub enum StreamLine {
    Init {
        session_id: String,
        model: Option<String>,
        mcp_servers: Vec<(String, String)>,
        permission_mode: Option<String>,
    },
    /// Bloco(s) do assistant já completos.
    Assistant {
        narration: Vec<String>,
        tool_uses: Vec<(String, String)>,
        thinking: Vec<String>,
    },
    /// Eco de mensagem do usuário (replay) — `text` é o conteúdo textual.
    UserReplay {
        text: String,
    },
    ToolResult {
        preview: String,
        is_error: bool,
        permission_denied: Option<String>,
    },
    TextDelta {
        text: String,
    },
    Result {
        result: TurnResult,
        stale_session: bool,
    },
    Status {
        text: String,
    },
    RateLimit {
        detail: String,
    },
    Other,
}

fn text_of_content(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                    b.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub fn parse_line(line: &str) -> StreamLine {
    let s = line.trim();
    if s.is_empty() {
        return StreamLine::Other;
    }
    let v: Value = match serde_json::from_str(s) {
        Ok(v) => v,
        Err(_) => return StreamLine::Other,
    };
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match ty {
        "system" => {
            let subtype = v.get("subtype").and_then(|t| t.as_str()).unwrap_or("");
            match subtype {
                "init" => StreamLine::Init {
                    session_id: v
                        .get("session_id")
                        .and_then(|s| s.as_str())
                        .unwrap_or("")
                        .to_string(),
                    model: v
                        .get("model")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string()),
                    mcp_servers: v
                        .get("mcp_servers")
                        .and_then(|m| m.as_array())
                        .map(|arr| {
                            arr.iter()
                                .map(|e| {
                                    (
                                        e.get("name")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                        e.get("status")
                                            .and_then(|n| n.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                    permission_mode: v
                        .get("permissionMode")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string()),
                },
                "status" => StreamLine::Status {
                    text: v
                        .get("status")
                        .and_then(|s| s.as_str())
                        .or_else(|| v.get("message").and_then(|s| s.as_str()))
                        .unwrap_or("")
                        .to_string(),
                },
                _ => StreamLine::Other,
            }
        }
        "assistant" => {
            let blocks = v
                .get("message")
                .and_then(|m| m.get("content"))
                .or_else(|| v.get("content"))
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();
            let mut narration = vec![];
            let mut tool_uses = vec![];
            let mut thinking = vec![];
            for b in blocks {
                match b.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                            if !t.trim().is_empty() {
                                narration.push(t.to_string());
                            }
                        }
                    }
                    Some("tool_use") => {
                        let name = b
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("tool")
                            .to_string();
                        let input = b.get("input").map(|i| i.to_string()).unwrap_or_default();
                        tool_uses.push((name, will_core::tools::truncate(&input, 200)));
                    }
                    Some("thinking") => {
                        if let Some(t) = b.get("thinking").and_then(|t| t.as_str()) {
                            if !t.trim().is_empty() {
                                thinking.push(will_core::tools::truncate(t, 400));
                            }
                        }
                    }
                    _ => {}
                }
            }
            StreamLine::Assistant {
                narration,
                tool_uses,
                thinking,
            }
        }
        "user" => {
            let content = v
                .get("message")
                .and_then(|m| m.get("content"))
                .cloned()
                .unwrap_or(Value::Null);
            // tool_result?
            let is_tool_result = content
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
                })
                .unwrap_or(false)
                || v.get("tool_use_result").is_some();
            if is_tool_result {
                let tur = v.get("tool_use_result");
                let mut preview = String::new();
                let mut is_error = false;
                if let Some(t) = tur {
                    if let Some(out) = t.get("stdout").and_then(|s| s.as_str()) {
                        preview = out.to_string();
                    }
                    if let Some(err) = t.get("stderr").and_then(|s| s.as_str()) {
                        if !err.is_empty() {
                            preview = if preview.is_empty() {
                                err.to_string()
                            } else {
                                format!("{preview}\n{err}")
                            };
                        }
                    }
                    if preview.is_empty() {
                        preview = will_core::tools::truncate(&t.to_string(), 300);
                    }
                }
                if let Some(arr) = content.as_array() {
                    for b in arr {
                        if b.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                            if b.get("is_error").and_then(|e| e.as_bool()) == Some(true) {
                                is_error = true;
                            }
                            if preview.is_empty() {
                                preview = text_of_content(b.get("content").unwrap_or(&Value::Null));
                            }
                        }
                    }
                }
                let lower = preview.to_lowercase();
                let permission_denied = if lower.contains("permission")
                    && (lower.contains("denied")
                        || lower.contains("not allowed")
                        || lower.contains("requires approval"))
                {
                    Some(will_core::tools::truncate(&preview, 120))
                } else {
                    None
                };
                StreamLine::ToolResult {
                    preview: will_core::tools::truncate(&preview, 300),
                    is_error,
                    permission_denied,
                }
            } else {
                StreamLine::UserReplay {
                    text: text_of_content(&content),
                }
            }
        }
        "stream_event" => {
            let ev = v.get("event").cloned().unwrap_or(Value::Null);
            if ev.get("type").and_then(|t| t.as_str()) == Some("content_block_delta") {
                if let Some(d) = ev.get("delta") {
                    if d.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
                        if let Some(t) = d.get("text").and_then(|t| t.as_str()) {
                            return StreamLine::TextDelta {
                                text: t.to_string(),
                            };
                        }
                    }
                }
            }
            StreamLine::Other
        }
        "result" => {
            let errors: Vec<String> = v
                .get("errors")
                .and_then(|e| e.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let stale = errors
                .iter()
                .any(|e| e.contains("No conversation found with session ID"));
            let subtype = v.get("subtype").and_then(|s| s.as_str()).unwrap_or("");
            let is_error = v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false)
                || subtype.contains("error");
            let mut text = v
                .get("result")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string();
            if text.is_empty() && is_error && !errors.is_empty() {
                text = errors.join(" | ");
            }
            let model = v
                .get("modelUsage")
                .and_then(|m| m.as_object())
                .and_then(|o| o.keys().next().cloned());
            StreamLine::Result {
                result: TurnResult {
                    text,
                    is_error,
                    cost_usd: v.get("total_cost_usd").and_then(|c| c.as_f64()),
                    duration_ms: v.get("duration_ms").and_then(|d| d.as_u64()),
                    model,
                    session_id: v
                        .get("session_id")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_string()),
                    usage: v.get("usage").cloned(),
                },
                stale_session: stale,
            }
        }
        "rate_limit_event" => rate_limit_line(&v),
        _ => StreamLine::Other,
    }
}

/// `rate_limit_event` é informativo: o CLI (2.1.x) emite um a cada turno com
/// `rate_limit_info.status = "allowed"` e a utilização da janela. Só é um
/// problema quando o status não é `allowed`/`allowed_warning` (ex.: `rejected`).
fn rate_limit_line(v: &Value) -> StreamLine {
    let info = v.get("rate_limit_info").unwrap_or(v);
    let status = info.get("status").and_then(|s| s.as_str()).unwrap_or("");
    if status.is_empty() || status == "allowed" || status == "allowed_warning" {
        return StreamLine::Other;
    }
    let window = info
        .get("rateLimitType")
        .and_then(|s| s.as_str())
        .unwrap_or("?");
    let util = info
        .get("unifiedWindows")
        .and_then(|w| w.get(window))
        .and_then(|w| w.get("utilization"))
        .and_then(|u| u.as_f64())
        .map(|u| format!(", uso {:.0}%", u * 100.0))
        .unwrap_or_default();
    let resets = info
        .get("resetsAt")
        .and_then(|r| r.as_i64())
        .and_then(|r| chrono::DateTime::from_timestamp(r, 0))
        .map(|d| {
            format!(
                ", libera em {}",
                d.with_timezone(&chrono::Local).format("%d/%m %H:%M")
            )
        })
        .unwrap_or_default();
    StreamLine::RateLimit {
        detail: format!("janela {window}: {status}{util}{resets}"),
    }
}

pub fn deltas_from_assistant(
    narration: &[String],
    tool_uses: &[(String, String)],
    thinking: &[String],
) -> Vec<EngineDelta> {
    let mut out = vec![];
    for t in thinking {
        out.push(EngineDelta::Thinking { text: t.clone() });
    }
    for (name, input) in tool_uses {
        out.push(EngineDelta::ToolUse {
            name: name.clone(),
            input_preview: input.clone(),
        });
    }
    for n in narration {
        out.push(EngineDelta::Narration { text: n.clone() });
    }
    out
}

/// Linha JSON para enviar uma mensagem do usuário.
pub fn user_message_line(text: &str) -> String {
    let v = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] }
    });
    let mut s = v.to_string();
    s.push('\n');
    s
}

/// Pedido de interrupção do turno atual (protocolo de controle do SDK).
pub fn interrupt_line() -> String {
    let v = serde_json::json!({
        "type": "control_request",
        "request_id": uuid::Uuid::new_v4().to_string(),
        "request": { "subtype": "interrupt" }
    });
    let mut s = v.to_string();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_init() {
        let l = r#"{"type":"system","subtype":"init","session_id":"abc","model":"m","mcp_servers":[{"name":"will_ia","status":"connected"}]}"#;
        match parse_line(l) {
            StreamLine::Init {
                session_id,
                mcp_servers,
                ..
            } => {
                assert_eq!(session_id, "abc");
                assert_eq!(mcp_servers[0].0, "will_ia");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parses_result_and_stale() {
        let l = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"errors":["No conversation found with session ID: x"],"session_id":"x"}"#;
        match parse_line(l) {
            StreamLine::Result { stale_session, .. } => assert!(stale_session),
            _ => panic!(),
        }
        let l = r#"{"type":"result","subtype":"success","result":"ok","total_cost_usd":0.01,"duration_ms":10,"session_id":"x","modelUsage":{"claude-x":{}}}"#;
        match parse_line(l) {
            StreamLine::Result {
                result,
                stale_session,
            } => {
                assert!(!stale_session);
                assert_eq!(result.text, "ok");
                assert_eq!(result.model.as_deref(), Some("claude-x"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn parses_user_replay_vs_tool_result() {
        let l =
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"oi"}]}}"#;
        assert!(matches!(parse_line(l), StreamLine::UserReplay { text } if text == "oi"));
        let l = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"x"}]},"tool_use_result":{"stdout":"x","stderr":""}}"#;
        assert!(matches!(parse_line(l), StreamLine::ToolResult { .. }));
    }

    #[test]
    fn rate_limit_event_allowed_is_informational() {
        let l = r#"{"type":"rate_limit_event","rate_limit_info":{"isUsingOverage":false,"overageStatus":"rejected","rateLimitType":"five_hour","resetsAt":1793204400,"status":"allowed","unifiedWindows":{"five_hour":{"resetsAt":1793204400,"status":"allowed","utilization":0.18}}}}"#;
        assert!(matches!(parse_line(l), StreamLine::Other));
        let l = r#"{"type":"rate_limit_event","rate_limit_info":{"rateLimitType":"five_hour","resetsAt":1793204400,"status":"rejected","unifiedWindows":{"five_hour":{"status":"rejected","utilization":1.0}}}}"#;
        match parse_line(l) {
            StreamLine::RateLimit { detail } => {
                assert!(
                    detail.starts_with("janela five_hour: rejected, uso 100%"),
                    "{detail}"
                );
                assert!(detail.contains("libera em"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn tolerates_garbage() {
        assert!(matches!(parse_line("not json"), StreamLine::Other));
        assert!(matches!(parse_line(""), StreamLine::Other));
    }
}
