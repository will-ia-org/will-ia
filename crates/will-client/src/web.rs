//! Peças do cliente que só existem no navegador: o WebSocket de eventos sobre
//! o `WebSocket` do próprio navegador e um `sleep` baseado em `setTimeout`.

use crate::{Client, ClientEvent};
use futures::channel::{mpsc, oneshot};
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use will_core::Event;

/// Espera `d` sem travar a página.
pub async fn sleep(d: Duration) {
    let ms = d.as_millis().min(i32::MAX as u128) as i32;
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

enum WsMsg {
    Open,
    Text(String),
    Closed(String),
}

/// Um `WebSocket` do navegador entregue como fila de mensagens. Fecha ao sair de escopo.
struct Socket {
    ws: web_sys::WebSocket,
    rx: mpsc::UnboundedReceiver<WsMsg>,
    _on_open: Closure<dyn FnMut()>,
    _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_close: Closure<dyn FnMut(web_sys::CloseEvent)>,
}

impl Socket {
    fn open(url: &str) -> Result<Self, String> {
        let ws = web_sys::WebSocket::new(url).map_err(|e| format!("{e:?}"))?;
        let (tx, rx) = mpsc::unbounded();
        let t = tx.clone();
        let on_open = Closure::<dyn FnMut()>::new(move || {
            let _ = t.unbounded_send(WsMsg::Open);
        });
        let t = tx.clone();
        let on_message =
            Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
                if let Some(s) = e.data().as_string() {
                    let _ = t.unbounded_send(WsMsg::Text(s));
                }
            });
        let on_close =
            Closure::<dyn FnMut(web_sys::CloseEvent)>::new(move |e: web_sys::CloseEvent| {
                let reason = e.reason();
                let _ = tx.unbounded_send(WsMsg::Closed(if reason.is_empty() {
                    "conexão fechada".into()
                } else {
                    reason
                }));
            });
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        Ok(Self {
            ws,
            rx,
            _on_open: on_open,
            _on_message: on_message,
            _on_close: on_close,
        })
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onclose(None);
        let _ = self.ws.close();
    }
}

/// Encerra o laço de eventos quando `abort` é chamado (ou ao sair de escopo).
pub struct EventsHandle {
    stop: parking_lot::Mutex<Option<oneshot::Sender<()>>>,
}

impl EventsHandle {
    pub fn abort(&self) {
        if let Some(tx) = self.stop.lock().take() {
            let _ = tx.send(());
        }
    }
}

impl Client {
    /// Como `spawn_events` do desktop: mantém o WS `/api/events` aberto,
    /// reconectando com recuo (1 s → 10 s).
    pub fn spawn_events_local(
        self: &Arc<Self>,
        sink: std::sync::mpsc::Sender<ClientEvent>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> EventsHandle {
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let me = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let run = async move {
                let mut backoff = Duration::from_secs(1);
                loop {
                    let url = format!("{}/api/events?token={}", me.ws_base(), me.token());
                    let reason = match Socket::open(&url) {
                        Ok(mut sock) => loop {
                            match sock.rx.next().await {
                                Some(WsMsg::Open) => {
                                    backoff = Duration::from_secs(1);
                                    if sink.send(ClientEvent::Connected).is_err() {
                                        return;
                                    }
                                    wake();
                                }
                                Some(WsMsg::Text(t)) => {
                                    if let Ok(ev) = serde_json::from_str::<Event>(&t) {
                                        if sink.send(ClientEvent::Event(ev)).is_err() {
                                            return;
                                        }
                                        wake();
                                    }
                                }
                                Some(WsMsg::Closed(r)) => break r,
                                None => break "conexão fechada".to_string(),
                            }
                        },
                        Err(e) => e,
                    };
                    if sink.send(ClientEvent::Disconnected(reason)).is_err() {
                        return;
                    }
                    wake();
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
            };
            futures::pin_mut!(run);
            let _ = futures::future::select(run, stop_rx).await;
        });
        EventsHandle {
            stop: parking_lot::Mutex::new(Some(stop_tx)),
        }
    }
}
