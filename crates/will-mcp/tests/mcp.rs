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
struct P;
impl ToolProvider for P {
    fn family(&self) -> &'static str {
        "test"
    }
    fn tools(&self) -> Vec<Arc<dyn WillTool>> {
        vec![Arc::new(Echo), Arc::new(Dangerous)]
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
    let bus = EventBus::default();
    let registry = Arc::new(ToolRegistry::new());
    registry.add_provider(Arc::new(P));
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
        },
        0,
    )
    .await
    .unwrap();
    (srv, tokens, approvals, bus, calls)
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
