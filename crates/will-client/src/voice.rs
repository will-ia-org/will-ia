//! Áudio da voz pelo WS `/api/voice/audio`: o microfone e o alto-falante são
//! do app; a sessão OpenAI é do servidor. Interface espelha o que o app
//! usava do `RealtimeManager` em processo (`audio_in`, `ptt`, `commit`,
//! `subscribe_audio_out`), para o pipeline de áudio não mudar.

use crate::Client;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;

/// O que chega do servidor.
#[derive(Debug, Clone)]
pub enum AudioOut {
    /// PCM16 mono 24 kHz do assistente.
    Pcm(Bytes),
    /// Controle JSON (`stop_playback`, `state`, `transcript`…).
    Control(Value),
}

pub struct VoiceLink {
    tx: mpsc::UnboundedSender<Message>,
    out: broadcast::Sender<AudioOut>,
    task: parking_lot::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl VoiceLink {
    /// Abre o WS. Falha só se a conexão inicial não subir.
    pub async fn connect(client: &Client) -> Result<Arc<Self>, String> {
        let url = format!(
            "{}/api/voice/audio?token={}",
            client.ws_base(),
            client.token()
        );
        let ws = client
            .connect_ws(&url)
            .await
            .map_err(|e| format!("áudio da voz: {e}"))?;
        let (mut ws_tx, mut ws_rx) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let (out, _) = broadcast::channel::<AudioOut>(512);
        let out2 = out.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    m = rx.recv() => match m {
                        Some(m) => { if ws_tx.send(m).await.is_err() { break; } }
                        None => { let _ = ws_tx.send(Message::Close(None)).await; break; }
                    },
                    m = ws_rx.next() => match m {
                        Some(Ok(Message::Binary(b))) => { let _ = out2.send(AudioOut::Pcm(b)); }
                        Some(Ok(Message::Text(t))) => {
                            if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                                let _ = out2.send(AudioOut::Control(v));
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Ok(_)) => {}
                        Some(Err(e)) => { tracing::debug!(error = %e, "ws de áudio caiu"); break; }
                    }
                }
            }
            let _ = out2.send(AudioOut::Control(
                serde_json::json!({"type": "state", "state": "disconnected"}),
            ));
        });
        Ok(Arc::new(Self {
            tx,
            out,
            task: parking_lot::Mutex::new(Some(task)),
        }))
    }

    /// PCM16 mono 24 kHz do microfone.
    pub fn audio_in(&self, pcm: Bytes) {
        let _ = self.tx.send(Message::Binary(pcm));
    }
    pub fn ptt(&self, down: bool) {
        self.control(serde_json::json!({"type": "ptt", "down": down}));
    }
    pub fn commit(&self) {
        self.control(serde_json::json!({"type": "commit"}));
    }
    pub fn control(&self, v: Value) {
        let _ = self.tx.send(Message::Text(v.to_string().into()));
    }
    pub fn subscribe_audio_out(&self) -> broadcast::Receiver<AudioOut> {
        self.out.subscribe()
    }
    pub fn close(&self) {
        if let Some(t) = self.task.lock().take() {
            t.abort();
        }
    }
}

impl Drop for VoiceLink {
    fn drop(&mut self) {
        self.close();
    }
}
