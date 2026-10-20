//! Testes de integração do motor com o binário `fake-claude`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use will_core::{ThreadId, TurnId};
use will_engine::{EngineConfig, EngineEvent, EnginePool, PermissionMode, SendRequest};

fn fake_claude() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let exe = if cfg!(windows) {
        "fake-claude.exe"
    } else {
        "fake-claude"
    };
    let candidates = [root.join("target/debug").join(exe)];
    for c in &candidates {
        if c.exists() {
            return c.clone();
        }
    }
    let st = std::process::Command::new("cargo")
        .args(["build", "-p", "fake-claude"])
        .current_dir(&root)
        .status()
        .expect("cargo build fake-claude");
    assert!(st.success());
    candidates[0].clone()
}

fn cfg(dir: &std::path::Path, extra_env: &[(&str, &str)]) -> EngineConfig {
    let mut env = BTreeMap::new();
    if let Ok(p) = std::env::var("PATH") {
        env.insert("PATH".to_string(), p);
    }
    for (k, v) in extra_env {
        env.insert(k.to_string(), v.to_string());
    }
    EngineConfig {
        claude_bin: fake_claude(),
        cwd: dir.to_path_buf(),
        env,
        permission_mode: PermissionMode::AcceptEdits,
        allowed_tools: vec!["Read".into()],
        mcp_config_path: None,
        strict_mcp_config: true,
        model: None,
        max_budget_usd: None,
        effort: None,
        include_partial_messages: false,
        append_system_prompt: "x".into(),
        max_live: 2,
        idle_timeout: Duration::from_secs(600),
        kill_grace: Duration::from_millis(500),
        raw_log_dir: None,
    }
}

async fn wait_for<F: FnMut(&EngineEvent) -> bool>(
    rx: &mut tokio::sync::broadcast::Receiver<EngineEvent>,
    mut f: F,
    secs: u64,
) -> EngineEvent {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let ev = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("timeout esperando evento")
            .expect("canal");
        if f(&ev) {
            return ev;
        }
    }
}

#[tokio::test]
async fn basic_turn_and_session() {
    let dir = tempfile::tempdir().unwrap();
    let pool = EnginePool::new(cfg(dir.path(), &[]));
    let mut rx = pool.subscribe();
    let t = ThreadId::new();
    let turn = TurnId::new();
    pool.send(
        t,
        None,
        Default::default(),
        SendRequest {
            turn_id: turn,
            text: "olá".into(),
        },
    )
    .unwrap();
    let sess = wait_for(&mut rx, |e| matches!(e, EngineEvent::Session { .. }), 10).await;
    let EngineEvent::Session {
        session_id,
        resumed,
        ..
    } = sess
    else {
        unreachable!()
    };
    assert!(!session_id.is_empty());
    assert!(!resumed);
    let fin = wait_for(
        &mut rx,
        |e| matches!(e, EngineEvent::TurnFinished { .. }),
        10,
    )
    .await;
    let EngineEvent::TurnFinished {
        turn_id,
        result,
        merged,
        ..
    } = fin
    else {
        unreachable!()
    };
    assert_eq!(turn_id, turn);
    assert!(merged.is_empty());
    assert_eq!(result.text, "eco: olá");
    assert!(pool.is_alive(t));
    assert_eq!(pool.pending(t), 0);

    // segundo turno no mesmo processo
    let turn2 = TurnId::new();
    pool.send(
        t,
        Some(session_id.clone()),
        Default::default(),
        SendRequest {
            turn_id: turn2,
            text: "de novo".into(),
        },
    )
    .unwrap();
    let fin = wait_for(
        &mut rx,
        |e| matches!(e, EngineEvent::TurnFinished { turn_id, .. } if *turn_id == turn2),
        10,
    )
    .await;
    let EngineEvent::TurnFinished { result, .. } = fin else {
        unreachable!()
    };
    assert_eq!(result.text, "eco: de novo");
    pool.shutdown().await;
}

#[tokio::test]
async fn mid_turn_message_is_merged_into_running_turn() {
    let dir = tempfile::tempdir().unwrap();
    let pool = EnginePool::new(cfg(dir.path(), &[]));
    let mut rx = pool.subscribe();
    let t = ThreadId::new();
    let t1 = TurnId::new();
    let t2 = TurnId::new();
    pool.send(
        t,
        None,
        Default::default(),
        SendRequest {
            turn_id: t1,
            text: "sleep 2".into(),
        },
    )
    .unwrap();
    wait_for(
        &mut rx,
        |e| matches!(e, EngineEvent::TurnStarted { turn_id, .. } if *turn_id == t1),
        10,
    )
    .await;
    // mensagem no meio do turno: escrita na hora
    pool.send(
        t,
        None,
        Default::default(),
        SendRequest {
            turn_id: t2,
            text: "complemento".into(),
        },
    )
    .unwrap();
    let sent = wait_for(
        &mut rx,
        |e| matches!(e, EngineEvent::Sent { turn_id, .. } if *turn_id == t2),
        5,
    )
    .await;
    let EngineEvent::Sent { mid_turn, .. } = sent else {
        unreachable!()
    };
    assert!(mid_turn, "segunda mensagem deve ser marcada como mid-turn");
    // fake-claude processa em série: t1 fecha, depois t2 (ecoada só quando consumida) fecha.
    let f1 = wait_for(
        &mut rx,
        |e| matches!(e, EngineEvent::TurnFinished { .. }),
        10,
    )
    .await;
    let EngineEvent::TurnFinished {
        turn_id, merged, ..
    } = f1
    else {
        unreachable!()
    };
    assert_eq!(turn_id, t1);
    assert!(
        merged.is_empty(),
        "t2 ainda não tinha sido consumida pelo processo"
    );
    let f2 = wait_for(
        &mut rx,
        |e| matches!(e, EngineEvent::TurnFinished { .. }),
        10,
    )
    .await;
    let EngineEvent::TurnFinished {
        turn_id, result, ..
    } = f2
    else {
        unreachable!()
    };
    assert_eq!(turn_id, t2);
    assert_eq!(result.text, "eco: complemento");
    pool.shutdown().await;
}

#[tokio::test]
async fn crash_drops_pending_and_restart_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let pool = EnginePool::new(cfg(dir.path(), &[]));
    let mut rx = pool.subscribe();
    let t = ThreadId::new();
    let t1 = TurnId::new();
    pool.send(
        t,
        None,
        Default::default(),
        SendRequest {
            turn_id: t1,
            text: "crash".into(),
        },
    )
    .unwrap();
    let d = wait_for(&mut rx, |e| matches!(e, EngineEvent::Dropped { .. }), 10).await;
    let EngineEvent::Dropped { turn_id, .. } = d else {
        unreachable!()
    };
    assert_eq!(turn_id, t1);
    assert!(!pool.is_alive(t));
    // próxima mensagem respawna com --resume
    let t2 = TurnId::new();
    pool.send(
        t,
        Some("sess-x".into()),
        Default::default(),
        SendRequest {
            turn_id: t2,
            text: "oi".into(),
        },
    )
    .unwrap();
    let s = wait_for(&mut rx, |e| matches!(e, EngineEvent::Session { .. }), 10).await;
    let EngineEvent::Session { resumed, .. } = s else {
        unreachable!()
    };
    assert!(resumed);
    wait_for(
        &mut rx,
        |e| matches!(e, EngineEvent::TurnFinished { turn_id, .. } if *turn_id == t2),
        10,
    )
    .await;
    pool.shutdown().await;
}

#[tokio::test]
async fn stale_session_restarts_and_resends() {
    let dir = tempfile::tempdir().unwrap();
    let pool = EnginePool::new(cfg(dir.path(), &[("FAKE_CLAUDE_STALE", "1")]));
    let mut rx = pool.subscribe();
    let t = ThreadId::new();
    let t1 = TurnId::new();
    pool.send(
        t,
        Some("expired-session".into()),
        Default::default(),
        SendRequest {
            turn_id: t1,
            text: "oi".into(),
        },
    )
    .unwrap();
    wait_for(
        &mut rx,
        |e| matches!(e, EngineEvent::StaleSession { .. }),
        10,
    )
    .await;
    let fin = wait_for(
        &mut rx,
        |e| matches!(e, EngineEvent::TurnFinished { .. }),
        10,
    )
    .await;
    let EngineEvent::TurnFinished {
        turn_id, result, ..
    } = fin
    else {
        unreachable!()
    };
    assert_eq!(turn_id, t1);
    assert_eq!(result.text, "eco: oi");
    pool.shutdown().await;
}

#[tokio::test]
async fn max_live_retires_idle_thread() {
    let dir = tempfile::tempdir().unwrap();
    let pool = EnginePool::new(cfg(dir.path(), &[])); // max_live = 2
    let mut rx = pool.subscribe();
    let a = ThreadId::new();
    let b = ThreadId::new();
    let c = ThreadId::new();
    for t in [a, b] {
        let id = TurnId::new();
        pool.send(
            t,
            None,
            Default::default(),
            SendRequest {
                turn_id: id,
                text: "x".into(),
            },
        )
        .unwrap();
        wait_for(
            &mut rx,
            |e| matches!(e, EngineEvent::TurnFinished { turn_id, .. } if *turn_id == id),
            10,
        )
        .await;
    }
    assert!(pool.is_alive(a) && pool.is_alive(b));
    let id = TurnId::new();
    pool.send(
        c,
        None,
        Default::default(),
        SendRequest {
            turn_id: id,
            text: "x".into(),
        },
    )
    .unwrap();
    wait_for(&mut rx, |e| matches!(e, EngineEvent::Retired { .. }), 10).await;
    wait_for(
        &mut rx,
        |e| matches!(e, EngineEvent::TurnFinished { turn_id, .. } if *turn_id == id),
        10,
    )
    .await;
    let alive = pool.stats().iter().filter(|s| s.alive).count();
    assert!(alive <= 2, "no máximo 2 vivos, havia {alive}");
    pool.shutdown().await;
}

#[tokio::test]
async fn reconfigure_restarts_live_processes() {
    let dir = tempfile::tempdir().unwrap();
    let pool = EnginePool::new(cfg(dir.path(), &[]));
    let mut rx = pool.subscribe();
    let t = ThreadId::new();
    let id = TurnId::new();
    pool.send(
        t,
        None,
        Default::default(),
        SendRequest {
            turn_id: id,
            text: "x".into(),
        },
    )
    .unwrap();
    wait_for(
        &mut rx,
        |e| matches!(e, EngineEvent::TurnFinished { .. }),
        10,
    )
    .await;
    let n = pool.reconfigure(cfg(dir.path(), &[("NEW", "1")]));
    assert_eq!(n, 1);
    wait_for(&mut rx, |e| matches!(e, EngineEvent::Retired { .. }), 10).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!pool.is_alive(t));
    pool.shutdown().await;
}
