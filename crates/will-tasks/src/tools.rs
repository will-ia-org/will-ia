//! Ferramentas MCP `tasks_*` (família `tasks`). Todas `auto`: mexer nas
//! tarefas não tem efeito externo (o aviso no Telegram é do laço, opcional).

use crate::{TaskInput, TasksModule, FAMILY};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use will_core::tools::{arg_opt_str, arg_str};
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
    Complete,
    Skip,
    Reopen,
}

impl Kind {
    const ALL: [Kind; 8] = [
        Kind::Create,
        Kind::List,
        Kind::Get,
        Kind::Update,
        Kind::Delete,
        Kind::Complete,
        Kind::Skip,
        Kind::Reopen,
    ];
    fn name(self) -> &'static str {
        match self {
            Kind::Create => "tasks_create",
            Kind::List => "tasks_list",
            Kind::Get => "tasks_get",
            Kind::Update => "tasks_update",
            Kind::Delete => "tasks_delete",
            Kind::Complete => "tasks_complete",
            Kind::Skip => "tasks_skip",
            Kind::Reopen => "tasks_reopen",
        }
    }
}

pub struct TasksProvider {
    module: Arc<TasksModule>,
}

impl TasksProvider {
    pub fn new(module: Arc<TasksModule>) -> Self {
        Self { module }
    }
}

impl ToolProvider for TasksProvider {
    fn family(&self) -> &'static str {
        FAMILY
    }
    fn tools(&self) -> Vec<Arc<dyn WillTool>> {
        Kind::ALL
            .iter()
            .map(|k| {
                Arc::new(TaskTool {
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

struct TaskTool {
    kind: Kind,
    module: Arc<TasksModule>,
}

fn schema(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required})
}

fn fields() -> Value {
    json!({
        "title": {"type": "string", "description": "Título curto e visível (até 120 caracteres)."},
        "description": {"type": "string", "description": "O que a tarefa envolve (texto livre, até 8000 caracteres)."},
        "start_at": {"type": "string", "description": "Início com hora, obrigatório: YYYY-MM-DDTHH:MM no fuso configurado (ex.: 2026-11-10T14:00) ou ISO-8601 com fuso. Só a data não vale."},
        "end_at": {"type": "string", "description": "Fim (opcional), mesmo formato. Depois dele, sem conclusão, a tarefa fica atrasada. Sem fim, fica em andamento até ser concluída. Na edição, \"\" limpa."},
        "priority": {"type": "string", "enum": ["high", "medium", "low"], "description": "Prioridade: high (alta), medium (média, padrão), low (baixa)."},
        "id": {"type": "string", "description": "Id da tarefa (completo ou prefixo único)."}
    })
}

fn subset(keys: &[&str]) -> Value {
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
impl WillTool for TaskTool {
    fn spec(&self) -> ToolSpec {
        let (description, input_schema) = match self.kind {
            Kind::Create => (
                "Cria uma tarefa no painel de tarefas (Gantt) do usuário: título, descrição, início com hora (obrigatório), fim (opcional) e prioridade. O estado é automático: futura → em andamento (azul) → atrasada (vermelha, se passou do fim) → concluída (verde).",
                schema(subset(&["title", "description", "start_at", "end_at", "priority"]), &["title", "start_at"]),
            ),
            Kind::List => (
                "Lista tarefas com estado e horários no fuso configurado. `filter`: pending (padrão: tudo que não foi fechado), running (em andamento), overdue (atrasadas), upcoming (futuras), done (concluídas), skipped (não realizadas) ou all.",
                schema(json!({"filter": {"type": "string", "enum": ["pending", "running", "overdue", "upcoming", "done", "skipped", "all"]}}), &[]),
            ),
            Kind::Get => (
                "Detalha uma tarefa pelo id (completo ou prefixo).",
                schema(subset(&["id"]), &["id"]),
            ),
            Kind::Update => (
                "Altera campos de uma tarefa: title, description, start_at, end_at (\"\" limpa), priority. Só os campos enviados mudam.",
                schema(subset(&["id", "title", "description", "start_at", "end_at", "priority"]), &["id"]),
            ),
            Kind::Delete => (
                "Remove uma tarefa definitivamente (para encerrar sem apagar, use tasks_complete).",
                schema(subset(&["id"]), &["id"]),
            ),
            Kind::Complete => (
                "Marca a tarefa como concluída (fica verde e some do Gantt por padrão).",
                schema(subset(&["id"]), &["id"]),
            ),
            Kind::Skip => (
                "Fecha a tarefa como NÃO realizada (o usuário faltou/desistiu): some do Gantt como a concluída, mas fica registrado que não foi feita. Use quando o usuário disser que não fez, pulou ou faltou — não use tasks_complete nesse caso.",
                schema(subset(&["id"]), &["id"]),
            ),
            Kind::Reopen => (
                "Desfaz a conclusão ou a marcação de não realizada: a tarefa volta a pendente (o estado volta a seguir o relógio).",
                schema(subset(&["id"]), &["id"]),
            ),
        };
        ToolSpec {
            name: self.kind.name().into(),
            description: description.into(),
            input_schema,
            requires: vec![Module::Tasks],
            default_policy: ToolPolicy::Auto,
            family: FAMILY.into(),
        }
    }

    fn approval_summary(&self, args: &Value) -> String {
        let id = || arg_opt_str(args, "id").unwrap_or("?");
        match self.kind {
            Kind::Create => format!(
                "Criar tarefa \"{}\" (início {})",
                arg_opt_str(args, "title").unwrap_or("?"),
                arg_opt_str(args, "start_at").unwrap_or("?")
            ),
            Kind::List => "Listar tarefas".into(),
            Kind::Get => format!("Ver tarefa {}", id()),
            Kind::Update => format!("Alterar tarefa {}", id()),
            Kind::Delete => format!("Remover tarefa {}", id()),
            Kind::Complete => format!("Concluir tarefa {}", id()),
            Kind::Skip => format!("Marcar tarefa {} como não realizada", id()),
            Kind::Reopen => format!("Reabrir tarefa {}", id()),
        }
    }

    async fn call(&self, ctx: ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let m = &self.module;
        if !m.enabled() {
            return Err(ToolError::Unavailable(
                "tarefas desligadas nas configurações".into(),
            ));
        }
        tracing::debug!(tool = self.kind.name(), thread_id = ?ctx.thread_id, "ferramenta de tarefas chamada");
        let input = || TaskInput {
            title: arg_opt_str(&args, "title").map(str::to_string),
            description: arg_opt_str(&args, "description").map(str::to_string),
            start_at: arg_opt_str(&args, "start_at").map(str::to_string),
            end_at: args
                .get("end_at")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            priority: arg_opt_str(&args, "priority").map(str::to_string),
        };
        match self.kind {
            Kind::Create => {
                let v = m
                    .create(input(), "agent")
                    .await
                    .map_err(ToolError::InvalidArgs)?;
                Ok(ToolOutput::json(&json!({"ok": true, "task": v})))
            }
            Kind::List => {
                let filter = arg_opt_str(&args, "filter").unwrap_or("pending");
                let list = m.list(filter).await.map_err(ToolError::InvalidArgs)?;
                Ok(ToolOutput::json(&json!({
                    "count": list.len(),
                    "filter": filter,
                    "timezone": m.zone().name(),
                    "tasks": list,
                })))
            }
            Kind::Get => {
                let id = m
                    .resolve_id(arg_str(&args, "id")?)
                    .await
                    .map_err(ToolError::InvalidArgs)?;
                let v = m.get(id).await.map_err(ToolError::Failed)?;
                Ok(ToolOutput::json(&json!({"task": v})))
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
                Ok(ToolOutput::json(&json!({"ok": true, "task": v})))
            }
            Kind::Delete => {
                let id = m
                    .resolve_id(arg_str(&args, "id")?)
                    .await
                    .map_err(ToolError::InvalidArgs)?;
                m.delete(id).await.map_err(ToolError::Failed)?;
                Ok(ToolOutput::json(&json!({"ok": true, "deleted": id})))
            }
            Kind::Complete | Kind::Skip | Kind::Reopen => {
                let id = m
                    .resolve_id(arg_str(&args, "id")?)
                    .await
                    .map_err(ToolError::InvalidArgs)?;
                let v = match self.kind {
                    Kind::Skip => m.skip(id).await,
                    k => m.set_done(id, k == Kind::Complete).await,
                }
                .map_err(ToolError::Failed)?;
                Ok(ToolOutput::json(&json!({"ok": true, "task": v})))
            }
        }
    }
}
