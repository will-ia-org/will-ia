//! Proxy CDP contra um upstream falso: reescrita de descoberta, repasse de
//! WebSocket com log de tráfego e erro claro quando não há upstream.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use futures::{SinkExt, StreamExt};
use std::sync::Arc;
use will_browser::{BrowserDeps, BrowserModule};
use will_core::settings::BrowserSettings;
use will_core::EventBus;

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
    assert_eq!(r.status(), 502);
    let body = r.text().await.unwrap();
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
    assert_eq!(r.status(), 502);
    let body = r.text().await.unwrap();
    assert!(body.contains("não consegui executar"), "{body}");
    m.stop().await;
}
