//! Ferramentas MCP `schedule_*` (família `scheduler`). Todas `auto` por padrão:
//! agendar não tem efeito externo; o efeito acontece quando o agente age no disparo.

use crate::{ScheduleInput, SchedulerModule, FAMILY};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use will_core::tools::{arg_opt_bool, arg_opt_str, arg_str};
use will_core::{
    Module, ToolContext, ToolError, ToolOutput, ToolPolicy, ToolProvider, ToolSpec, WillTool,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Create,
    List,
    Get,
    Update,
    Delete,
}

impl Kind {
    const ALL: [Kind; 5] = [
        Kind::Create,
        Kind::List,
        Kind::Get,
        Kind::Update,
        Kind::Delete,
    ];
    fn name(self) -> &'static str {
        match self {
            Kind::Create => "schedule_create",
            Kind::List => "schedule_list",
            Kind::Get => "schedule_get",
            Kind::Update => "schedule_update",
            Kind::Delete => "schedule_delete",
        }
    }
}

pub struct SchedulerProvider {
    module: Arc<SchedulerModule>,
}

impl SchedulerProvider {
    pub fn new(module: Arc<SchedulerModule>) -> Self {
        Self { module }
    }
}

impl ToolProvider for SchedulerProvider {
    fn family(&self) -> &'static str {
        FAMILY
    }
    fn tools(&self) -> Vec<Arc<dyn WillTool>> {
        Kind::ALL
            .iter()
            .map(|k| {
                Arc::new(ScheduleTool {
                    kind: *k,
                    module: self.module.clone(),
                }) as Arc<dyn WillTool>
            })
            .collect()
    }
    fn available(&self) -> bool {
        self.module.enabled()
    }
}

struct ScheduleTool {
    kind: Kind,
    module: Arc<SchedulerModule>,
}

fn schema(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required})
}

fn fields() -> Value {
    json!({
        "name": {"type": "string", "description": "Nome curto (até 80 caracteres), ex.: \"Resumo das notícias\""},
        "message": {"type": "string", "description": "Texto que você receberá como mensagem quando disparar. Escreva como uma instrução completa para você mesmo (o que fazer, para quem enviar), pois o contexto da conversa pode ter sido resumido."},
        "cron": {"type": "string", "description": "Expressão cron de 5 campos (min hora dia mês dia-da-semana), ex.: \"0 8 * * *\" = todo dia às 08:00; \"30 9 * * 1-5\" = dias úteis às 09:30. Exclusivo com run_at."},
        "run_at": {"type": "string", "description": "Disparo único: YYYY-MM-DDTHH:MM (no fuso do agendamento) ou ISO-8601 com fuso. Exclusivo com cron."},
        "timezone": {"type": "string", "description": "Fuso IANA (ex.: America/Sao_Paulo). Padrão: fuso configurado/sistema."}
    })
}

fn field_subset(keys: &[&str]) -> Value {
    let all = fields();
    let mut out = serde_json::Map::new();
    for k in keys {
        if let Some(v) = all.get(*k) {
            out.insert((*k).to_string(), v.clone());
        }
    }
    Value::Object(out)
}

#[async_trait]
impl WillTool for ScheduleTool {
    fn spec(&self) -> ToolSpec {
        let (description, input_schema) = match self.kind {
            Kind::Create => (
                "Agenda uma mensagem para você mesmo nesta conversa: recorrente (cron) ou única (run_at). Quando chegar a hora, o serviço entrega `message` nesta thread como se o usuário tivesse escrito, e você executa o que ela pede. Use para lembretes, rotinas (\"todo dia às 8 resuma as notícias e envie no Telegram\") e tarefas futuras. Responde com o agendamento criado, incluindo o próximo disparo.",
                schema(field_subset(&["name", "message", "cron", "run_at", "timezone"]), &["name", "message"]),
            ),
            Kind::List => (
                "Lista agendamentos com próximo disparo, thread dona e estado. Por padrão só os desta conversa; `all: true` lista todos.",
                schema(json!({"all": {"type": "boolean", "description": "Listar agendamentos de todas as threads (padrão: só desta)"}}), &[]),
            ),
            Kind::Get => (
                "Detalha um agendamento (aceita id completo ou prefixo) com histórico dos últimos disparos.",
                schema(json!({"id": {"type": "string"}}), &["id"]),
            ),
            Kind::Update => (
                "Altera campos de um agendamento: name, message, cron ou run_at, timezone, enabled (pausar/retomar). Só os campos enviados mudam.",
                {
                    let mut props = field_subset(&["name", "message", "cron", "run_at", "timezone"]);
                    if let Some(o) = props.as_object_mut() {
                        o.insert("id".into(), json!({"type": "string"}));
                        o.insert("enabled".into(), json!({"type": "boolean", "description": "false pausa, true retoma"}));
                    }
                    schema(props, &["id"])
                },
            ),
            Kind::Delete => (
                "Remove um agendamento definitivamente (para pausar, use schedule_update com enabled=false).",
                schema(json!({"id": {"type": "string"}}), &["id"]),
            ),
        };
        ToolSpec {
            name: self.kind.name().into(),
            description: description.into(),
            input_schema,
            requires: vec![Module::Scheduler],
            default_policy: ToolPolicy::Auto,
            family: FAMILY.into(),
        }
    }

    fn approval_summary(&self, args: &Value) -> String {
        match self.kind {
            Kind::Create => format!(
                "Agendar \"{}\" ({})",
                arg_opt_str(args, "name").unwrap_or("?"),
                arg_opt_str(args, "cron")
                    .map(|c| format!("cron {c}"))
                    .or_else(|| arg_opt_str(args, "run_at").map(|a| format!("em {a}")))
                    .unwrap_or_default()
            ),
            Kind::Update => format!(
                "Alterar agendamento {}",
                arg_opt_str(args, "id").unwrap_or("?")
            ),
            Kind::Delete => format!(
                "Remover agendamento {}",
                arg_opt_str(args, "id").unwrap_or("?")
            ),
            Kind::List => "Listar agendamentos".into(),
            Kind::Get => format!("Ver agendamento {}", arg_opt_str(args, "id").unwrap_or("?")),
        }
    }

    async fn call(&self, ctx: ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let m = &self.module;
        if !m.enabled() {
            tracing::warn!(tool = self.kind.name(), thread_id = ?ctx.thread_id, "ferramenta chamada com o scheduler desligado");
            return Err(ToolError::Unavailable(
                "scheduler desligado nas configurações".into(),
            ));
        }
        tracing::debug!(tool = self.kind.name(), thread_id = ?ctx.thread_id, "ferramenta de agendamento chamada");
        let input = || -> ScheduleInput {
            ScheduleInput {
                name: arg_opt_str(&args, "name").map(str::to_string),
                message: arg_opt_str(&args, "message").map(str::to_string),
                cron: arg_opt_str(&args, "cron").map(str::to_string),
                run_at: arg_opt_str(&args, "run_at").map(str::to_string),
                timezone: arg_opt_str(&args, "timezone").map(str::to_string),
                enabled: arg_opt_bool(&args, "enabled"),
                thread_id: None,
            }
        };
        match self.kind {
            Kind::Create => {
                let thread = ctx.thread_id.ok_or_else(|| {
                    tracing::warn!("schedule_create chamado sem thread (token genérico)");
                    ToolError::Forbidden("schedule_create só funciona a partir de uma thread (o disparo volta para ela)".into())
                })?;
                let v = m.create(input(), thread, "agent").await.map_err(|e| {
                    tracing::debug!(thread_id = %thread, error = %e, "schedule_create recusado");
                    ToolError::InvalidArgs(e)
                })?;
                Ok(ToolOutput::json(&json!({
                    "ok": true,
                    "schedule": v,
                    "hint": "Quando disparar, você receberá a mensagem com um cabeçalho [Agendamento ...]. Responda ao usuário confirmando o que foi agendado e quando."
                })))
            }
            Kind::List => {
                let all = arg_opt_bool(&args, "all").unwrap_or(false);
                let list = m
                    .list(if all { None } else { ctx.thread_id })
                    .await
                    .map_err(ToolError::Failed)?;
                Ok(ToolOutput::json(
                    &json!({"count": list.len(), "schedules": list}),
                ))
            }
            Kind::Get => {
                let id = m
                    .resolve_id(arg_str(&args, "id")?)
                    .await
                    .map_err(ToolError::InvalidArgs)?;
                let v = m.get(id).await.map_err(ToolError::Failed)?;
                let runs = m.runs(id).await.unwrap_or_default();
                Ok(ToolOutput::json(&json!({"schedule": v, "runs": runs})))
            }
            Kind::Update => {
                let id = m
                    .resolve_id(arg_str(&args, "id")?)
                    .await
                    .map_err(ToolError::InvalidArgs)?;
                let v = m
                    .update(id, input())
                    .await
                    .map_err(ToolError::InvalidArgs)?;
                Ok(ToolOutput::json(&json!({"ok": true, "schedule": v})))
            }
            Kind::Delete => {
                let id = m
                    .resolve_id(arg_str(&args, "id")?)
                    .await
                    .map_err(ToolError::InvalidArgs)?;
                m.delete(id).await.map_err(ToolError::Failed)?;
                Ok(ToolOutput::json(&json!({"ok": true, "deleted": id})))
            }
        }
    }
}
