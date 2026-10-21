//! Task única que é dona do WebSocket com a OpenAI Realtime.
//!
//! Recebe comandos do `RealtimeManager` (áudio, PTT, commit, resultado do
//! cérebro), traduz eventos do servidor em estado/áudio para a UI e cuida da
//! reconexão com backoff.

use crate::audio::{decode_base64, encode_base64, pcm_ms, Chunker};
use crate::protocol::{self as proto, ServerEvent, SessionVariant};
use crate::RealtimeManager;
use bytes::Bytes;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;
use will_core::settings::RealtimeSettings;
use will_core::TurnId;

pub(crate) type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsSink = SplitSink<WsStream, Message>;
type WsSource = SplitStream<WsStream>;

const RECONNECT_BACKOFF: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);

/// Comandos do manager para a task da sessão.
#[derive(Debug)]
pub(crate) enum Cmd {
    Audio(Bytes),
    Ptt(bool),
    Commit,
    /// Texto já limpo/prefixado para injetar como item do usuário.
    BrainText(String),
}

/// Lado do manager: canal de comandos + cancelamento.
pub(crate) struct SessionHandle {
    pub id: u64,
    pub cmd_tx: mpsc::UnboundedSender<Cmd>,
    pub cancel: CancellationToken,
    pub task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl SessionHandle {
    pub fn send(&self, cmd: Cmd) -> bool {
        self.cmd_tx.send(cmd).is_ok()
    }
}

enum Outcome {
    /// Encerrada pelo usuário.
    Stopped,
    /// Conexão caiu ou erro fatal de transporte.
    Lost(String),
}

/// Estado mutável de uma conexão (zera a cada reconexão).
#[derive(Default)]
struct Conn {
    chunker: Chunker,
    /// Bytes enviados desde o último commit (evita commit de buffer vazio).
    uncommitted_bytes: usize,
    response_active: bool,
    /// Item do assistant cujo áudio está sendo tocado.
    current_item: Option<String>,
    audio_bytes: usize,
    audio_started: Option<Instant>,
    speaking_announced: bool,
}

pub(crate) struct SessionRunner {
    pub id: u64,
    mgr: Arc<RealtimeManager>,
    settings: RealtimeSettings,
    api_key: String,
    endpoint: String,
    variant: Mutex<SessionVariant>,
    fallback_done: AtomicBool,
    /// O servidor aceitou o `session.update` (persona + ferramentas aplicadas)?
    session_updated: AtomicBool,
    /// Uso acumulado da sessão (soma dos `response.done.usage`).
    usage: Mutex<SessionUsage>,
    started: Instant,
}

/// Tokens consumidos numa sessão de voz, por tipo.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct SessionUsage {
    pub responses: u64,
    pub text_in: u64,
    pub text_cached: u64,
    pub audio_in: u64,
    pub audio_cached: u64,
    pub text_out: u64,
    pub audio_out: u64,
}

impl SessionUsage {
    /// Soma um objeto `usage` do `response.done` (formato GA e beta).
    pub fn add(&mut self, u: &Value) {
        let g = |v: &Value, path: &[&str]| -> u64 {
            let mut cur = v;
            for p in path {
                cur = match cur.get(p) {
                    Some(x) => x,
                    None => return 0,
                };
            }
            cur.as_u64().unwrap_or(0)
        };
        self.responses += 1;
        let text_in = g(u, &["input_token_details", "text_tokens"]);
        let audio_in = g(u, &["input_token_details", "audio_tokens"]);
        let cached_text = g(
            u,
            &[
                "input_token_details",
                "cached_tokens_details",
                "text_tokens",
            ],
        );
        let cached_audio = g(
            u,
            &[
                "input_token_details",
                "cached_tokens_details",
                "audio_tokens",
            ],
        );
        let cached_total = g(u, &["input_token_details", "cached_tokens"]);
        // Sem detalhe do cache, atribui tudo ao texto (mais barato = estimativa conservadora para baixo? não:
        // texto e áudio em cache custam o mesmo na tabela padrão, então tanto faz).
        let (cached_text, cached_audio) = if cached_text + cached_audio == 0 {
            (cached_total, 0)
        } else {
            (cached_text, cached_audio)
        };
        self.text_in += text_in.saturating_sub(cached_text);
        self.text_cached += cached_text;
        self.audio_in += audio_in.saturating_sub(cached_audio);
        self.audio_cached += cached_audio;
        self.text_out += g(u, &["output_token_details", "text_tokens"]);
        self.audio_out += g(u, &["output_token_details", "audio_tokens"]);
    }

    pub fn total(&self) -> u64 {
        self.text_in
            + self.text_cached
            + self.audio_in
            + self.audio_cached
            + self.text_out
            + self.audio_out
    }

    /// Custo estimado em USD pela tabela (USD por 1M tokens).
    pub fn usd(&self, p: &will_core::settings::VoicePricing) -> f64 {
        (self.text_in as f64 * p.text_in
            + self.text_cached as f64 * p.text_cached
            + self.audio_in as f64 * p.audio_in
            + self.audio_cached as f64 * p.audio_cached
            + self.text_out as f64 * p.text_out
            + self.audio_out as f64 * p.audio_out)
            / 1_000_000.0
    }
}

impl SessionRunner {
    pub fn new(
        id: u64,
        mgr: Arc<RealtimeManager>,
        settings: RealtimeSettings,
        api_key: String,
        endpoint: String,
    ) -> Self {
        let variant = SessionVariant::preferred_for(&settings.model);
        Self {
            id,
            mgr,
            settings,
            api_key,
            endpoint,
            variant: Mutex::new(variant),
            fallback_done: AtomicBool::new(false),
            session_updated: AtomicBool::new(false),
            usage: Mutex::new(SessionUsage::default()),
            started: Instant::now(),
        }
    }

    fn url(&self) -> String {
        let sep = if self.endpoint.contains('?') {
            '&'
        } else {
            '?'
        };
        format!("{}{}model={}", self.endpoint, sep, self.settings.model)
    }

    /// Abre o WebSocket (sem enviar `session.update`).
    pub async fn connect(&self) -> Result<WsStream, String> {
        let url = self.url();
        let mut req = url
            .clone()
            .into_client_request()
            .map_err(|e| format!("URL inválida ({url}): {e}"))?;
        let auth = HeaderValue::from_str(&format!("Bearer {}", self.api_key))
            .map_err(|_| "API key inválida".to_string())?;
        req.headers_mut().insert("Authorization", auth);
        // Sem header `OpenAI-Beta`: a API Realtime GA (/v1/realtime) rejeita o formato beta.
        match connect_async(req).await {
            Ok((ws, _resp)) => Ok(ws),
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                let status = resp.status();
                let body = resp
                    .body()
                    .as_ref()
                    .map(|b| {
                        String::from_utf8_lossy(b)
                            .chars()
                            .take(300)
                            .collect::<String>()
                    })
                    .unwrap_or_default();
                Err(format!("HTTP {status}: {body}"))
            }
            Err(e) => Err(format!("falha ao conectar: {e}")),
        }
    }

    /// Loop principal: dirige a conexão e reconecta até 3× em caso de queda.
    pub async fn run(
        self: Arc<Self>,
        ws: WsStream,
        mut cmd_rx: mpsc::UnboundedReceiver<Cmd>,
        cancel: CancellationToken,
    ) {
        let mut ws = Some(ws);
        let mut attempt = 0usize;
        loop {
            let stream = match ws.take() {
                Some(s) => s,
                None => {
                    if attempt >= RECONNECT_BACKOFF.len() {
                        self.mgr
                            .set_state("stopped", Some("conexão perdida".into()));
                        break;
                    }
                    let wait = RECONNECT_BACKOFF[attempt];
                    attempt += 1;
                    self.mgr.set_state(
                        "connecting",
                        Some(format!(
                            "reconectando ({attempt}/{})",
                            RECONNECT_BACKOFF.len()
                        )),
                    );
                    tokio::select! {
                        _ = cancel.cancelled() => { self.mgr.set_state("stopped", None); break; }
                        _ = tokio::time::sleep(wait) => {}
                    }
                    match self.connect().await {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(attempt, error = %e, "realtime: reconexão falhou");
                            self.mgr.deps.bus.log(
                                "warn",
                                "realtime",
                                format!("reconexão {attempt} falhou: {e}"),
                            );
                            continue;
                        }
                    }
                }
            };
            match self.drive(stream, &mut cmd_rx, &cancel).await {
                Outcome::Stopped => {
                    self.mgr.set_state("stopped", None);
                    break;
                }
                Outcome::Lost(reason) => {
                    tracing::warn!(reason = %reason, "realtime: conexão perdida");
                    self.mgr
                        .deps
                        .bus
                        .log("warn", "realtime", format!("conexão perdida: {reason}"));
                    self.mgr.audio_control(json!({"type": "stop_playback"}));
                    if cancel.is_cancelled() {
                        self.mgr.set_state("stopped", None);
                        break;
                    }
                }
            }
        }
        let usage = self.usage.lock().clone();
        self.mgr
            .session_finished(self.id, usage, self.started.elapsed());
        self.mgr.session_ended(self.id);
    }

    async fn drive(
        &self,
        ws: WsStream,
        cmd_rx: &mut mpsc::UnboundedReceiver<Cmd>,
        cancel: &CancellationToken,
    ) -> Outcome {
        let (mut sink, mut source): (WsSink, WsSource) = ws.split();
        let mut conn = Conn::default();
        let variant = *self.variant.lock();
        tracing::info!(variant = variant.as_str(), model = %self.settings.model, "realtime: enviando session.update");
        if let Err(e) = send_json(&mut sink, &proto::session_update(variant, &self.settings)).await
        {
            return Outcome::Lost(e);
        }
        let mut flush = tokio::time::interval(FLUSH_INTERVAL);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            let step: Result<Option<Outcome>, String> = tokio::select! {
                _ = cancel.cancelled() => {
                    let _ = sink.send(Message::Close(None)).await;
                    Ok(Some(Outcome::Stopped))
                }
                msg = source.next() => match msg {
                    Some(Ok(Message::Text(t))) => self.on_text(&mut conn, &mut sink, t.as_str()).await.map(|_| None),
                    Some(Ok(Message::Close(frame))) => Ok(Some(Outcome::Lost(format!("fechado pelo servidor: {frame:?}")))),
                    Some(Ok(_)) => Ok(None),
                    Some(Err(e)) => Ok(Some(Outcome::Lost(e.to_string()))),
                    None => Ok(Some(Outcome::Lost("conexão encerrada".into()))),
                },
                cmd = cmd_rx.recv() => match cmd {
                    Some(cmd) => self.on_cmd(&mut conn, &mut sink, cmd).await.map(|_| None),
                    None => Ok(Some(Outcome::Stopped)),
                },
                _ = flush.tick() => self.flush_audio(&mut conn, &mut sink, false).await.map(|_| None),
            };
            match step {
                Ok(None) => {}
                Ok(Some(outcome)) => return outcome,
                Err(e) => return Outcome::Lost(e),
            }
        }
    }

    // ----- comandos do manager -------------------------------------------------

    async fn on_cmd(&self, conn: &mut Conn, sink: &mut WsSink, cmd: Cmd) -> Result<(), String> {
        match cmd {
            Cmd::Audio(bytes) => {
                for chunk in conn.chunker.push(&bytes) {
                    self.send_audio_chunk(conn, sink, &chunk).await?;
                }
                Ok(())
            }
            Cmd::Ptt(true) => {
                self.barge_in(conn, sink).await?;
                conn.chunker.clear();
                conn.uncommitted_bytes = 0;
                send_json(sink, &proto::audio_clear()).await?;
                self.mgr.set_state("user_speaking", None);
                Ok(())
            }
            Cmd::Ptt(false) | Cmd::Commit => self.flush_audio(conn, sink, true).await,
            Cmd::BrainText(text) => {
                send_json(sink, &proto::user_text_item(&text)).await?;
                send_json(sink, &proto::response_create()).await
            }
        }
    }

    async fn send_audio_chunk(
        &self,
        conn: &mut Conn,
        sink: &mut WsSink,
        chunk: &[u8],
    ) -> Result<(), String> {
        conn.uncommitted_bytes += chunk.len();
        send_json(sink, &proto::audio_append(&encode_base64(chunk))).await
    }

    /// Envia o resto do buffer; com `commit`, fecha o turno e pede resposta.
    async fn flush_audio(
        &self,
        conn: &mut Conn,
        sink: &mut WsSink,
        commit: bool,
    ) -> Result<(), String> {
        if let Some(rest) = conn.chunker.flush() {
            self.send_audio_chunk(conn, sink, &rest).await?;
        }
        if commit {
            if conn.uncommitted_bytes == 0 {
                tracing::debug!("realtime: commit ignorado (sem áudio)");
                return Ok(());
            }
            conn.uncommitted_bytes = 0;
            send_json(sink, &proto::audio_commit()).await?;
            send_json(sink, &proto::response_create()).await?;
            self.mgr.set_state("thinking", None);
        }
        Ok(())
    }

    // ----- eventos do servidor -------------------------------------------------

    async fn on_text(&self, conn: &mut Conn, sink: &mut WsSink, text: &str) -> Result<(), String> {
        let event = match proto::parse_server_event(text) {
            Ok(ev) => ev,
            Err(e) => {
                tracing::warn!(error = %e, "realtime: evento ilegível");
                return Ok(());
            }
        };
        match event {
            ServerEvent::SessionCreated => {
                tracing::info!("realtime: session.created");
            }
            ServerEvent::SessionUpdated => {
                self.session_updated.store(true, Ordering::SeqCst);
                tracing::info!(
                    variant = self.variant.lock().as_str(),
                    "realtime: session.updated"
                );
                if !conn.response_active {
                    self.mgr.set_state("listening", None);
                }
            }
            ServerEvent::SpeechStarted => {
                // Com barge-in desligado, o VAD do servidor pode disparar com o eco do
                // próprio assistente (cauda do alto-falante/fone Bluetooth). Nesse caso não
                // interrompemos a reprodução local nem truncamos o item: só ignoramos.
                let still_playing = conn
                    .audio_started
                    .map(|t| t.elapsed().as_millis() as u64 <= pcm_ms(conn.audio_bytes) + 800)
                    .unwrap_or(false);
                if !self.settings.barge_in
                    && !proto::is_push_to_talk(&self.settings)
                    && (conn.response_active || still_playing)
                {
                    tracing::debug!("realtime: fala detectada durante a resposta com barge-in desligado; ignorando");
                    return Ok(());
                }
                self.mgr.set_state("user_speaking", None);
                self.barge_in(conn, sink).await?;
            }
            ServerEvent::SpeechStopped => {
                conn.uncommitted_bytes = 0;
                self.mgr.set_state("thinking", None);
            }
            ServerEvent::InputTranscript(t) => self.on_transcript("user", t).await,
            ServerEvent::ResponseCreated => {
                conn.response_active = true;
                if !conn.speaking_announced {
                    self.mgr.set_state("thinking", None);
                }
            }
            ServerEvent::AudioDelta { item_id, delta } => {
                let pcm = match decode_base64(&delta) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(error = %e, "realtime: áudio base64 inválido");
                        return Ok(());
                    }
                };
                if conn.current_item != item_id || conn.audio_started.is_none() {
                    conn.current_item = item_id;
                    conn.audio_bytes = 0;
                    conn.audio_started = Some(Instant::now());
                }
                conn.audio_bytes += pcm.len();
                if !conn.speaking_announced {
                    conn.speaking_announced = true;
                    self.mgr.set_state("speaking", None);
                }
                self.mgr.audio_pcm(Bytes::from(pcm));
            }
            ServerEvent::AudioDone => {
                tracing::debug!(
                    bytes = conn.audio_bytes,
                    "realtime: áudio da resposta completo"
                );
            }
            ServerEvent::OutputTranscript(t) | ServerEvent::TextDone(t) => {
                self.on_transcript("assistant", t).await
            }
            ServerEvent::FunctionCall {
                name,
                call_id,
                arguments,
            } => {
                self.on_function_call(conn, sink, &name, &call_id, &arguments)
                    .await?;
            }
            ServerEvent::ResponseDone { status, usage } => {
                if let Some(u) = &usage {
                    self.usage.lock().add(u);
                }
                conn.response_active = false;
                conn.speaking_announced = false;
                if let Some(s) = status.as_deref() {
                    if s != "completed" {
                        tracing::info!(status = s, "realtime: response.done");
                    }
                }
                let pending = self.mgr.pending_count();
                if pending > 0 {
                    self.mgr.set_state(
                        "brain_working",
                        Some(format!("{pending} tarefa(s) pendente(s)")),
                    );
                } else {
                    self.mgr.set_state("listening", None);
                }
            }
            ServerEvent::Error {
                code,
                message,
                param,
            } => {
                self.on_error(sink, code, message, param).await?;
            }
            ServerEvent::RateLimits => tracing::debug!("realtime: rate_limits.updated"),
            ServerEvent::Other(kind) => tracing::trace!(kind = %kind, "realtime: evento ignorado"),
        }
        Ok(())
    }

    async fn on_error(
        &self,
        sink: &mut WsSink,
        code: Option<String>,
        message: String,
        param: Option<String>,
    ) -> Result<(), String> {
        let unknown =
            proto::is_unknown_parameter_error(code.as_deref(), &message, param.as_deref());
        if unknown && !self.fallback_done.swap(true, Ordering::SeqCst) {
            let next = {
                let mut v = self.variant.lock();
                *v = v.other();
                *v
            };
            tracing::warn!(error = %message, variant = next.as_str(), "realtime: session.update rejeitado; tentando outra variante");
            self.mgr.deps.bus.log(
                "warn",
                "realtime",
                format!(
                    "session.update rejeitado ({message}); tentando variante {}",
                    next.as_str()
                ),
            );
            return send_json(sink, &proto::session_update(next, &self.settings)).await;
        }
        if code.as_deref() == Some("response_cancel_not_active") {
            // O servidor já tinha cancelado/concluído a resposta: nada a fazer.
            tracing::debug!("realtime: response.cancel sem resposta ativa (benigno)");
            return Ok(());
        }
        if !self.session_updated.load(Ordering::SeqCst) {
            // Erro antes do `session.updated`: a configuração foi rejeitada e a sessão
            // seguiu com os padrões do servidor (sem persona, sem ferramentas).
            let msg = format!("configuração da sessão rejeitada pela OpenAI ({message}). A voz funciona, mas SEM persona e SEM ferramentas (ask_brain, list_threads). Corrija em Configurações › Voz e reinicie a voz.");
            tracing::error!(code = ?code, param = ?param, "realtime: {msg}");
            self.mgr.deps.bus.log("error", "realtime", msg.clone());
            self.mgr.deps.bus.emit(will_core::Event::Notification {
                title: "Voz: configuração rejeitada".into(),
                body: msg.clone(),
            });
            self.mgr.set_state("error", Some(msg));
            return Ok(());
        }
        tracing::error!(code = ?code, param = ?param, error = %message, "realtime: erro do servidor");
        self.mgr
            .deps
            .bus
            .log("error", "realtime", format!("erro do servidor: {message}"));
        self.mgr.set_state("error", Some(message));
        Ok(())
    }

    async fn on_transcript(&self, role: &str, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.mgr
            .audio_control(json!({"type": "transcript", "role": role, "text": text}));
        if self.settings.save_transcripts {
            if let Err(e) = self
                .mgr
                .deps
                .storage
                .add_voice_transcript(role, &text)
                .await
            {
                tracing::warn!(error = %e, "realtime: falha ao salvar transcrição");
            }
        }
    }

    async fn on_function_call(
        &self,
        conn: &mut Conn,
        sink: &mut WsSink,
        name: &str,
        call_id: &str,
        arguments: &str,
    ) -> Result<(), String> {
        let output = match name {
            "ask_brain" => match proto::parse_ask_brain_args(arguments) {
                Ok(args) => match self
                    .mgr
                    .deps
                    .host
                    .ask_brain(
                        &args.prompt,
                        args.new_thread,
                        args.thread.as_deref(),
                        args.title.as_deref(),
                    )
                    .await
                {
                    Ok(task) => {
                        self.mgr.track_pending(task.turn_id, task.thread_id);
                        self.mgr
                            .set_state("brain_working", Some(task.turn_id.to_string()));
                        tracing::info!(turn = %task.turn_id, "realtime: tarefa delegada ao cérebro");
                        json!({"status": "working", "task_id": task.turn_id.to_string()})
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "realtime: ask_brain falhou");
                        json!({"status": "error", "error": e})
                    }
                },
                Err(e) => json!({"status": "error", "error": e}),
            },
            "brain_status" => {
                let busy = self.mgr.deps.host.brain_busy().await;
                json!({"busy": busy, "pending_tasks": self.mgr.pending_count()})
            }
            "list_threads" => match self.mgr.deps.host.brain_threads().await {
                Ok(list) => {
                    let items: Vec<Value> = list
                        .iter()
                        .take(20)
                        .map(|t| {
                            json!({
                                "id": t.id.to_string(),
                                "title": t.title.clone().unwrap_or_else(|| "(sem título)".into()),
                                "origin": t.origin.kind().as_str(),
                                "last_activity": t.last_activity_at.to_rfc3339(),
                                "working": t.pending > 0,
                            })
                        })
                        .collect();
                    json!({"threads": items, "count": list.len()})
                }
                Err(e) => json!({"status": "error", "error": e}),
            },
            other => {
                tracing::warn!(tool = other, "realtime: ferramenta desconhecida");
                json!({"status": "error", "error": format!("ferramenta desconhecida: {other}")})
            }
        };
        conn.response_active = true;
        send_json(sink, &proto::function_call_output(call_id, &output)).await?;
        send_json(sink, &proto::response_create()).await
    }

    /// Usuário começou a falar enquanto o assistente (provavelmente) ainda toca.
    async fn barge_in(&self, conn: &mut Conn, sink: &mut WsSink) -> Result<(), String> {
        let Some(item) = conn.current_item.take() else {
            return Ok(());
        };
        let total_ms = pcm_ms(conn.audio_bytes);
        let played_ms = conn
            .audio_started
            .take()
            .map(|t| t.elapsed().as_millis() as u64)
            .unwrap_or(total_ms);
        conn.audio_bytes = 0;
        if played_ms >= total_ms && !conn.response_active {
            // Já terminou de tocar; nada a interromper.
            return Ok(());
        }
        let played_ms = played_ms.min(total_ms);
        tracing::info!(item = %item, played_ms, total_ms, "realtime: barge-in");
        self.mgr.audio_control(json!({"type": "stop_playback"}));
        // Com `interrupt_response` o servidor costuma cancelar sozinho; mandamos o
        // response.cancel mesmo assim (idempotente: `response_cancel_not_active` é ignorado).
        if conn.response_active {
            send_json(sink, &proto::response_cancel()).await?;
        }
        send_json(sink, &proto::truncate_item(&item, played_ms)).await
    }
}

async fn send_json(sink: &mut WsSink, value: &Value) -> Result<(), String> {
    sink.send(Message::text(value.to_string()))
        .await
        .map_err(|e| format!("falha ao enviar: {e}"))
}

/// Tarefa delegada ao cérebro aguardando resultado.
#[derive(Debug, Clone)]
pub(crate) struct PendingTask {
    pub thread_id: will_core::ThreadId,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

pub(crate) type PendingMap = std::collections::HashMap<TurnId, PendingTask>;
