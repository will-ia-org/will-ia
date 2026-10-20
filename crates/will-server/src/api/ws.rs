use super::App;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use will_realtime::AudioOut;

/// Stream de eventos do serviço.
pub async fn events_ws(ws: WebSocketUpgrade, State(state): State<App>) -> Response {
    ws.on_upgrade(move |socket| handle_events(socket, state))
}

async fn handle_events(mut socket: WebSocket, state: App) {
    let mut rx = state.bus.subscribe();
    // snapshot inicial de status dos módulos
    let snapshot: Vec<(will_core::Module, will_core::ModuleStatus)> = state
        .module_status
        .read()
        .iter()
        .map(|(m, s)| (*m, s.clone()))
        .collect();
    for (m, s) in snapshot {
        let ev = will_core::Event::ModuleStatus {
            module: m,
            status: s,
        };
        if let Ok(t) = serde_json::to_string(&ev) {
            if socket.send(Message::Text(t.into())).await.is_err() {
                return;
            }
        }
    }
    loop {
        tokio::select! {
            ev = rx.recv() => {
                match ev {
                    Ok(ev) => {
                        if let Ok(t) = serde_json::to_string(&ev) {
                            if socket.send(Message::Text(t.into())).await.is_err() { break; }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
        }
    }
}

/// Áudio da voz: binário PCM16 24 kHz nos dois sentidos + controle em JSON.
pub async fn voice_ws(ws: WebSocketUpgrade, State(state): State<App>) -> Response {
    ws.on_upgrade(move |socket| handle_voice(socket, state))
}

async fn handle_voice(socket: WebSocket, state: App) {
    let Some(rt) = state.realtime.read().clone() else {
        return;
    };
    let (mut tx, mut rx_ws) = socket.split();
    let mut audio_rx = rt.subscribe_audio_out();
    let rt2 = rt.clone();
    let send_task = tokio::spawn(async move {
        loop {
            match audio_rx.recv().await {
                Ok(AudioOut::Pcm(b)) => {
                    if tx.send(Message::Binary(b)).await.is_err() {
                        break;
                    }
                }
                Ok(AudioOut::Control(v)) => {
                    if tx.send(Message::Text(v.to_string().into())).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });
    while let Some(Ok(msg)) = rx_ws.next().await {
        match msg {
            Message::Binary(b) => rt2.audio_in(b),
            Message::Text(t) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                    match v.get("type").and_then(|x| x.as_str()) {
                        Some("ptt") => {
                            rt2.ptt(v.get("down").and_then(|d| d.as_bool()).unwrap_or(false))
                        }
                        Some("commit") => rt2.commit(),
                        Some("start") => {
                            let _ = rt2.start().await;
                        }
                        Some("stop") => rt2.stop().await,
                        _ => {}
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    send_task.abort();
}
