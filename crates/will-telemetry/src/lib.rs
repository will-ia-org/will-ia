//! Telemetria: os logs (`tracing`) viram registros **OTLP/HTTP (Protobuf)** —
//! o protocolo padrão do OpenTelemetry — e vão para o Axiom ou qualquer outro
//! backend OTLP (Grafana Cloud, New Relic, OpenObserve, SigNoz…), com fila em disco quando a rede falha,
//! redação de segredos e campos fixos da instância (id, host, SO, versão,
//! processo). Um `heartbeat` periódico carrega as **métricas de uso**
//! (eventos por nível e por módulo, spans por nome — turnos, chamadas de
//! ferramenta, mensagens…), que é o que dá "módulo mais usado", taxa de
//! erros e uptime no painel. ADR-0025.
//!
//! Uso: `let (layer, handle) = Telemetry::layer(instance)`; o `layer` entra no
//! `tracing_subscriber::registry()`; `handle.configure(..)` liga/desliga e
//! troca endpoint/token em tempo de execução.

mod instance;
mod layer;
mod metrics;
mod otlp;
mod redact;
mod shipper;

pub use instance::{instance_id, InstanceInfo};
pub use layer::TelemetryLayer;
pub use metrics::{Metrics, Snapshot};
pub use redact::redact;
pub use shipper::{ShipConfig, Stats};

use std::sync::Arc;
use tokio::sync::{mpsc, watch};

/// Um registro de log pronto para envio (já redigido). Chaves reservadas:
/// `time_ns`, `level`, `message`, `trace_id`, `span_id`; o resto vira atributo.
pub type Record = serde_json::Map<String, serde_json::Value>;

/// Um span fechado (vira um span OTLP).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SpanRecord {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub target: String,
    pub module: String,
    pub start_ns: u64,
    pub end_ns: u64,
    pub fields: serde_json::Map<String, serde_json::Value>,
    pub error: bool,
}

/// Item da fila / do spool em disco.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "t")]
pub enum Item {
    #[serde(rename = "log")]
    Log(Record),
    #[serde(rename = "span")]
    Span(SpanRecord),
}

/// Capacidade da fila em memória entre o `Layer` e o remetente.
const QUEUE: usize = 4096;

/// Ponto de controle: configuração em tempo de execução, estatísticas e
/// envio de eventos avulsos (teste, relatórios de crash, heartbeat).
#[derive(Clone)]
pub struct Handle {
    cfg: watch::Sender<Option<ShipConfig>>,
    tx: mpsc::Sender<Item>,
    pub instance: Arc<InstanceInfo>,
    pub metrics: Arc<Metrics>,
    stats: Arc<parking_lot::Mutex<Stats>>,
    level: Arc<std::sync::atomic::AtomicU8>,
}

pub struct Telemetry;

impl Telemetry {
    /// Cria o `Layer` do `tracing` e o `Handle`. O remetente roda em
    /// `tokio::spawn`, então chame dentro de um runtime (ou chame
    /// `Handle::start` depois, com o runtime no ar).
    pub fn layer(instance: InstanceInfo) -> (TelemetryLayer, Handle) {
        let instance = Arc::new(instance);
        let metrics = Arc::new(Metrics::default());
        let (tx, rx) = mpsc::channel(QUEUE);
        let (cfg, cfg_rx) = watch::channel(None);
        let stats = Arc::new(parking_lot::Mutex::new(Stats::default()));
        let level = Arc::new(std::sync::atomic::AtomicU8::new(layer::level_code(
            tracing::Level::INFO,
        )));
        let layer = TelemetryLayer::new(tx.clone(), metrics.clone(), level.clone());
        let handle = Handle {
            cfg,
            tx,
            instance,
            metrics,
            stats,
            level,
        };
        shipper::Pending::stash(rx, cfg_rx);
        (layer, handle)
    }
}

impl Handle {
    /// Sobe a tarefa de envio (uma vez; precisa de runtime tokio ativo).
    pub fn start(&self, spool_dir: std::path::PathBuf) {
        if let Some((rx, cfg_rx)) = shipper::Pending::take() {
            let s = shipper::Shipper {
                rx,
                cfg: cfg_rx,
                instance: self.instance.clone(),
                stats: self.stats.clone(),
                spool: spool_dir.join("telemetry-spool.ndjson"),
            };
            tokio::spawn(s.run());
            let h = self.clone();
            tokio::spawn(async move { h.heartbeat_loop().await });
        }
    }

    /// Liga (`Some`) ou desliga (`None`) o envio; pode ser chamado a qualquer hora.
    pub fn configure(&self, cfg: Option<ShipConfig>, level: tracing::Level) {
        self.level.store(
            layer::level_code(level),
            std::sync::atomic::Ordering::Relaxed,
        );
        let _ = self.cfg.send(cfg);
    }

    /// Nível a partir do texto das configurações (`error|warn|info|debug|trace`).
    pub fn level_from_str(s: &str) -> tracing::Level {
        match s {
            "error" => tracing::Level::ERROR,
            "warn" => tracing::Level::WARN,
            "debug" => tracing::Level::DEBUG,
            "trace" => tracing::Level::TRACE,
            _ => tracing::Level::INFO,
        }
    }

    pub fn enabled(&self) -> bool {
        self.cfg.borrow().is_some()
    }

    pub fn stats(&self) -> Stats {
        self.stats.lock().clone()
    }

    /// Enfileira um evento avulso (campos extras + mensagem), nível `info`.
    pub fn emit(&self, kind: &str, message: &str, fields: serde_json::Value) {
        let mut rec = layer::base_record(tracing::Level::INFO, "will_telemetry", message);
        rec.insert("kind".into(), kind.into());
        if let serde_json::Value::Object(m) = fields {
            for (k, v) in m {
                rec.insert(k, v);
            }
        }
        let _ = self.tx.try_send(Item::Log(rec));
    }

    /// Envia um evento de teste **agora**, fora da fila, e devolve o resultado.
    pub async fn send_test(&self) -> Result<String, String> {
        let cfg = self
            .cfg
            .borrow()
            .clone()
            .ok_or_else(|| "telemetria desligada ou sem token".to_string())?;
        let mut rec = layer::base_record(tracing::Level::INFO, "will_telemetry", "evento de teste");
        rec.insert("kind".into(), "test".into());
        self.instance.decorate(&mut rec);
        let status = shipper::post(&cfg, &self.instance, &[rec]).await?;
        Ok(format!("evento de teste enviado (HTTP {status})"))
    }

    /// Envia o conteúdo novo de um arquivo de log (crash/instalador) como um
    /// evento por arquivo, lembrando o deslocamento já enviado.
    pub fn ship_file_tail(&self, path: &std::path::Path, kind: &str, offsets: &std::path::Path) {
        let Ok(meta) = std::fs::metadata(path) else {
            return;
        };
        let len = meta.len();
        let mut map: std::collections::BTreeMap<String, u64> = std::fs::read(offsets)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let key = path.to_string_lossy().into_owned();
        let from = map.get(&key).copied().unwrap_or(0).min(len);
        if len <= from {
            if len < from {
                map.insert(key, len);
                let _ = std::fs::write(offsets, serde_json::to_vec(&map).unwrap_or_default());
            }
            return;
        }
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        // No máximo os últimos 64 KB novos.
        let start = from.max(len.saturating_sub(64 * 1024)) as usize;
        let text = String::from_utf8_lossy(&bytes[start..]).into_owned();
        let level = if kind == "fatal" {
            tracing::Level::ERROR
        } else {
            tracing::Level::WARN
        };
        let mut rec = layer::base_record(level, "will_telemetry", &format!("arquivo {kind}"));
        rec.insert("kind".into(), "file".into());
        rec.insert("file_kind".into(), kind.into());
        rec.insert(
            "file".into(),
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
        );
        rec.insert("content".into(), redact(&text).into());
        rec.insert("bytes".into(), (len - start as u64).into());
        if self.tx.try_send(Item::Log(rec)).is_ok() {
            map.insert(key, len);
            let _ = std::fs::write(offsets, serde_json::to_vec(&map).unwrap_or_default());
        }
    }

    async fn heartbeat_loop(self) {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
        tick.tick().await;
        loop {
            tick.tick().await;
            self.heartbeat("periodic");
        }
    }

    /// Emite o `heartbeat` com as métricas acumuladas desde o último (delta).
    pub fn heartbeat(&self, reason: &str) {
        let snap = self.metrics.take_snapshot();
        let st = self.stats();
        self.emit(
            "heartbeat",
            "heartbeat",
            serde_json::json!({
                "reason": reason,
                "uptime_s": self.instance.uptime_s(),
                "interval_s": snap.interval_s,
                "events_by_level": snap.by_level,
                "events_by_module": snap.by_module,
                "spans": snap.spans,
                "dropped": snap.dropped,
                "ship": { "sent": st.sent, "failed": st.failed, "spooled": st.spooled },
            }),
        );
    }
}
