//! Ponta a ponta com Chrome e agent-browser reais (ignorado por padrão):
//! `cargo test -p will-browser --test real -- --ignored`.

use will_browser::{BrowserDeps, BrowserModule};
use will_core::settings::BrowserSettings;
use will_core::EventBus;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
#[ignore]
async fn launches_real_browser_and_agent_browser_talks_through_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = BrowserSettings {
        listen_port: free_port(),
        upstream_port: free_port(),
        headless: true,
        user_data_dir: Some(dir.path().join("profile").to_string_lossy().to_string()),
        launch_wait_secs: 30,
        ..Default::default()
    };
    let m = BrowserModule::new(
        BrowserDeps {
            bus: EventBus::new(64),
            data_dir: dir.path().to_path_buf(),
            kb_path: parking_lot::RwLock::new(None),
        },
        cfg,
    );
    m.start().await;
    let detected = m.detect().await;
    eprintln!("navegadores: {detected:?}");
    assert!(!detected.is_empty(), "nenhum navegador detectado");

    // 1) primeiro pedido pelo proxy: sobe o Chrome e responde
    let listen = m.config().listen_port;
    let t0 = std::time::Instant::now();
    let v: serde_json::Value = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{listen}/json/version"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    eprintln!("/json/version via proxy em {:?}: {v}", t0.elapsed());
    assert!(v["webSocketDebuggerUrl"]
        .as_str()
        .unwrap()
        .starts_with(&format!("ws://127.0.0.1:{listen}/")));
    let st = m.status().await;
    assert!(st.launched.is_some() && st.upstream_up);

    // 2) aba nova pelo proxy
    let tab = m.test_new_tab("about:blank").await.unwrap();
    eprintln!("aba: {tab}");

    // 3) agent-browser de ponta a ponta (se instalado)
    if will_browser::detect::find_node_tool("agent-browser").is_some() {
        let out = m.test_agent_browser().await;
        eprintln!("agent-browser: {out:?}");
        assert!(out.is_ok());
        let entries = m.traffic.since(0, 2000);
        eprintln!("mensagens CDP registradas: {}", entries.len());
        assert!(entries.iter().any(|e| e.kind == "command"));
        assert!(entries.iter().any(|e| e.kind == "response"));
    } else {
        eprintln!("agent-browser não instalado; pulando etapa 3");
    }

    m.close_browser().await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    assert!(m
        .probe_upstream(std::time::Duration::from_millis(800))
        .await
        .is_err());
    m.stop().await;
}
