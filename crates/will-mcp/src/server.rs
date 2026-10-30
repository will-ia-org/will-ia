//! Servidor MCP `will_ia`: axum + rmcp Streamable HTTP, bearer por processo, políticas.

use crate::redact_args;
use crate::registry::ToolRegistry;
use crate::tokens::{Caller, TokenRegistry};
use axum::extract::{Request, State};
use axum::http::{header::AUTHORIZATION, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use rmcp::{ErrorData, RoleServer};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use will_core::{
    ApprovalDecision, ApprovalManager, Event, EventBus, ThreadId, ToolContext, ToolError,
    ToolPolicy,
};

#[derive(Debug, Clone)]
pub struct CallRecord {
    pub thread_id: Option<ThreadId>,
    pub tool: String,
    pub args_redacted: String,
    pub ok: bool,
    pub error: Option<String>,
    pub duration_ms: u64,
}

pub type OnCall = Arc<dyn Fn(CallRecord) + Send + Sync>;

pub struct McpServerDeps {
    pub registry: Arc<ToolRegistry>,
    pub tokens: Arc<TokenRegistry>,
    pub approvals: ApprovalManager,
    pub bus: EventBus,
    pub approval_timeout: Duration,
    pub on_call: OnCall,
}

#[derive(Clone)]
pub struct WillMcpServer {
    inner: Arc<McpServerDeps>,
}

impl WillMcpServer {
    fn caller(ctx: &RequestContext<RoleServer>) -> Option<Caller> {
        ctx.extensions
            .get::<http::request::Parts>()
            .and_then(|p| p.extensions.get::<Caller>().cloned())
            .or_else(|| ctx.extensions.get::<Caller>().cloned())
    }

    fn to_rmcp_tool(spec: &will_core::ToolSpec) -> Tool {
        let schema: rmcp::model::JsonObject = match &spec.input_schema {
            serde_json::Value::Object(m) => m.clone(),
            _ => serde_json::Map::new(),
        };
        Tool::new(
            spec.name.clone(),
            spec.description.clone(),
            Arc::new(schema),
        )
    }

    fn tool_output(o: will_core::ToolOutput) -> CallToolResult {
        let mut r = if o.is_error {
            CallToolResult::error(vec![ContentBlock::text(o.text)])
        } else {
            CallToolResult::success(vec![ContentBlock::text(o.text)])
        };
        if let Some(s) = o.structured {
            r.structured_content = Some(s);
        }
        r
    }

    async fn run_call(
        &self,
        name: &str,
        args: serde_json::Value,
        caller: Option<Caller>,
    ) -> CallToolResult {
        let call_id = uuid::Uuid::now_v7();
        let thread_id = caller.as_ref().and_then(|c| c.thread_id);
        let span = tracing::info_span!(
            "tool_call",
            tool = %name,
            call_id = %call_id,
            thread_id = thread_id.map(|t| t.to_string()).unwrap_or_default(),
            policy = tracing::field::Empty,
            ok = tracing::field::Empty,
            duration_ms = tracing::field::Empty,
            generic = caller.as_ref().map(|c| c.generic).unwrap_or(false)
        );
        self.run_call_inner(name, args, caller)
            .instrument(span)
            .await
    }

    async fn run_call_inner(
        &self,
        name: &str,
        args: serde_json::Value,
        caller: Option<Caller>,
    ) -> CallToolResult {
        let start = Instant::now();
        let deps = &self.inner;
        let thread_id = caller.as_ref().and_then(|c| c.thread_id);
        let origin = caller.as_ref().and_then(|c| c.origin.clone());
        let Some((spec, tool, available)) = deps.registry.find(name) else {
            tracing::warn!("ferramenta desconhecida chamada pelo agente");
            return CallToolResult::error(vec![ContentBlock::text(format!(
                "ferramenta desconhecida: {name}"
            ))]);
        };
        let (policy, overridden) = deps.registry.policy_for(&spec);
        let span = tracing::Span::current();
        span.record("policy", policy.as_str());
        tracing::debug!(policy = policy.as_str(), overridden, family = %spec.family, "ferramenta chamada");
        let redacted = redact_args(&args);
        let finish = |ok: bool, err: Option<String>, out: CallToolResult| {
            let dur = start.elapsed().as_millis() as u64;
            let span = tracing::Span::current();
            span.record("ok", ok);
            span.record("duration_ms", dur);
            match &err {
                None => tracing::info!(ok, duration_ms = dur, "ferramenta executada"),
                Some(e) => tracing::warn!(
                    ok,
                    duration_ms = dur,
                    error = %will_core::tools::truncate(e, 200),
                    "ferramenta falhou ou foi recusada"
                ),
            }
            (deps.on_call)(CallRecord {
                thread_id,
                tool: name.to_string(),
                args_redacted: redacted.to_string(),
                ok,
                error: err,
                duration_ms: dur,
            });
            deps.bus.emit(Event::McpToolCalled {
                thread_id,
                tool: name.to_string(),
                ok,
                duration_ms: dur,
            });
            out
        };
        if !available {
            return finish(false, Some("módulo indisponível".into()), CallToolResult::error(vec![ContentBlock::text(format!("A ferramenta {name} não está disponível agora (módulo desligado ou não autorizado)."))]));
        }
        match policy {
            ToolPolicy::Off => {
                return finish(
                    false,
                    Some("desligada".into()),
                    CallToolResult::error(vec![ContentBlock::text(format!(
                        "A ferramenta {name} está desligada nas configurações."
                    ))]),
                );
            }
            ToolPolicy::Confirm => {
                let summary = tool.approval_summary(&args);
                tracing::info!(
                    timeout_secs = deps.approval_timeout.as_secs(),
                    "política confirmar: aguardando aprovação do usuário"
                );
                let (req, decision, via) = deps
                    .approvals
                    .request(
                        thread_id,
                        name,
                        summary,
                        redacted.clone(),
                        deps.approval_timeout,
                    )
                    .await;
                let decision_label = match decision {
                    ApprovalDecision::Approved => "approved",
                    ApprovalDecision::Denied => "denied",
                    ApprovalDecision::Timeout => "expired",
                };
                let approval_span = tracing::info_span!(
                    "approval",
                    approval_id = %req.id,
                    tool = %name,
                    thread_id = thread_id.map(|t| t.to_string()).unwrap_or_default(),
                    decision = decision_label
                );
                approval_span.in_scope(|| {
                    tracing::debug!(
                        %via,
                        wait_ms = start.elapsed().as_millis() as u64,
                        "aprovação resolvida para a chamada"
                    )
                });
                if decision != ApprovalDecision::Approved {
                    let msg = match decision {
                        ApprovalDecision::Timeout => "Ação não aprovada pelo usuário (tempo esgotado). Não insista; informe o usuário.",
                        _ => "Ação não aprovada pelo usuário. Não insista; informe o usuário.",
                    };
                    return finish(
                        false,
                        Some(format!("{}:{via}", decision.as_str())),
                        CallToolResult::error(vec![ContentBlock::text(msg)]),
                    );
                }
            }
            ToolPolicy::Auto => {}
        }
        let ctx = ToolContext { thread_id, origin };
        match tool.call(ctx, args).await {
            Ok(out) => {
                let ok = !out.is_error;
                let err = if ok {
                    None
                } else {
                    Some(will_core::tools::truncate(&out.text, 200))
                };
                finish(ok, err, Self::tool_output(out))
            }
            Err(e) => {
                let msg = match &e {
                    ToolError::Denied => "Ação não aprovada pelo usuário.".to_string(),
                    other => other.to_string(),
                };
                finish(
                    false,
                    Some(msg.clone()),
                    CallToolResult::error(vec![ContentBlock::text(msg)]),
                )
            }
        }
    }
}

impl ServerHandler for WillMcpServer {
    fn get_info(&self) -> InitializeResult {
        let mut r = InitializeResult::default();
        r.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .build();
        r.server_info = Implementation::new("will_ia", env!("CARGO_PKG_VERSION"));
        r.instructions = Some(
            "Ferramentas do serviço will-ia (Telegram, WhatsApp pessoal, Google Agenda, Gmail, notificações). \
Algumas exigem aprovação do usuário antes de executar; se negadas, informe e não insista. \
Nunca envie mensagens, e-mails ou altere a agenda sem pedido explícito do usuário."
                .into(),
        );
        r
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools: Vec<Tool> = self
            .inner
            .registry
            .active()
            .iter()
            .map(|(spec, _)| Self::to_rmcp_tool(spec))
            .collect();
        let caller = Self::caller(&ctx);
        tracing::debug!(
            thread_id = caller
                .as_ref()
                .and_then(|c| c.thread_id)
                .map(|t| t.to_string())
                .unwrap_or_default(),
            tools = tools.len(),
            "tools/list atendido"
        );
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let caller = Self::caller(&ctx);
        let args = request
            .arguments
            .map(serde_json::Value::Object)
            .unwrap_or(serde_json::Value::Object(Default::default()));
        let result = self.run_call(&request.name, args, caller).await;
        Ok(result.into())
    }
}

#[derive(Clone)]
struct AuthState {
    tokens: Arc<TokenRegistry>,
}

async fn auth_middleware(State(st): State<AuthState>, mut req: Request, next: Next) -> Response {
    let token = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(|s| s.trim().to_string());
    let Some(token) = token else {
        tracing::warn!(path = %req.uri().path(), "requisição MCP sem bearer");
        return (StatusCode::UNAUTHORIZED, "missing bearer").into_response();
    };
    match st.tokens.lookup(&token) {
        Some(caller) => {
            req.extensions_mut().insert(caller);
            next.run(req).await
        }
        None => {
            tracing::warn!(path = %req.uri().path(), token_len = token.len(), "requisição MCP com bearer inválido/revogado");
            (StatusCode::UNAUTHORIZED, "invalid bearer").into_response()
        }
    }
}

pub struct McpServer {
    pub addr: SocketAddr,
    cancel: CancellationToken,
}

impl McpServer {
    /// Sobe em 127.0.0.1:<port> (0 = aleatória). Retorna o endereço real.
    pub async fn start(deps: McpServerDeps, port: u16) -> Result<Self, String> {
        let cancel = CancellationToken::new();
        let handler = WillMcpServer {
            inner: Arc::new(deps),
        };
        let tokens = handler.inner.tokens.clone();
        let config =
            StreamableHttpServerConfig::default().with_cancellation_token(cancel.child_token());
        let h2 = handler.clone();
        let service = StreamableHttpService::new(
            move || Ok(h2.clone()),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        let app = Router::new().nest_service("/mcp", service).layer(
            axum::middleware::from_fn_with_state(AuthState { tokens }, auth_middleware),
        );
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| {
                tracing::error!(port, error = %e, "bind do servidor MCP falhou");
                format!("bind MCP: {e}")
            })?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let c2 = cancel.clone();
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app)
                .with_graceful_shutdown(async move { c2.cancelled().await })
                .await
            {
                tracing::error!(error = %e, "servidor MCP encerrou com erro");
            } else {
                tracing::info!("servidor MCP encerrado");
            }
        });
        tracing::info!(%addr, module = "mcp", status = "running", "servidor MCP will_ia no ar");
        Ok(Self { addr, cancel })
    }

    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    pub fn stop(&self) {
        tracing::info!(addr = %self.addr, module = "mcp", status = "stopped", "parando servidor MCP");
        self.cancel.cancel();
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
