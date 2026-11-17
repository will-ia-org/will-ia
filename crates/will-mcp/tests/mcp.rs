use async_trait::async_trait;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use std::sync::Arc;
use std::time::Duration;
use will_core::{
    ApprovalDecision, ApprovalManager, Event, EventBus, Origin, ThreadId, ToolContext, ToolError,
    ToolOutput, ToolPolicy, ToolProvider, ToolSpec, WillTool,
};
use will_mcp::{CallRecord, McpServer, McpServerDeps, TokenRegistry, ToolRegistry};

struct Echo;
#[async_trait]
impl WillTool for Echo {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "echo".into(),
            description: "eco".into(),
            input_schema: serde_json::json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
            requires: vec![],
            default_policy: ToolPolicy::Auto,
            family: "test".into(),
        }
    }
    async fn call(
        &self,
        ctx: ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::json(
            &serde_json::json!({"echo": args["text"], "thread": ctx.thread_id}),
        ))
    }
}
struct Dangerous;
#[async_trait]
impl WillTool for Dangerous {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "dangerous".into(),
            description: "perigosa".into(),
            input_schema: serde_json::json!({"type":"object","properties":{}}),
            requires: vec![],
            default_policy: ToolPolicy::Confirm,
            family: "test".into(),
        }
    }
    async fn call(
        &self,
        _ctx: ToolContext,
        _args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("feito"))
    }
}
/// Envio com destinatário: `auto_listed` por padrão, lista de e-mail e
/// bloqueio de duplicidade. Conta quantas vezes executou de verdade.
struct Send(Arc<std::sync::atomic::AtomicUsize>);
#[async_trait]
impl WillTool for Send {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "send".into(),
            description: "envia".into(),
            input_schema: serde_json::json!({"type":"object","properties":{"to":{"type":"string"},"text":{"type":"string"}}}),
            requires: vec![],
            default_policy: ToolPolicy::AutoListed,
            family: "test".into(),
        }
    }
    fn trust_channel(&self) -> Option<will_core::TrustChannel> {
        Some(will_core::TrustChannel::Email)
    }
    fn guards_duplicates(&self) -> bool {
        true
    }
    async fn recipients(&self, args: &serde_json::Value) -> Option<Vec<will_core::Recipient>> {
        let key = will_core::trust::email_key(args.get("to")?.as_str()?)?;
        Some(vec![will_core::Recipient::new(key.clone(), key)])
    }
    async fn call(
        &self,
        _ctx: ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        Ok(ToolOutput::json(
            &serde_json::json!({"sent": n, "to": args["to"]}),
        ))
    }
}

struct P(Arc<std::sync::atomic::AtomicUsize>);
impl ToolProvider for P {
    fn family(&self) -> &'static str {
        "test"
    }
    fn tools(&self) -> Vec<Arc<dyn WillTool>> {
        vec![
            Arc::new(Echo),
            Arc::new(Dangerous),
            Arc::new(Send(self.0.clone())),
        ]
    }
    fn available(&self) -> bool {
        true
    }
}

async fn setup() -> (
    McpServer,
    Arc<TokenRegistry>,
    ApprovalManager,
    EventBus,
    Arc<parking_lot::Mutex<Vec<CallRecord>>>,
) {
    let (srv, tokens, approvals, bus, calls, _sent) =
        setup_with(will_core::TrustedRecipients::default(), Duration::ZERO).await;
    (srv, tokens, approvals, bus, calls)
}

/// Como [`setup`], com lista de confiança e janela de duplicidade; devolve
/// também o contador de envios reais da ferramenta `send`.
async fn setup_with(
    trusted: will_core::TrustedRecipients,
    duplicate_window: Duration,
) -> (
    McpServer,
    Arc<TokenRegistry>,
    ApprovalManager,
    EventBus,
    Arc<parking_lot::Mutex<Vec<CallRecord>>>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    let bus = EventBus::default();
    let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let registry = Arc::new(ToolRegistry::new());
    registry.add_provider(Arc::new(P(sent.clone())));
    let tokens = Arc::new(TokenRegistry::new());
    let approvals = ApprovalManager::new(bus.clone());
    let calls = Arc::new(parking_lot::Mutex::new(vec![]));
    let c2 = calls.clone();
    let srv = McpServer::start(
        McpServerDeps {
            registry,
            tokens: tokens.clone(),
            approvals: approvals.clone(),
            bus: bus.clone(),
            approval_timeout: Duration::from_secs(2),
            on_call: Arc::new(move |r| c2.lock().push(r)),
            trusted: Arc::new(move || trusted.clone()),
            duplicate_window,
            recent_sends: Default::default(),
        },
        0,
    )
    .await
    .unwrap();
    (srv, tokens, approvals, bus, calls, sent)
}

fn send_args(to: &str, text: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m = serde_json::Map::new();
    m.insert("to".into(), serde_json::json!(to));
    m.insert("text".into(), serde_json::json!(text));
    m
}

fn text_of(r: &rmcp::model::CallToolResult) -> String {
    serde_json::to_string(&r.content).unwrap_or_default()
}

#[tokio::test]
async fn auto_listed_runs_alone_only_for_trusted_and_blocks_duplicates() {
    use std::sync::atomic::Ordering::SeqCst;
    let mut trusted = will_core::TrustedRecipients::default();
    trusted.add(
        will_core::TrustChannel::Email,
        will_core::Recipient::new("@ok.com", ""),
    );
    let (srv, tokens, approvals, _bus, calls, sent) =
        setup_with(trusted, Duration::from_secs(60)).await;
    let tok = tokens.issue_generic();
    let c = client(&srv.url(), &tok).await;

    // destinatário listado: executa sem pedir aprovação
    let r = c
        .call_tool(
            CallToolRequestParams::new("send").with_arguments(send_args("Ana <ana@ok.com>", "oi")),
        )
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true));
    assert_eq!(sent.load(SeqCst), 1);
    assert!(approvals.pending().is_empty());

    // a mesma chamada de novo: não envia outra vez e avisa a duplicidade
    let r = c
        .call_tool(
            CallToolRequestParams::new("send").with_arguments(send_args("Ana <ana@ok.com>", "oi")),
        )
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true));
    assert_eq!(sent.load(SeqCst), 1);
    assert!(text_of(&r).contains("duplicada"), "{}", text_of(&r));

    // texto diferente é outro envio
    let r = c
        .call_tool(
            CallToolRequestParams::new("send").with_arguments(send_args("ana@ok.com", "outra")),
        )
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true));
    assert_eq!(sent.load(SeqCst), 2);
    let _ = c.cancel().await;

    let policies: Vec<&str> = calls.lock().iter().map(|r| r.policy).collect();
    assert_eq!(policies, vec!["auto_listed", "duplicate", "auto_listed"]);
}

#[tokio::test]
async fn auto_listed_confirms_unlisted_and_offers_trust() {
    use std::sync::atomic::Ordering::SeqCst;
    let (srv, tokens, approvals, bus, calls, sent) = setup_with(
        will_core::TrustedRecipients::default(),
        Duration::from_secs(60),
    )
    .await;
    let tok = tokens.issue_generic();
    let c = client(&srv.url(), &tok).await;

    // fora da lista: pede aprovação, com a oferta de confiar; negada, não envia
    let mut rx = bus.subscribe();
    let ap = approvals.clone();
    let decide = |decision: ApprovalDecision, mut rx: tokio::sync::broadcast::Receiver<Event>| {
        let ap = ap.clone();
        tokio::spawn(async move {
            loop {
                if let Ok(Event::ApprovalRequested { approval }) = rx.recv().await {
                    let offer = approval.trust.clone().expect("oferta de confiar");
                    assert_eq!(offer.channel, will_core::TrustChannel::Email);
                    assert_eq!(offer.recipients[0].key, "x@fora.com");
                    ap.decide(approval.id, decision, "test");
                    break;
                }
            }
        })
    };
    let h = decide(ApprovalDecision::Denied, rx);
    let r = c
        .call_tool(CallToolRequestParams::new("send").with_arguments(send_args("x@fora.com", "oi")))
        .await
        .unwrap();
    h.await.unwrap();
    assert_eq!(r.is_error, Some(true));
    assert_eq!(sent.load(SeqCst), 0);

    // negada não conta como enviada: a mesma chamada pede aprovação de novo
    rx = bus.subscribe();
    let h = decide(ApprovalDecision::Approved, rx);
    let r = c
        .call_tool(CallToolRequestParams::new("send").with_arguments(send_args("x@fora.com", "oi")))
        .await
        .unwrap();
    h.await.unwrap();
    assert_ne!(r.is_error, Some(true));
    assert_eq!(sent.load(SeqCst), 1);
    let _ = c.cancel().await;

    let policies: Vec<&str> = calls.lock().iter().map(|r| r.policy).collect();
    assert_eq!(policies, vec!["confirm", "confirm"]);
}

async fn client(url: &str, token: &str) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let cfg = StreamableHttpClientTransportConfig::with_uri(url.to_string())
        .auth_header(token.to_string());
    let transport = StreamableHttpClientTransport::with_client(reqwest::Client::new(), cfg);
    ().serve(transport).await.expect("initialize")
}

#[tokio::test]
async fn lists_and_calls_with_thread_identity() {
    let (srv, tokens, _ap, _bus, calls) = setup().await;
    let t = ThreadId::new();
    let tok = tokens.issue(t, Origin::Ui);
    let c = client(&srv.url(), &tok).await;
    let tools = c.list_all_tools().await.unwrap();
    let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    assert!(names.contains(&"echo".to_string()) && names.contains(&"dangerous".to_string()));
    let mut args = serde_json::Map::new();
    args.insert("text".into(), serde_json::json!("oi"));
    let r = c
        .call_tool(CallToolRequestParams::new("echo").with_arguments(args))
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true));
    let sc = r.structured_content.unwrap();
    assert_eq!(sc["echo"], "oi");
    assert_eq!(sc["thread"], serde_json::json!(t));
    let _ = c.cancel().await;
    assert_eq!(calls.lock().len(), 1);
    assert!(calls.lock()[0].ok);
}

#[tokio::test]
async fn rejects_without_bearer() {
    let (srv, _tokens, _ap, _bus, _calls) = setup().await;
    let resp = reqwest::Client::new()
        .post(srv.url())
        .header("content-type", "application/json")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn confirm_policy_waits_for_approval() {
    let (srv, tokens, approvals, bus, _calls) = setup().await;
    let tok = tokens.issue_generic();
    let c = client(&srv.url(), &tok).await;
    let mut rx = bus.subscribe();
    // aprova
    let ap2 = approvals.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(Event::ApprovalRequested { approval }) = rx.recv().await {
                assert_eq!(approval.tool, "dangerous");
                ap2.decide(approval.id, ApprovalDecision::Approved, "test");
                break;
            }
        }
    });
    let r = c
        .call_tool(CallToolRequestParams::new("dangerous"))
        .await
        .unwrap();
    assert_ne!(r.is_error, Some(true));
    let _ = c.cancel().await;
}

#[tokio::test]
async fn confirm_policy_denied_and_timeout() {
    let (srv, tokens, approvals, bus, _calls) = setup().await;
    let tok = tokens.issue_generic();
    let c = client(&srv.url(), &tok).await;
    let mut rx = bus.subscribe();
    let ap2 = approvals.clone();
    tokio::spawn(async move {
        loop {
            if let Ok(Event::ApprovalRequested { approval }) = rx.recv().await {
                ap2.decide(approval.id, ApprovalDecision::Denied, "test");
                break;
            }
        }
    });
    let r = c
        .call_tool(CallToolRequestParams::new("dangerous"))
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(true));
    // timeout (2 s) sem decisão
    let r = c
        .call_tool(CallToolRequestParams::new("dangerous"))
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(true));
    let _ = c.cancel().await;
}
