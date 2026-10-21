//! Mensagens do protocolo OpenAI Realtime (beta "legacy" e GA).

use serde_json::{json, Value};
use will_core::settings::RealtimeSettings;

/// Variante do payload de `session.update`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionVariant {
    /// API beta: `modalities`, `input_audio_format`, `turn_detection` na raiz.
    Legacy,
    /// API GA: `session.type = realtime`, bloco `audio.{input,output}`.
    Ga,
}

impl SessionVariant {
    /// GA primeiro para os modelos `gpt-realtime*`; beta para os demais.
    pub fn preferred_for(model: &str) -> Self {
        if model.starts_with("gpt-realtime") {
            SessionVariant::Ga
        } else {
            SessionVariant::Legacy
        }
    }

    pub fn other(self) -> Self {
        match self {
            SessionVariant::Legacy => SessionVariant::Ga,
            SessionVariant::Ga => SessionVariant::Legacy,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SessionVariant::Legacy => "legacy",
            SessionVariant::Ga => "ga",
        }
    }
}

pub fn is_push_to_talk(settings: &RealtimeSettings) -> bool {
    settings.activation.eq_ignore_ascii_case("push_to_talk")
}

/// Ferramentas expostas ao modelo de voz.
pub fn tools_spec() -> Value {
    json!([
        {
            "type": "function",
            "name": "ask_brain",
            "description": "Delega ao cérebro (Claude Code) tarefas que exigem pesquisa, memória de longo prazo, leitura/escrita de arquivos, execução de scripts ou raciocínio longo. Chame novamente para acrescentar contexto a uma tarefa em andamento.",
            "parameters": {
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "Pedido completo, em texto, para o cérebro." },
                    "new_thread": { "type": "boolean", "description": "true para começar uma conversa nova com o cérebro (contexto limpo)." },
                    "thread": { "type": "string", "description": "Para continuar um assunto existente: id (ou prefixo) ou título de uma thread retornada por list_threads. Sem isto, usa a thread de voz atual." },
                    "title": { "type": "string", "description": "Título curto (2 a 5 palavras, em português) que identifica o assunto, derivado do pedido. Informe sempre que new_thread for true." }
                },
                "required": ["prompt"]
            }
        },
        {
            "type": "function",
            "name": "list_threads",
            "description": "Lista as conversas (threads) existentes com o cérebro: id, título/assunto, origem, última atividade e se há trabalho em andamento. Use para oferecer ao usuário continuar um assunto em vez de começar do zero.",
            "parameters": { "type": "object", "properties": {} }
        },
        {
            "type": "function",
            "name": "brain_status",
            "description": "Verifica se o cérebro ainda está trabalhando na última tarefa.",
            "parameters": { "type": "object", "properties": {} }
        }
    ])
}

/// Instruções da persona + orientação fixa sobre as ferramentas (threads).
pub fn instructions_with_tools(settings: &RealtimeSettings) -> String {
    format!(
        "{}\n\nFerramentas: `list_threads` lista os assuntos (threads) já abertos com o cérebro. Quando o usuário mencionar um assunto que pode já existir, ou pedir para continuar algo, chame `list_threads`, diga em uma frase quais assuntos existem (pelo título) e pergunte se quer continuar um deles ou começar um novo. Para continuar, chame `ask_brain` com `thread` igual ao título ou id escolhido; para começar do zero, `new_thread: true` junto com `title`, um título curto (2 a 5 palavras) que resuma o assunto, por exemplo \"Relatório de vendas\" ou \"Viagem a Recife\". Nunca use títulos genéricos como \"Voz\" ou \"Conversa\". Sem indicação, `ask_brain` segue na thread atual.",
        settings.instructions.trim()
    )
}

fn turn_detection(settings: &RealtimeSettings) -> Value {
    if is_push_to_talk(settings) {
        return Value::Null;
    }
    let kind = settings.turn_detection.trim();
    if kind.eq_ignore_ascii_case("semantic_vad") {
        json!({ "type": "semantic_vad" })
    } else {
        json!({
            "type": "server_vad",
            "silence_duration_ms": 600,
            "threshold": settings.vad_threshold.clamp(0.0, 1.0),
            "interrupt_response": settings.barge_in
        })
    }
}

/// `session.update` no formato beta.
pub fn legacy_session_update(settings: &RealtimeSettings) -> Value {
    json!({
        "type": "session.update",
        "session": {
            "modalities": ["audio", "text"],
            "voice": settings.voice,
            "instructions": instructions_with_tools(settings),
            "input_audio_format": "pcm16",
            "output_audio_format": "pcm16",
            "input_audio_transcription": transcription(settings),
            "turn_detection": turn_detection(settings),
            "tools": tools_spec(),
            "tool_choice": "auto"
        }
    })
}

/// Transcrição do áudio do usuário: opcional. Campo vazio → `null` (o servidor
/// rejeitaria o `session.update` inteiro com `{"model": ""}`, e a sessão ficaria
/// sem persona e sem ferramentas).
fn transcription(settings: &RealtimeSettings) -> Value {
    let m = settings.transcription_model.trim();
    if m.is_empty() {
        Value::Null
    } else {
        json!({ "model": m })
    }
}

/// `session.update` no formato GA.
pub fn ga_session_update(settings: &RealtimeSettings) -> Value {
    json!({
        "type": "session.update",
        "session": {
            "type": "realtime",
            "output_modalities": ["audio"],
            "instructions": instructions_with_tools(settings),
            "audio": {
                "input": {
                    "format": { "type": "audio/pcm", "rate": 24000 },
                    "turn_detection": turn_detection(settings),
                    "transcription": transcription(settings)
                },
                "output": {
                    "format": { "type": "audio/pcm", "rate": 24000 },
                    "voice": settings.voice
                }
            },
            "tools": tools_spec(),
            "tool_choice": "auto"
        }
    })
}

pub fn session_update(variant: SessionVariant, settings: &RealtimeSettings) -> Value {
    match variant {
        SessionVariant::Legacy => legacy_session_update(settings),
        SessionVariant::Ga => ga_session_update(settings),
    }
}

pub fn audio_append(base64: &str) -> Value {
    json!({ "type": "input_audio_buffer.append", "audio": base64 })
}

pub fn audio_commit() -> Value {
    json!({ "type": "input_audio_buffer.commit" })
}

pub fn audio_clear() -> Value {
    json!({ "type": "input_audio_buffer.clear" })
}

pub fn response_create() -> Value {
    json!({ "type": "response.create" })
}

pub fn response_cancel() -> Value {
    json!({ "type": "response.cancel" })
}

pub fn function_call_output(call_id: &str, output: &Value) -> Value {
    json!({
        "type": "conversation.item.create",
        "item": {
            "type": "function_call_output",
            "call_id": call_id,
            "output": output.to_string()
        }
    })
}

pub fn user_text_item(text: &str) -> Value {
    json!({
        "type": "conversation.item.create",
        "item": {
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": text }]
        }
    })
}

pub fn truncate_item(item_id: &str, audio_end_ms: u64) -> Value {
    json!({
        "type": "conversation.item.truncate",
        "item_id": item_id,
        "content_index": 0,
        "audio_end_ms": audio_end_ms
    })
}

/// O erro indica parâmetro desconhecido no `session.update` (variante errada)?
pub fn is_unknown_parameter_error(code: Option<&str>, message: &str, param: Option<&str>) -> bool {
    let m = message.to_ascii_lowercase();
    let code_hit = code.is_some_and(|c| {
        let c = c.to_ascii_lowercase();
        c == "unknown_parameter" || c == "invalid_request_error" && m.contains("unknown parameter")
    });
    code_hit
        || m.contains("unknown parameter")
        || m.contains("unknown_parameter")
        || m.contains("unrecognized request argument")
        || (m.contains("missing required parameter")
            && param.is_some_and(|p| p.starts_with("session.")))
        || param.is_some_and(|p| {
            p.starts_with("session.") && (m.contains("unknown") || m.contains("invalid"))
        })
}

/// Evento do servidor já classificado (ambas as nomenclaturas).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerEvent {
    SessionCreated,
    SessionUpdated,
    SpeechStarted,
    SpeechStopped,
    InputTranscript(String),
    ResponseCreated,
    AudioDelta {
        item_id: Option<String>,
        delta: String,
    },
    AudioDone,
    OutputTranscript(String),
    TextDone(String),
    FunctionCall {
        name: String,
        call_id: String,
        arguments: String,
    },
    ResponseDone {
        status: Option<String>,
        usage: Option<Value>,
    },
    Error {
        code: Option<String>,
        message: String,
        param: Option<String>,
    },
    RateLimits,
    Other(String),
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

pub fn parse_server_event(text: &str) -> Result<ServerEvent, serde_json::Error> {
    let v: Value = serde_json::from_str(text)?;
    Ok(classify(&v))
}

pub fn classify(v: &Value) -> ServerEvent {
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "session.created" => ServerEvent::SessionCreated,
        "session.updated" => ServerEvent::SessionUpdated,
        "input_audio_buffer.speech_started" => ServerEvent::SpeechStarted,
        "input_audio_buffer.speech_stopped" => ServerEvent::SpeechStopped,
        "conversation.item.input_audio_transcription.completed" => {
            ServerEvent::InputTranscript(str_field(v, "transcript").unwrap_or_default())
        }
        "response.created" => ServerEvent::ResponseCreated,
        "response.audio.delta" | "response.output_audio.delta" => ServerEvent::AudioDelta {
            item_id: str_field(v, "item_id"),
            delta: str_field(v, "delta").unwrap_or_default(),
        },
        "response.audio.done" | "response.output_audio.done" => ServerEvent::AudioDone,
        "response.audio_transcript.done" | "response.output_audio_transcript.done" => {
            ServerEvent::OutputTranscript(str_field(v, "transcript").unwrap_or_default())
        }
        "response.text.done" | "response.output_text.done" => {
            ServerEvent::TextDone(str_field(v, "text").unwrap_or_default())
        }
        "response.function_call_arguments.done" => ServerEvent::FunctionCall {
            name: str_field(v, "name").unwrap_or_default(),
            call_id: str_field(v, "call_id").unwrap_or_default(),
            arguments: match v.get("arguments") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            },
        },
        "response.done" => ServerEvent::ResponseDone {
            status: v.get("response").and_then(|r| str_field(r, "status")),
            usage: v.get("response").and_then(|r| r.get("usage")).cloned(),
        },
        "error" => {
            let err = v.get("error").cloned().unwrap_or(Value::Null);
            ServerEvent::Error {
                code: str_field(&err, "code"),
                message: str_field(&err, "message").unwrap_or_else(|| err.to_string()),
                param: str_field(&err, "param"),
            }
        }
        "rate_limits.updated" => ServerEvent::RateLimits,
        other => ServerEvent::Other(other.to_string()),
    }
}

/// Argumentos da ferramenta `ask_brain`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct AskBrainArgs {
    pub prompt: String,
    #[serde(default)]
    pub new_thread: bool,
    /// Thread existente (id/prefixo ou título) para continuar.
    #[serde(default)]
    pub thread: Option<String>,
    /// Título curto proposto para uma thread nova.
    #[serde(default)]
    pub title: Option<String>,
}

/// Faz o parse dos argumentos (string JSON) de `ask_brain`.
pub fn parse_ask_brain_args(raw: &str) -> Result<AskBrainArgs, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("argumentos vazios".into());
    }
    let v: Value = serde_json::from_str(raw).map_err(|e| format!("argumentos inválidos: {e}"))?;
    // Alguns modelos devolvem a string JSON duplamente codificada.
    let v = match v {
        Value::String(inner) => {
            serde_json::from_str(&inner).map_err(|e| format!("argumentos inválidos: {e}"))?
        }
        other => other,
    };
    let args: AskBrainArgs =
        serde_json::from_value(v).map_err(|e| format!("argumentos inválidos: {e}"))?;
    if args.prompt.trim().is_empty() {
        return Err("prompt vazio".into());
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> RealtimeSettings {
        RealtimeSettings {
            voice: "verse".into(),
            transcription_model: "whisper-1".into(),
            ..Default::default()
        }
    }

    #[test]
    fn legacy_shape() {
        let v = legacy_session_update(&settings());
        assert_eq!(v["type"], "session.update");
        let s = &v["session"];
        assert_eq!(s["modalities"], json!(["audio", "text"]));
        assert_eq!(s["voice"], "verse");
        assert_eq!(s["input_audio_format"], "pcm16");
        assert_eq!(s["output_audio_format"], "pcm16");
        assert_eq!(s["input_audio_transcription"]["model"], "whisper-1");
        assert_eq!(s["turn_detection"]["type"], "server_vad");
        assert_eq!(s["turn_detection"]["silence_duration_ms"], 600);
        assert_eq!(s["tool_choice"], "auto");
        let names: Vec<&str> = s["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["ask_brain", "list_threads", "brain_status"]);
        assert_eq!(s["tools"][0]["parameters"]["required"], json!(["prompt"]));
        assert!(s.get("type").is_none());
    }

    #[test]
    fn ga_shape() {
        let v = ga_session_update(&settings());
        let s = &v["session"];
        assert_eq!(s["type"], "realtime");
        assert_eq!(
            s["audio"]["input"]["format"],
            json!({"type": "audio/pcm", "rate": 24000})
        );
        assert_eq!(s["audio"]["input"]["turn_detection"]["type"], "server_vad");
        assert_eq!(s["audio"]["input"]["transcription"]["model"], "whisper-1");
        assert_eq!(s["audio"]["output"]["voice"], "verse");
        assert_eq!(s["audio"]["output"]["format"]["rate"], 24000);
        assert!(s.get("modalities").is_none());
        assert!(s.get("input_audio_format").is_none());
        assert_eq!(s["tools"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn empty_transcription_model_is_omitted() {
        let mut st = settings();
        st.transcription_model = "  ".into();
        assert!(ga_session_update(&st)["session"]["audio"]["input"]["transcription"].is_null());
        assert!(legacy_session_update(&st)["session"]["input_audio_transcription"].is_null());
    }

    #[test]
    fn ptt_disables_turn_detection_and_semantic_vad_has_no_silence() {
        let mut st = settings();
        st.activation = "push_to_talk".into();
        assert!(legacy_session_update(&st)["session"]["turn_detection"].is_null());
        assert!(ga_session_update(&st)["session"]["audio"]["input"]["turn_detection"].is_null());
        st.activation = "toggle".into();
        st.turn_detection = "semantic_vad".into();
        let td = &legacy_session_update(&st)["session"]["turn_detection"];
        assert_eq!(td["type"], "semantic_vad");
        assert!(td.get("silence_duration_ms").is_none());
    }

    #[test]
    fn variant_preference() {
        assert_eq!(
            SessionVariant::preferred_for("gpt-realtime"),
            SessionVariant::Ga
        );
        assert_eq!(
            SessionVariant::preferred_for("gpt-realtime-mini"),
            SessionVariant::Ga
        );
        assert_eq!(
            SessionVariant::preferred_for("gpt-4o-realtime-preview"),
            SessionVariant::Legacy
        );
        assert_eq!(SessionVariant::Ga.other(), SessionVariant::Legacy);
    }

    #[test]
    fn parses_ask_brain_args() {
        let a =
            parse_ask_brain_args(r#"{"prompt":"liste meus arquivos","new_thread":true}"#).unwrap();
        assert_eq!(a.prompt, "liste meus arquivos");
        assert!(a.new_thread);
        let b = parse_ask_brain_args(r#"{"prompt":"oi"}"#).unwrap();
        assert!(!b.new_thread);
        assert!(b.title.is_none());
        let t = parse_ask_brain_args(
            r#"{"prompt":"revise o relatório","new_thread":true,"title":"Relatório de vendas"}"#,
        )
        .unwrap();
        assert_eq!(t.title.as_deref(), Some("Relatório de vendas"));
        // duplamente codificado
        let c = parse_ask_brain_args(r#""{\"prompt\":\"x\"}""#).unwrap();
        assert_eq!(c.prompt, "x");
        assert!(parse_ask_brain_args("").is_err());
        assert!(parse_ask_brain_args("{}").is_err());
        assert!(parse_ask_brain_args(r#"{"prompt":"  "}"#).is_err());
        assert!(parse_ask_brain_args("not json").is_err());
    }

    #[test]
    fn classifies_both_namings() {
        let legacy = json!({"type":"response.audio.delta","item_id":"it_1","delta":"AAAA"});
        let ga = json!({"type":"response.output_audio.delta","item_id":"it_2","delta":"BBBB"});
        assert_eq!(
            classify(&legacy),
            ServerEvent::AudioDelta {
                item_id: Some("it_1".into()),
                delta: "AAAA".into()
            }
        );
        assert_eq!(
            classify(&ga),
            ServerEvent::AudioDelta {
                item_id: Some("it_2".into()),
                delta: "BBBB".into()
            }
        );
        assert_eq!(
            classify(&json!({"type":"response.output_audio_transcript.done","transcript":"olá"})),
            ServerEvent::OutputTranscript("olá".into())
        );
        assert_eq!(
            classify(&json!({"type":"response.audio_transcript.done","transcript":"oi"})),
            ServerEvent::OutputTranscript("oi".into())
        );
        let fc = json!({"type":"response.function_call_arguments.done","name":"ask_brain","call_id":"c1","arguments":"{\"prompt\":\"p\"}"});
        assert_eq!(
            classify(&fc),
            ServerEvent::FunctionCall {
                name: "ask_brain".into(),
                call_id: "c1".into(),
                arguments: "{\"prompt\":\"p\"}".into()
            }
        );
        let err = json!({"type":"error","error":{"type":"invalid_request_error","code":"unknown_parameter","message":"Unknown parameter: 'session.audio'.","param":"session.audio"}});
        match classify(&err) {
            ServerEvent::Error {
                code,
                message,
                param,
            } => {
                assert!(is_unknown_parameter_error(
                    code.as_deref(),
                    &message,
                    param.as_deref()
                ));
            }
            other => panic!("{other:?}"),
        }
        assert!(!is_unknown_parameter_error(
            Some("rate_limit_exceeded"),
            "Too many requests",
            None
        ));
        assert_eq!(
            classify(&json!({"type":"weird.event"})),
            ServerEvent::Other("weird.event".into())
        );
    }

    #[test]
    fn outbound_shapes() {
        let out = function_call_output("c1", &json!({"status":"working","task_id":"t1"}));
        assert_eq!(out["item"]["type"], "function_call_output");
        assert_eq!(out["item"]["call_id"], "c1");
        let parsed: Value = serde_json::from_str(out["item"]["output"].as_str().unwrap()).unwrap();
        assert_eq!(parsed["status"], "working");
        let item = user_text_item("[Resultado]: ok");
        assert_eq!(item["item"]["role"], "user");
        assert_eq!(item["item"]["content"][0]["type"], "input_text");
        let tr = truncate_item("it", 1234);
        assert_eq!(tr["audio_end_ms"], 1234);
        assert_eq!(tr["content_index"], 0);
    }
}
