//! Ferramentas do próprio serviço: notify_user, list_threads, create_thread, send_to_thread, kb_info.

use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use will_core::tools::{arg_opt_bool, arg_opt_str, arg_str};
use will_core::{
    Event, EventBus, Module, Origin, SharedThreadHost, ThreadId, ToolContext, ToolError,
    ToolOutput, ToolPolicy, ToolProvider, ToolSpec, WillTool,
};

pub struct CoreTools {
    pub host: SharedThreadHost,
    pub bus: EventBus,
    pub kb_path: PathBuf,
    pub mcp_url: Arc<parking_lot::RwLock<String>>,
    pub enable_thread_tools: bool,
}

impl ToolProvider for CoreTools {
    fn family(&self) -> &'static str {
        "core"
    }
    fn tools(&self) -> Vec<Arc<dyn WillTool>> {
        let mut v: Vec<Arc<dyn WillTool>> = vec![
            Arc::new(NotifyUser {
                bus: self.bus.clone(),
            }),
            Arc::new(KbInfo {
                kb_path: self.kb_path.clone(),
                mcp_url: self.mcp_url.clone(),
            }),
            Arc::new(ListThreads {
                host: self.host.clone(),
            }),
            Arc::new(CreateThread {
                host: self.host.clone(),
            }),
            Arc::new(SendToThread {
                host: self.host.clone(),
            }),
        ];
        if !self.enable_thread_tools {
            v.truncate(2);
        }
        v
    }
    fn available(&self) -> bool {
        true
    }
}

fn schema(props: serde_json::Value, required: &[&str]) -> serde_json::Value {
    serde_json::json!({"type": "object", "properties": props, "required": required})
}

struct NotifyUser {
    bus: EventBus,
}
#[async_trait]
impl WillTool for NotifyUser {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "notify_user".into(),
            description: "Mostra uma notificação nativa no desktop do usuário (título + corpo curto). Use para avisar que uma tarefa longa terminou.".into(),
            input_schema: schema(serde_json::json!({"title": {"type": "string"}, "body": {"type": "string"}}), &["title", "body"]),
            requires: vec![],
            default_policy: ToolPolicy::Auto,
            family: "core".into(),
        }
    }
    async fn call(
        &self,
        _ctx: ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let title = arg_str(&args, "title")?.to_string();
        let body = arg_str(&args, "body")?.to_string();
        self.bus.emit(Event::Notification { title, body });
        Ok(ToolOutput::text("ok"))
    }
}

struct KbInfo {
    kb_path: PathBuf,
    mcp_url: Arc<parking_lot::RwLock<String>>,
}
#[async_trait]
impl WillTool for KbInfo {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "kb_info".into(),
            description: "Informações da base de conhecimento: caminho, URL do MCP local e catálogo de ferramentas em tools/README.md.".into(),
            input_schema: schema(serde_json::json!({}), &[]),
            requires: vec![],
            default_policy: ToolPolicy::Auto,
            family: "core".into(),
        }
    }
    async fn call(
        &self,
        ctx: ToolContext,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let readme = std::fs::read_to_string(self.kb_path.join("tools/README.md")).ok();
        Ok(ToolOutput::json(&serde_json::json!({
            "kb_path": self.kb_path,
            "mcp_url": *self.mcp_url.read(),
            "thread_id": ctx.thread_id,
            "origin": ctx.origin,
            "tools_readme": readme.map(|r| will_core::tools::truncate(&r, 8000)),
        })))
    }
}

struct ListThreads {
    host: SharedThreadHost,
}
#[async_trait]
impl WillTool for ListThreads {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_threads".into(),
            description:
                "Lista as threads (conversas) ativas do serviço com id, origem, título e estado."
                    .into(),
            input_schema: schema(serde_json::json!({}), &[]),
            requires: vec![Module::Engine],
            default_policy: ToolPolicy::Auto,
            family: "core".into(),
        }
    }
    async fn call(
        &self,
        _ctx: ToolContext,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let t = self.host.list_threads().await.map_err(ToolError::Failed)?;
        Ok(ToolOutput::json(&t))
    }
}

struct CreateThread {
    host: SharedThreadHost,
}
#[async_trait]
impl WillTool for CreateThread {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "create_thread".into(),
            description: "Cria uma nova thread (conversa paralela com o cérebro) e opcionalmente envia um prompt inicial. Útil para delegar trabalho longo.".into(),
            input_schema: schema(serde_json::json!({"title": {"type": "string"}, "initial_prompt": {"type": "string"}}), &["title"]),
            requires: vec![Module::Engine],
            default_policy: ToolPolicy::Confirm,
            family: "core".into(),
        }
    }
    fn approval_summary(&self, args: &serde_json::Value) -> String {
        format!(
            "Criar thread '{}'",
            arg_opt_str(args, "title").unwrap_or("sem título")
        )
    }
    async fn call(
        &self,
        _ctx: ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let title = arg_str(&args, "title")?;
        let t = self
            .host
            .ensure_thread(&Origin::Ui, Some(title))
            .await
            .map_err(ToolError::Failed)?;
        let mut turn = None;
        if let Some(p) = arg_opt_str(&args, "initial_prompt") {
            turn = Some(
                self.host
                    .send_message(t.id, p)
                    .await
                    .map_err(ToolError::Failed)?,
            );
        }
        Ok(ToolOutput::json(
            &serde_json::json!({"thread_id": t.id, "turn_id": turn}),
        ))
    }
}

struct SendToThread {
    host: SharedThreadHost,
}
#[async_trait]
impl WillTool for SendToThread {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "send_to_thread".into(),
            description: "Envia uma mensagem a outra thread existente (entra imediatamente, mesmo se ela estiver trabalhando).".into(),
            input_schema: schema(serde_json::json!({"thread_id": {"type": "string"}, "text": {"type": "string"}, "wait": {"type": "boolean", "description": "ignorado nesta versão"}}), &["thread_id", "text"]),
            requires: vec![Module::Engine],
            default_policy: ToolPolicy::Confirm,
            family: "core".into(),
        }
    }
    async fn call(
        &self,
        _ctx: ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let id = arg_str(&args, "thread_id")?;
        let tid =
            ThreadId::parse(id).map_err(|_| ToolError::InvalidArgs("thread_id inválido".into()))?;
        let text = arg_str(&args, "text")?;
        let _ = arg_opt_bool(&args, "wait");
        let turn = self
            .host
            .send_message(tid, text)
            .await
            .map_err(ToolError::Failed)?;
        Ok(ToolOutput::json(&serde_json::json!({"turn_id": turn})))
    }
}
