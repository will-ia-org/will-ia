//! Versão do navegador do áudio da voz (`voice.rs`): a mesma interface, sobre
//! o `WebSocket` do próprio navegador. O microfone e o alto-falante ficam com
//! a página (`window.willIaAudio`, ver `will-app/src/voice/audio_web.rs`).

use crate::Client;
use bytes::Bytes;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::broadcast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

/// O que chega do servidor.
#[derive(Debug, Clone)]
pub enum AudioOut {
    /// PCM16 mono 24 kHz do assistente.
    Pcm(Bytes),
    /// Controle JSON (`stop_playback`, `state`, `transcript`…).
    Control(Value),
}

pub struct VoiceLink {
    ws: web_sys::WebSocket,
    out: broadcast::Sender<AudioOut>,
    _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_close: Closure<dyn FnMut()>,
}

impl VoiceLink {
    /// Abre o WS. Falha só se a conexão inicial não subir.
    pub async fn connect(client: &Client) -> Result<Arc<Self>, String> {
        let url = format!(
            "{}/api/voice/audio?token={}",
            client.ws_base(),
            client.token()
        );
        let ws = web_sys::WebSocket::new(&url).map_err(|e| format!("áudio da voz: {e:?}"))?;
        ws.set_binary_type(web_sys::BinaryType::Arraybuffer);
        // Espera abrir (ou falhar) antes de devolver o link.
        let (open_tx, open_rx) = futures::channel::oneshot::channel::<bool>();
        let open_tx = std::rc::Rc::new(std::cell::RefCell::new(Some(open_tx)));
        let t = open_tx.clone();
        let on_open = Closure::<dyn FnMut()>::new(move || {
            if let Some(tx) = t.borrow_mut().take() {
                let _ = tx.send(true);
            }
        });
        let t = open_tx.clone();
        let on_error = Closure::<dyn FnMut()>::new(move || {
            if let Some(tx) = t.borrow_mut().take() {
                let _ = tx.send(false);
            }
        });
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        let opened = open_rx.await.unwrap_or(false);
        ws.set_onopen(None);
        ws.set_onerror(None);
        if !opened {
            let _ = ws.close();
            return Err("áudio da voz: não foi possível abrir a conexão".into());
        }
        let (out, _) = broadcast::channel::<AudioOut>(512);
        let o = out.clone();
        let on_message =
            Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
                let data = e.data();
                if let Some(text) = data.as_string() {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        let _ = o.send(AudioOut::Control(v));
                    }
                } else if data.is_instance_of::<js_sys::ArrayBuffer>() {
                    let bytes = js_sys::Uint8Array::new(&data).to_vec();
                    let _ = o.send(AudioOut::Pcm(Bytes::from(bytes)));
                }
            });
        let o = out.clone();
        let on_close = Closure::<dyn FnMut()>::new(move || {
            let _ = o.send(AudioOut::Control(
                serde_json::json!({"type": "state", "state": "disconnected"}),
            ));
        });
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        Ok(Arc::new(Self {
            ws,
            out,
            _on_message: on_message,
            _on_close: on_close,
        }))
    }

    fn is_open(&self) -> bool {
        self.ws.ready_state() == web_sys::WebSocket::OPEN
    }

    /// PCM16 mono 24 kHz do microfone.
    pub fn audio_in(&self, pcm: Bytes) {
        if self.is_open() {
            let _ = self.ws.send_with_u8_array(&pcm);
        }
    }
    pub fn ptt(&self, down: bool) {
        self.control(serde_json::json!({"type": "ptt", "down": down}));
    }
    pub fn commit(&self) {
        self.control(serde_json::json!({"type": "commit"}));
    }
    pub fn control(&self, v: Value) {
        if self.is_open() {
            let _ = self.ws.send_with_str(&v.to_string());
        }
    }
    pub fn subscribe_audio_out(&self) -> broadcast::Receiver<AudioOut> {
        self.out.subscribe()
    }
    pub fn close(&self) {
        self.ws.set_onmessage(None);
        self.ws.set_onclose(None);
        let _ = self.ws.close();
    }
}

impl Drop for VoiceLink {
    fn drop(&mut self) {
        self.close();
    }
}
