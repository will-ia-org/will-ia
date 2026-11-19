//! Ferramentas MCP das gravações de conversa (docs/32): buscar por assunto
//! (embeddings), listar e ler uma conversa. Só leitura.

use crate::app::AppState;
use crate::recordings as rec;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::{Arc, Weak};
use will_core::tools::{arg_opt_u64, arg_str};
use will_core::{ToolContext, ToolError, ToolOutput, ToolPolicy, ToolProvider, ToolSpec, WillTool};
use will_storage::recordings::Recording;

pub const FAMILY: &str = "recordings";

/// Provedor das ferramentas `recordings_*`. Guarda o estado por referência
/// fraca: o registro de ferramentas pertence ao próprio estado.
pub struct RecordingsProvider {
    pub state: Weak<AppState>,
}

impl ToolProvider for RecordingsProvider {
    fn family(&self) -> &'static str {
        FAMILY
    }
    fn tools(&self) -> Vec<Arc<dyn WillTool>> {
        [Kind::Search, Kind::List, Kind::Get]
            .into_iter()
            .map(|kind| {
                Arc::new(RecordingTool {
                    state: self.state.clone(),
                    kind,
                }) as Arc<dyn WillTool>
            })
            .collect()
    }
    fn available(&self) -> bool {
        true
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Search,
    List,
    Get,
}

struct RecordingTool {
    state: Weak<AppState>,
    kind: Kind,
}

fn schema(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required})
}

/// Ficha resumida de uma gravação para o agente.
fn card(r: &Recording) -> Value {
    json!({
        "id": r.id,
        "started_at": r.started_at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string(),
        "duration": rec::clock(r.duration_secs),
        "with": rec::other_label(r),
        "subject": r.subject,
        "status": r.status,
        "summary": r.summary,
    })
}

#[async_trait]
impl WillTool for RecordingTool {
    fn spec(&self) -> ToolSpec {
        let (name, description, input_schema) = match self.kind {
            Kind::Search => (
                "recordings_search",
                "Busca por assunto (semântica) nas conversas gravadas pelo usuário (ligações, reuniões): \
                 devolve os trechos e resumos mais próximos do texto buscado, com a gravação, a data, \
                 com quem foi e o momento da fala. Use para perguntas como \"quando falei sobre imóveis?\" \
                 ou \"o que combinei com o André?\". Para ler a conversa inteira, use recordings_get.",
                schema(
                    json!({
                        "query": {"type": "string", "description": "O que procurar, em linguagem natural."},
                        "limit": {"type": "integer", "description": "Quantos trechos devolver (padrão 8, máximo 30)."},
                    }),
                    &["query"],
                ),
            ),
            Kind::List => (
                "recordings_list",
                "Lista as conversas gravadas pelo usuário, da mais recente para a mais antiga: id, data, \
                 duração, com quem foi, assunto e resumo.",
                schema(
                    json!({
                        "limit": {"type": "integer", "description": "Quantas devolver (padrão 20, máximo 200)."},
                    }),
                    &[],
                ),
            ),
            Kind::Get => (
                "recordings_get",
                "Devolve uma conversa gravada: ficha, resumo e a transcrição completa com o tempo de cada \
                 fala (\"Você\" é o usuário; o outro nome é com quem ele falou).",
                schema(
                    json!({
                        "id": {"type": "string", "description": "Id da gravação (de recordings_search ou recordings_list)."},
                    }),
                    &["id"],
                ),
            ),
        };
        ToolSpec {
            name: name.into(),
            description: description.into(),
            input_schema,
            requires: vec![],
            default_policy: ToolPolicy::Auto,
            family: FAMILY.into(),
        }
    }

    async fn call(&self, _ctx: ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| ToolError::Unavailable("serviço encerrando".into()))?;
        let failed = |e: will_storage::StorageError| ToolError::Failed(e.to_string());
        match self.kind {
            Kind::Search => {
                let query = arg_str(&args, "query")?;
                let limit = arg_opt_u64(&args, "limit").unwrap_or(8).clamp(1, 30) as usize;
                let hits = rec::search(&state, query, limit)
                    .await
                    .map_err(ToolError::Failed)?;
                let mut out = Vec::with_capacity(hits.len());
                // Uma consulta ao banco por gravação distinta, não por trecho.
                let mut cards: std::collections::HashMap<String, Recording> = Default::default();
                for h in hits {
                    if !cards.contains_key(&h.recording_id) {
                        match state.storage.get_recording(&h.recording_id).await {
                            Ok(r) => {
                                cards.insert(h.recording_id.clone(), r);
                            }
                            // Índice de uma gravação apagada: ignora o trecho.
                            Err(_) => continue,
                        }
                    }
                    let r = &cards[&h.recording_id];
                    let text = if h.kind == "summary" {
                        h.text.clone()
                    } else {
                        rec::render_piece(&h.text, rec::other_label(r))
                    };
                    out.push(json!({
                        "recording_id": r.id,
                        "started_at": r.started_at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string(),
                        "with": rec::other_label(r),
                        "subject": r.subject,
                        "score": (h.score * 1000.0).round() / 1000.0,
                        "kind": h.kind,
                        "at": rec::clock(h.start_secs),
                        "text": text,
                    }));
                }
                tracing::info!(
                    query_chars = query.chars().count(),
                    hits = out.len(),
                    "recordings_search"
                );
                Ok(ToolOutput::json(&json!({
                    "query": query,
                    "hits": out,
                    "note": "kind=summary é o resumo da conversa inteira; kind=speech é um trecho das falas a partir do minuto `at`. A transcrição é automática e pode ter erros.",
                })))
            }
            Kind::List => {
                let limit = arg_opt_u64(&args, "limit").unwrap_or(20).clamp(1, 200) as i64;
                let rows = state.storage.list_recordings(limit).await.map_err(failed)?;
                let out: Vec<Value> = rows.iter().map(card).collect();
                Ok(ToolOutput::json(&json!({ "recordings": out })))
            }
            Kind::Get => {
                let id = arg_str(&args, "id")?;
                if !rec::valid_id(id) {
                    return Err(ToolError::InvalidArgs("id de gravação inválido".into()));
                }
                let r = state.storage.get_recording(id).await.map_err(|e| match e {
                    will_storage::StorageError::NotFound => {
                        ToolError::Failed("gravação não encontrada".into())
                    }
                    other => failed(other),
                })?;
                let segments = rec::read_transcript(&state, &r).await;
                let mut v = card(&r);
                v["transcript"] = json!(rec::transcript_text(&r, &segments));
                Ok(ToolOutput::json(&v))
            }
        }
    }
}
