//! Sessão contra um servidor WebSocket mock: session.update (GA → fallback
//! legacy), handoff ask_brain → function_call_output → injeção do resultado,
//! áudio de saída, transcrições, áudio de entrada, reconexão e stop.

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;
use will_core::settings::RealtimeSettings;
use will_core::{BrainHost, BrainTask, Event, EventBus, ThreadId, TurnId, TurnResult};
use will_realtime::{AudioOut, RealtimeDeps, RealtimeManager};

struct FakeBrain {
    bus: EventBus,
    thread: ThreadId,
    prompts: Mutex<Vec<(String, bool)>>,
    last_turn: Mutex<Option<TurnId>>,
}

#[async_trait::async_trait]
impl BrainHost for FakeBrain {
    async fn ask_brain(
        &self,
        prompt: &str,
        new_thread: bool,
        _thread: Option<&str>,
        _title: Option<&str>,
    ) -> Result<BrainTask, String> {
        self.prompts.lock().push((prompt.to_string(), new_thread));
        let turn_id = TurnId::new();
        *self.last_turn.lock() = Some(turn_id);
        Ok(BrainTask {
            thread_id: self.thread,
            turn_id,
        })
    }
    async fn brain_threads(&self) -> Result<Vec<will_core::ThreadSummary>, String> {
        Ok(vec![])
    }
    async fn brain_busy(&self) -> bool {
        self.last_turn.lock().is_some()
    }
    fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.bus.subscribe()
    }
}

/// Mensagem recebida pelo servidor mock: (índice da conexão, JSON) ou None no fim da conexão.
type Inbound = (usize, Option<Value>);

/// Servidor mock: aceita conexões em sequência; repassa o que recebe para `inbound`
/// e envia ao cliente tudo que chega em `outbound`.
async fn mock_server(
    listener: TcpListener,
    inbound: mpsc::UnboundedSender<Inbound>,
    mut outbound: mpsc::UnboundedReceiver<Value>,
) {
    let mut conn_idx = 0usize;
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
            return;
        };
        let (mut sink, mut source) = ws.split();
        loop {
            tokio::select! {
                msg = source.next() => match msg {
                    Some(Ok(Message::Text(t))) => {
                        let v: Value = serde_json::from_str(t.as_str()).unwrap();
                        let _ = inbound.send((conn_idx, Some(v)));
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        let _ = inbound.send((conn_idx, None));
                        break;
                    }
                    Some(Ok(_)) => {}
                },
                out = outbound.recv() => match out {
                    Some(v) if v["type"] == "__close__" => {
                        let _ = sink.send(Message::Close(None)).await;
                        let _ = inbound.send((conn_idx, None));
                        break;
                    }
                    Some(v) => { sink.send(Message::text(v.to_string())).await.unwrap(); }
                    None => return,
                },
            }
        }
        conn_idx += 1;
    }
}

async fn next_msg(rx: &mut mpsc::UnboundedReceiver<Inbound>) -> Inbound {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout aguardando mensagem do cliente")
        .expect("canal fechado")
}

/// Próxima mensagem com o `type` dado (ignora appends de áudio etc.).
async fn expect_type(rx: &mut mpsc::UnboundedReceiver<Inbound>, kind: &str) -> Value {
    loop {
        let (_, msg) = next_msg(rx).await;
        let v = msg.unwrap_or_else(|| panic!("conexão fechou aguardando {kind}"));
        if v["type"] == kind {
            return v;
        }
    }
}

async fn wait_event(rx: &mut broadcast::Receiver<Event>, pred: impl Fn(&Event) -> bool) -> Event {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Ok(ev) if pred(&ev) => return ev,
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => panic!("bus fechado"),
            }
        }
    })
    .await
    .expect("timeout aguardando evento")
}

async fn wait_audio(
    rx: &mut broadcast::Receiver<AudioOut>,
    pred: impl Fn(&AudioOut) -> bool,
) -> AudioOut {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await {
                Ok(a) if pred(&a) => return a,
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => panic!("audio_out fechado"),
            }
        }
    })
    .await
    .expect("timeout aguardando audio_out")
}

#[tokio::test]
async fn handoff_flow_against_mock_server() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Inbound>();
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(mock_server(listener, in_tx, out_rx));

    let bus = EventBus::new(256);
    let brain = Arc::new(FakeBrain {
        bus: bus.clone(),
        thread: ThreadId::new(),
        prompts: Mutex::new(vec![]),
        last_turn: Mutex::new(None),
    });
    let storage = will_storage::Storage::open_memory().await.unwrap();
    let deps = RealtimeDeps {
        host: brain.clone(),
        bus: bus.clone(),
        storage: storage.clone(),
    };
    let settings = RealtimeSettings {
        enabled: true,
        model: "gpt-realtime".into(),
        brain_speech_limit_chars: 40,
        // o passo 8 testa o barge-in (interromper falando por cima)
        barge_in: true,
        ..Default::default()
    };

    // Sem chave → erro antes de conectar.
    let no_key = RealtimeManager::new(deps.clone(), settings.clone(), None);
    assert!(no_key.start().await.is_err());

    let mgr = RealtimeManager::new(deps, settings, Some(secrecy::SecretString::from("sk-test")));
    mgr.set_endpoint(format!("ws://127.0.0.1:{port}/v1/realtime"));
    let mut events = bus.subscribe();
    let mut audio_rx = mgr.subscribe_audio_out();

    mgr.start().await.expect("start");
    assert!(mgr.is_active());

    // 1) session.update GA primeiro (modelo gpt-realtime).
    let su = expect_type(&mut in_rx, "session.update").await;
    assert_eq!(su["session"]["type"], "realtime");
    assert_eq!(
        su["session"]["audio"]["input"]["format"]["type"],
        "audio/pcm"
    );
    out_tx
        .send(json!({"type": "session.created", "session": {}}))
        .unwrap();

    // 2) Servidor rejeita → fallback para legacy.
    out_tx.send(json!({"type": "error", "error": {"type": "invalid_request_error", "code": "unknown_parameter", "message": "Unknown parameter: 'session.audio'.", "param": "session.audio"}})).unwrap();
    let su2 = expect_type(&mut in_rx, "session.update").await;
    assert_eq!(su2["session"]["modalities"], json!(["audio", "text"]));
    assert_eq!(su2["session"]["input_audio_format"], "pcm16");
    out_tx
        .send(json!({"type": "session.updated", "session": {}}))
        .unwrap();
    wait_event(
        &mut events,
        |e| matches!(e, Event::Voice { state, .. } if state == "listening"),
    )
    .await;

    // 3) Áudio de entrada → input_audio_buffer.append em chunks de 4800 bytes.
    let pcm: Vec<u8> = (0..9600u32).map(|i| (i % 251) as u8).collect();
    mgr.audio_in(Bytes::from(pcm.clone()));
    let a1 = expect_type(&mut in_rx, "input_audio_buffer.append").await;
    let a2 = expect_type(&mut in_rx, "input_audio_buffer.append").await;
    let d1 = will_realtime::audio::decode_base64(a1["audio"].as_str().unwrap()).unwrap();
    let d2 = will_realtime::audio::decode_base64(a2["audio"].as_str().unwrap()).unwrap();
    assert_eq!(d1.len(), 4800);
    assert_eq!([d1, d2].concat(), pcm);
    mgr.commit();
    expect_type(&mut in_rx, "input_audio_buffer.commit").await;
    expect_type(&mut in_rx, "response.create").await;

    // 4) Function call ask_brain → output "working" + response.create.
    out_tx
        .send(json!({"type": "response.created", "response": {}}))
        .unwrap();
    out_tx
        .send(json!({
            "type": "response.function_call_arguments.done",
            "name": "ask_brain", "call_id": "call_1",
            "arguments": "{\"prompt\":\"quais são meus compromissos?\",\"new_thread\":true}"
        }))
        .unwrap();
    let fco = expect_type(&mut in_rx, "conversation.item.create").await;
    assert_eq!(fco["item"]["type"], "function_call_output");
    assert_eq!(fco["item"]["call_id"], "call_1");
    let output: Value = serde_json::from_str(fco["item"]["output"].as_str().unwrap()).unwrap();
    assert_eq!(output["status"], "working");
    let turn_id = brain.last_turn.lock().expect("ask_brain chamado");
    assert_eq!(output["task_id"], turn_id.to_string());
    assert_eq!(
        brain.prompts.lock()[0],
        ("quais são meus compromissos?".to_string(), true)
    );
    expect_type(&mut in_rx, "response.create").await;
    wait_event(
        &mut events,
        |e| matches!(e, Event::Voice { state, .. } if state == "brain_working"),
    )
    .await;
    assert_eq!(mgr.status()["pending_tasks"].as_array().unwrap().len(), 1);

    // 5) brain_status.
    out_tx.send(json!({"type": "response.function_call_arguments.done", "name": "brain_status", "call_id": "call_2", "arguments": "{}"})).unwrap();
    let st = expect_type(&mut in_rx, "conversation.item.create").await;
    let output: Value = serde_json::from_str(st["item"]["output"].as_str().unwrap()).unwrap();
    assert_eq!(output["busy"], true);
    expect_type(&mut in_rx, "response.create").await;

    // 6) Cérebro termina → texto limpo injetado como item do usuário.
    bus.emit(Event::TurnFinished {
        thread_id: brain.thread,
        turn_id,
        result: TurnResult {
            text: "Você tem **duas** reuniões:\n```\nignored\n```\n- 10h com a Ana\n- 15h dentista"
                .into(),
            is_error: false,
            cost_usd: None,
            duration_ms: None,
            model: None,
            session_id: None,
            usage: None,
        },
    });
    let item = expect_type(&mut in_rx, "conversation.item.create").await;
    assert_eq!(item["item"]["type"], "message");
    assert_eq!(item["item"]["role"], "user");
    let text = item["item"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with(&format!("[Resultado do cérebro para a tarefa {turn_id}]: ")),
        "{text}"
    );
    assert!(
        text.contains("Você tem duas reuniões: 10h com a Ana"),
        "{text}"
    );
    assert!(!text.contains("```") && !text.contains("ignored") && !text.contains("**"));
    assert!(text.ends_with('…'), "deve truncar em 40 chars: {text}");
    expect_type(&mut in_rx, "response.create").await;
    assert_eq!(mgr.status()["pending_tasks"].as_array().unwrap().len(), 0);

    // 7) Áudio de saída (nome GA) → AudioOut::Pcm; transcrição → storage.
    let pcm_out = vec![1u8, 2, 3, 4, 5, 6];
    out_tx.send(json!({"type": "response.output_audio.delta", "item_id": "item_a", "delta": will_realtime::audio::encode_base64(&pcm_out)})).unwrap();
    let got = wait_audio(&mut audio_rx, |a| matches!(a, AudioOut::Pcm(_))).await;
    match got {
        AudioOut::Pcm(b) => assert_eq!(b.as_ref(), &pcm_out[..]),
        _ => unreachable!(),
    }
    wait_event(
        &mut events,
        |e| matches!(e, Event::Voice { state, .. } if state == "speaking"),
    )
    .await;
    out_tx.send(json!({"type": "response.output_audio_transcript.done", "transcript": "Você tem duas reuniões hoje."})).unwrap();
    out_tx.send(json!({"type": "conversation.item.input_audio_transcription.completed", "transcript": "quais são meus compromissos"})).unwrap();
    let ctrl = wait_audio(
        &mut audio_rx,
        |a| matches!(a, AudioOut::Control(v) if v["type"] == "transcript" && v["role"] == "user"),
    )
    .await;
    assert!(matches!(ctrl, AudioOut::Control(_)));
    let transcripts = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let t = storage.list_voice_transcripts(10).await.unwrap();
            if t.len() >= 2 {
                return t;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let roles: Vec<&str> = transcripts.iter().map(|t| t.role.as_str()).collect();
    assert!(
        roles.contains(&"user") && roles.contains(&"assistant"),
        "{roles:?}"
    );

    // 8) Barge-in: speech_started enquanto toca → stop_playback + response.cancel + truncate.
    out_tx
        .send(json!({"type": "input_audio_buffer.speech_started"}))
        .unwrap();
    wait_audio(
        &mut audio_rx,
        |a| matches!(a, AudioOut::Control(v) if v["type"] == "stop_playback"),
    )
    .await;
    expect_type(&mut in_rx, "response.cancel").await;
    let tr = expect_type(&mut in_rx, "conversation.item.truncate").await;
    assert_eq!(tr["item_id"], "item_a");
    assert_eq!(tr["content_index"], 0);
    assert!(tr["audio_end_ms"].as_u64().is_some());
    out_tx
        .send(json!({"type": "response.done", "response": {"status": "cancelled"}}))
        .unwrap();
    wait_event(
        &mut events,
        |e| matches!(e, Event::Voice { state, .. } if state == "listening"),
    )
    .await;

    // 9) Servidor derruba a conexão → reconecta (1 s) e reenvia session.update.
    out_tx.send(json!({"type": "__close__"})).unwrap();
    loop {
        let (idx, msg) = next_msg(&mut in_rx).await;
        if idx == 1 {
            let v = msg.expect("segunda conexão fechou cedo");
            assert_eq!(v["type"], "session.update");
            // Variante legacy mantida após o fallback.
            assert_eq!(v["session"]["input_audio_format"], "pcm16");
            break;
        }
    }
    assert!(mgr.is_active());

    // 10) stop → fecha, estado stopped.
    mgr.stop().await;
    assert!(!mgr.is_active());
    assert_eq!(mgr.status()["active"], false);
    assert_eq!(mgr.status()["state"], "stopped");

    // 11) Resultado do cérebro com a sessão fechada → Notification.
    let (_, turn2) = {
        let t = brain.ask_brain("x", false, None, None).await.unwrap();
        (t.thread_id, t.turn_id)
    };
    // Pendência registrada manualmente não existe mais; simulamos nova delegação via sessão? Não:
    // o manager só rastreia tarefas iniciadas pela voz. Verificamos que eventos alheios são ignorados.
    bus.emit(Event::TurnFinished {
        thread_id: brain.thread,
        turn_id: turn2,
        result: TurnResult {
            text: "alheio".into(),
            is_error: false,
            cost_usd: None,
            duration_ms: None,
            model: None,
            session_id: None,
            usage: None,
        },
    });
    let res = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            if let Ok(Event::Notification { .. }) = events.recv().await {
                return true;
            }
        }
    })
    .await;
    assert!(res.is_err(), "turno não rastreado não deve notificar");
}

#[tokio::test]
async fn brain_result_after_stop_becomes_notification() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Inbound>();
    let (out_tx, out_rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(mock_server(listener, in_tx, out_rx));

    let bus = EventBus::new(256);
    let brain = Arc::new(FakeBrain {
        bus: bus.clone(),
        thread: ThreadId::new(),
        prompts: Mutex::new(vec![]),
        last_turn: Mutex::new(None),
    });
    let storage = will_storage::Storage::open_memory().await.unwrap();
    let deps = RealtimeDeps {
        host: brain.clone(),
        bus: bus.clone(),
        storage,
    };
    let settings = RealtimeSettings {
        enabled: true,
        model: "gpt-4o-realtime-preview".into(),
        activation: "push_to_talk".into(),
        ..Default::default()
    };
    let mgr = RealtimeManager::new(deps, settings, Some(secrecy::SecretString::from("sk-test")));
    mgr.set_endpoint(format!("ws://127.0.0.1:{port}/v1/realtime"));
    let mut events = bus.subscribe();

    assert!(mgr.toggle().await.unwrap());
    // Modelo legado → legacy direto; PTT → turn_detection null.
    let su = expect_type(&mut in_rx, "session.update").await;
    assert!(su["session"]["turn_detection"].is_null());
    assert!(su["session"].get("type").is_none());
    out_tx
        .send(json!({"type": "session.updated", "session": {}}))
        .unwrap();

    // Em PTT, áudio sem tecla pressionada é ignorado.
    mgr.audio_in(Bytes::from_static(&[0u8; 4800]));
    mgr.ptt(true);
    expect_type(&mut in_rx, "input_audio_buffer.clear").await;
    mgr.audio_in(Bytes::from_static(&[7u8; 4800]));
    mgr.ptt(false);
    let app = expect_type(&mut in_rx, "input_audio_buffer.append").await;
    assert_eq!(
        will_realtime::audio::decode_base64(app["audio"].as_str().unwrap()).unwrap(),
        vec![7u8; 4800]
    );
    expect_type(&mut in_rx, "input_audio_buffer.commit").await;
    expect_type(&mut in_rx, "response.create").await;

    // Delegação e depois encerramento antes do resultado.
    out_tx.send(json!({"type": "response.function_call_arguments.done", "name": "ask_brain", "call_id": "c", "arguments": "{\"prompt\":\"pesquise\"}"})).unwrap();
    expect_type(&mut in_rx, "conversation.item.create").await;
    expect_type(&mut in_rx, "response.create").await;
    let turn_id = brain.last_turn.lock().unwrap();
    assert!(!mgr.toggle().await.unwrap());

    bus.emit(Event::TurnFinished {
        thread_id: brain.thread,
        turn_id,
        result: TurnResult {
            text: "Encontrei três artigos.".into(),
            is_error: false,
            cost_usd: None,
            duration_ms: None,
            model: None,
            session_id: None,
            usage: None,
        },
    });
    let ev = wait_event(&mut events, |e| matches!(e, Event::Notification { .. })).await;
    match ev {
        Event::Notification { title, body } => {
            assert_eq!(title, "O cérebro terminou");
            assert_eq!(body, "Encontrei três artigos.");
        }
        _ => unreachable!(),
    }
    assert_eq!(mgr.status()["pending_tasks"].as_array().unwrap().len(), 0);
}
