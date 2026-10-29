//! Proxy CDP contra um upstream falso: reescrita de descoberta, repasse de
//! WebSocket com log de tráfego, erro claro (JSON 503) quando não há upstream,
//! descoberta por `DevToolsActivePort` quando o `/json/*` é 404 e recusa de
//! lançar um segundo navegador em porta ocupada.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use will_browser::{BrowserDeps, BrowserModule};
use will_core::settings::BrowserSettings;
use will_core::EventBus;

/// Upstream que imita um Chrome **sem** `--remote-allow-origins`: `/json/*` → 404,
/// mas o WebSocket CDP em `/devtools/browser/abc` (e `/devtools/page/T1`) funciona.
async fn fake_upstream_without_http_discovery() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route(
            "/json/version",
            get(|| async { (axum::http::StatusCode::NOT_FOUND, "") }),
        )
        .route(
            "/devtools/browser/abc",
            get(|ws: WebSocketUpgrade| async move { ws.on_upgrade(cdp_fake).into_response() }),
        )
        .route(
            "/devtools/page/T1",
            get(|ws: WebSocketUpgrade| async move { ws.on_upgrade(echo).into_response() }),
        );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    port
}

/// Responde `Browser.getVersion`, `Target.getTargets` e `Target.createTarget` como um Chrome; o resto ecoa.
async fn cdp_fake(mut socket: WebSocket) {
    while let Some(Ok(Message::Text(t))) = socket.next().await {
        let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
        let id = v["id"].as_i64().unwrap();
        let result = match v["method"].as_str().unwrap() {
            "Browser.getVersion" => {
                serde_json::json!({"product": "FakeChrome/2", "protocolVersion": "1.3", "userAgent": "ua", "jsVersion": "v8", "revision": "r"})
            }
            "Target.getTargets" => serde_json::json!({"targetInfos": [
                {"targetId": "T1", "type": "page", "title": "Aba", "url": "about:blank"},
                {"targetId": "B0", "type": "browser", "title": "", "url": ""}]}),
            "Target.createTarget" => serde_json::json!({"targetId": "T2"}),
            m => serde_json::json!({"echo": m}),
        };
        let _ = socket
            .send(Message::Text(
                serde_json::json!({"id": id, "result": result})
                    .to_string()
                    .into(),
            ))
            .await;
    }
}

async fn fake_upstream() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = Router::new()
        .route(
            "/json/version",
            get(move || async move {
                axum::Json(serde_json::json!({
                    "Browser": "FakeChrome/1",
                    "webSocketDebuggerUrl": format!("ws://127.0.0.1:{port}/devtools/browser/abc")
                }))
            }),
        )
        .route(
            "/devtools/browser/abc",
            get(|ws: WebSocketUpgrade| async move { ws.on_upgrade(echo).into_response() }),
        );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    port
}

/// Responde a cada comando `{id, method}` com `{id, result:{method}}` e emite um evento.
async fn echo(mut socket: WebSocket) {
    while let Some(Ok(Message::Text(t))) = socket.next().await {
        let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
        let id = v["id"].as_i64().unwrap();
        let method = v["method"].as_str().unwrap().to_string();
        let _ = socket
            .send(Message::Text(
                serde_json::json!({"id": id, "result": {"echo": method}})
                    .to_string()
                    .into(),
            ))
            .await;
        let _ = socket
            .send(Message::Text(
                serde_json::json!({"method": "Fake.event", "params": {}})
                    .to_string()
                    .into(),
            ))
            .await;
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn module(upstream_port: u16, auto_launch: bool) -> Arc<BrowserModule> {
    let dir = tempfile::tempdir().unwrap();
    let cfg = BrowserSettings {
        listen_port: free_port(),
        upstream_port,
        auto_launch,
        attach_existing: false,
        launch_wait_secs: 3,
        ..Default::default()
    };
    let m = BrowserModule::new(
        BrowserDeps {
            bus: EventBus::new(64),
            data_dir: dir.keep(),
            kb_path: parking_lot::RwLock::new(None),
        },
        cfg,
    );
    m.start().await;
    m
}

#[tokio::test]
async fn attaches_via_devtools_active_port_when_http_is_404() {
    let up = fake_upstream_without_http_discovery().await;
    let m = module(up, false).await;
    // perfil dedicado com DevToolsActivePort apontando pro upstream falso
    std::fs::create_dir_all(m.user_data_dir()).unwrap();
    std::fs::write(
        m.user_data_dir().join("DevToolsActivePort"),
        format!("{up}\n/devtools/browser/abc"),
    )
    .unwrap();
    let listen = m.config().listen_port;
    let client = reqwest::Client::new();
    let r = client
        .get(format!("http://127.0.0.1:{listen}/json/version"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["Browser"], "FakeChrome/2");
    assert_eq!(
        v["webSocketDebuggerUrl"],
        format!("ws://127.0.0.1:{listen}/devtools/browser/abc")
    );
    let list: serde_json::Value = client
        .get(format!("http://127.0.0.1:{listen}/json/list"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    assert_eq!(
        list[0]["webSocketDebuggerUrl"],
        format!("ws://127.0.0.1:{listen}/devtools/page/T1")
    );
    let novo: serde_json::Value = client
        .put(format!("http://127.0.0.1:{listen}/json/new?about:blank"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(novo["id"], "T2");
    // WebSocket de página passa pelo proxy normalmente
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{listen}/devtools/page/T1"))
            .await
            .unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        r#"{"id":1,"method":"Page.enable"}"#.into(),
    ))
    .await
    .unwrap();
    let r1 = ws.next().await.unwrap().unwrap().into_text().unwrap();
    assert!(r1.as_str().contains("Page.enable"));
    ws.close(None).await.unwrap();
    let st = m.status().await;
    assert!(st.upstream_up && !st.upstream_http_discovery, "{st:?}");
    assert!(
        st.upstream_source
            .as_deref()
            .unwrap_or("")
            .starts_with("DevToolsActivePort"),
        "{st:?}"
    );
    assert_eq!(st.upstream_browser.as_deref(), Some("FakeChrome/2"));
    m.stop().await;
}

#[tokio::test]
async fn busy_port_without_cdp_discovery_does_not_launch() {
    let up = fake_upstream_without_http_discovery().await;
    let m = module(up, true).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = m.config();
    cfg.browser_path = Some(dir.path().join("nao-existe").to_string_lossy().to_string());
    m.update_config(cfg).await;
    let listen = m.config().listen_port;
    let r = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{listen}/json/version"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    let v: serde_json::Value = r.json().await.unwrap();
    let err = v["error"].as_str().unwrap();
    assert!(err.contains("ocupada"), "{err}");
    assert!(!err.contains("não consegui executar"), "{err}");
    assert!(m.status().await.launched.is_none());
    m.stop().await;
}

#[tokio::test]
async fn rewrites_discovery_and_pumps_websocket() {
    let up = fake_upstream().await;
    let m = module(up, false).await;
    let listen = m.config().listen_port;
    let client = reqwest::Client::new();
    let v: serde_json::Value = client
        .get(format!("http://127.0.0.1:{listen}/json/version"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ws_url = v["webSocketDebuggerUrl"].as_str().unwrap().to_string();
    assert_eq!(
        ws_url,
        format!("ws://127.0.0.1:{listen}/devtools/browser/abc")
    );

    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::Text(
        r#"{"id":7,"method":"Target.getTargets"}"#.into(),
    ))
    .await
    .unwrap();
    let r1 = ws.next().await.unwrap().unwrap().into_text().unwrap();
    assert!(r1.as_str().contains("Target.getTargets"));
    let r2 = ws.next().await.unwrap().unwrap().into_text().unwrap();
    assert!(r2.as_str().contains("Fake.event"));
    ws.close(None).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let entries = m.traffic.since(0, 100);
    assert_eq!(entries.len(), 3, "{entries:?}");
    assert_eq!(entries[0].kind, "command");
    assert_eq!(entries[1].kind, "response");
    assert_eq!(entries[1].method.as_deref(), Some("Target.getTargets"));
    assert_eq!(entries[2].kind, "event");
    let st = m.status().await;
    assert!(st.listening && st.upstream_up);
    assert_eq!(st.upstream_browser.as_deref(), Some("FakeChrome/1"));
    assert_eq!(st.connections, 1);
    m.stop().await;
}

#[tokio::test]
async fn no_upstream_without_auto_launch_is_a_clear_502() {
    let m = module(free_port(), false).await;
    let listen = m.config().listen_port;
    let r = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{listen}/json/version"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    let v: serde_json::Value = r.json().await.unwrap();
    let body = v["error"].as_str().unwrap().to_string();
    assert!(body.contains("lançamento automático desligado"), "{body}");
    assert!(m.status().await.last_error.is_some());
    m.stop().await;
}

#[tokio::test]
async fn auto_launch_with_fake_browser_script() {
    if cfg!(windows) {
        return;
    }
    // "navegador" falso: um script que sobe o upstream falso na porta pedida
    // não é trivial sem Node; aqui validamos só o caminho de erro de binário ausente.
    let m = module(free_port(), true).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = m.config();
    cfg.browser_path = Some(dir.path().join("nao-existe").to_string_lossy().to_string());
    cfg.launch_wait_secs = 3;
    m.update_config(cfg).await;
    let listen = m.config().listen_port;
    let r = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{listen}/json/version"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    let v: serde_json::Value = r.json().await.unwrap();
    let body = v["error"].as_str().unwrap().to_string();
    assert!(body.contains("não consegui executar"), "{body}");
    m.stop().await;
}
