//! Remetente: lotes de logs/spans → OTLP/HTTP (Protobuf). Sem rede, os itens
//! vão para um spool em disco (NDJSON, limitado) e são reenviados depois.

use crate::{InstanceInfo, Item, Record, SpanRecord};
use secrecy::{ExposeSecret, SecretString};
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::sync::{mpsc, watch};

/// Para onde e como enviar. Montado pelo servidor a partir das configurações
/// (preset Axiom ou OTLP genérico).
#[derive(Clone)]
pub struct ShipConfig {
    pub logs_url: String,
    /// `None` desliga o envio de spans.
    pub traces_url: Option<String>,
    /// Cabeçalhos extras (ex.: `X-Axiom-Dataset`).
    pub headers: Vec<(String, String)>,
    /// Vira `Authorization: Bearer …`.
    pub bearer: Option<SecretString>,
}

impl ShipConfig {
    /// Lê o JSON de `GET /api/telemetry/client-config` (`enabled`, `logs_url`,
    /// `traces_url`, `headers`, `bearer`).
    pub fn from_client_config(v: &serde_json::Value) -> Option<Self> {
        if !v.get("enabled").and_then(|b| b.as_bool()).unwrap_or(false) {
            return None;
        }
        let logs_url = v.get("logs_url")?.as_str()?.to_string();
        let traces_url = v
            .get("traces_url")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        let headers = v
            .get("headers")
            .and_then(|h| h.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|kv| {
                        let k = kv.get(0)?.as_str()?;
                        let v = kv.get(1)?.as_str()?;
                        Some((k.to_string(), v.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let bearer = v
            .get("bearer")
            .and_then(|b| b.as_str())
            .map(|s| SecretString::from(s.to_string()));
        Some(Self {
            logs_url,
            traces_url,
            headers,
            bearer,
        })
    }
}

#[derive(Clone, Default, Debug, serde::Serialize)]
pub struct Stats {
    pub sent: u64,
    pub failed: u64,
    pub spooled: u64,
    pub spool_bytes: u64,
    pub last_sent_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_error: Option<String>,
}

const BATCH: usize = 500;
const FLUSH_EVERY: std::time::Duration = std::time::Duration::from_secs(5);
const SPOOL_MAX: u64 = 20 * 1024 * 1024;
/// Quantos itens guardamos em memória enquanto a telemetria ainda não foi
/// configurada (o início do processo chega antes das configurações).
const PRE_CONFIG_KEEP: usize = 1000;

/// Receptores guardados entre `Telemetry::layer` (sem runtime) e `Handle::start`.
pub(crate) struct Pending;
type PendingPair = (mpsc::Receiver<Item>, watch::Receiver<Option<ShipConfig>>);
static PENDING: OnceLock<StdMutex<Option<PendingPair>>> = OnceLock::new();

impl Pending {
    pub(crate) fn stash(rx: mpsc::Receiver<Item>, cfg: watch::Receiver<Option<ShipConfig>>) {
        let cell = PENDING.get_or_init(|| StdMutex::new(None));
        if let Ok(mut g) = cell.lock() {
            *g = Some((rx, cfg));
        }
    }
    pub(crate) fn take() -> Option<PendingPair> {
        PENDING
            .get()
            .and_then(|c| c.lock().ok().and_then(|mut g| g.take()))
    }
}

pub(crate) struct Shipper {
    pub rx: mpsc::Receiver<Item>,
    pub cfg: watch::Receiver<Option<ShipConfig>>,
    pub instance: Arc<InstanceInfo>,
    pub stats: Arc<parking_lot::Mutex<Stats>>,
    pub spool: PathBuf,
}

fn client() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .user_agent(concat!("will-ia/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default()
    })
}

async fn post_proto(cfg: &ShipConfig, url: &str, body: Vec<u8>) -> Result<u16, String> {
    let mut req = client()
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, crate::otlp::CONTENT_TYPE)
        .body(body);
    for (k, v) in &cfg.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(b) = &cfg.bearer {
        req = req.bearer_auth(b.expose_secret());
    }
    let resp = req
        .send()
        .await
        .map_err(|e| crate::redact(&e.to_string()))?;
    let status = resp.status();
    if status.is_success() {
        return Ok(status.as_u16());
    }
    let text = resp.text().await.unwrap_or_default();
    Err(format!(
        "HTTP {}: {}",
        status.as_u16(),
        crate::redact(text.trim())
            .chars()
            .take(300)
            .collect::<String>()
    ))
}

/// Envia um lote de registros de log (usado pelo teste manual).
pub(crate) async fn post(
    cfg: &ShipConfig,
    inst: &InstanceInfo,
    recs: &[Record],
) -> Result<u16, String> {
    post_proto(cfg, &cfg.logs_url, crate::otlp::encode_logs(inst, recs)).await
}

impl Shipper {
    pub(crate) async fn run(mut self) {
        let mut pre: std::collections::VecDeque<Item> = std::collections::VecDeque::new();
        let mut batch: Vec<Item> = Vec::new();
        let mut tick = tokio::time::interval(FLUSH_EVERY);
        let mut drain_tick = tokio::time::interval(std::time::Duration::from_secs(60));
        self.refresh_spool_size();
        loop {
            tokio::select! {
                item = self.rx.recv() => {
                    let Some(item) = item else { break };
                    if self.cfg.borrow().is_none() {
                        pre.push_back(item);
                        if pre.len() > PRE_CONFIG_KEEP {
                            pre.pop_front();
                        }
                        continue;
                    }
                    if !pre.is_empty() {
                        batch.extend(pre.drain(..));
                    }
                    batch.push(item);
                    if batch.len() >= BATCH {
                        self.flush(&mut batch).await;
                    }
                }
                _ = tick.tick() => {
                    if !batch.is_empty() && self.cfg.borrow().is_some() {
                        self.flush(&mut batch).await;
                    }
                }
                _ = drain_tick.tick() => {
                    if self.cfg.borrow().is_some() {
                        self.drain_spool().await;
                    }
                }
                r = self.cfg.changed() => {
                    if r.is_err() { break; }
                    if self.cfg.borrow().is_some() && !pre.is_empty() {
                        batch.extend(pre.drain(..));
                    }
                }
            }
        }
        if !batch.is_empty() {
            self.flush(&mut batch).await;
        }
    }

    async fn flush(&self, batch: &mut Vec<Item>) {
        let items = std::mem::take(batch);
        let Some(cfg) = self.cfg.borrow().clone() else {
            return;
        };
        match self.send(&cfg, &items).await {
            Ok(()) => {
                let mut s = self.stats.lock();
                s.sent += items.len() as u64;
                s.last_sent_at = Some(chrono::Utc::now());
                s.last_error = None;
            }
            Err(e) => {
                {
                    let mut s = self.stats.lock();
                    s.failed += items.len() as u64;
                    s.last_error = Some(e);
                }
                self.spool_items(&items);
            }
        }
    }

    async fn send(&self, cfg: &ShipConfig, items: &[Item]) -> Result<(), String> {
        let mut logs: Vec<Record> = Vec::new();
        let mut spans: Vec<SpanRecord> = Vec::new();
        for it in items {
            match it {
                Item::Log(r) => logs.push(r.clone()),
                Item::Span(s) => spans.push(s.clone()),
            }
        }
        if !logs.is_empty() {
            post_proto(
                cfg,
                &cfg.logs_url,
                crate::otlp::encode_logs(&self.instance, &logs),
            )
            .await?;
        }
        if let (Some(url), false) = (&cfg.traces_url, spans.is_empty()) {
            post_proto(cfg, url, crate::otlp::encode_traces(&self.instance, &spans)).await?;
        }
        Ok(())
    }

    fn spool_items(&self, items: &[Item]) {
        let cur = std::fs::metadata(&self.spool).map(|m| m.len()).unwrap_or(0);
        if cur > SPOOL_MAX {
            return;
        }
        let mut buf = String::new();
        for it in items {
            if let Ok(line) = serde_json::to_string(it) {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        if let Some(parent) = self.spool.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        use std::io::Write;
        let ok = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.spool)
            .and_then(|mut f| f.write_all(buf.as_bytes()))
            .is_ok();
        if ok {
            self.stats.lock().spooled += items.len() as u64;
        }
        self.refresh_spool_size();
    }

    fn refresh_spool_size(&self) {
        let len = std::fs::metadata(&self.spool).map(|m| m.len()).unwrap_or(0);
        self.stats.lock().spool_bytes = len;
    }

    async fn drain_spool(&self) {
        let Ok(text) = std::fs::read_to_string(&self.spool) else {
            return;
        };
        if text.trim().is_empty() {
            return;
        }
        let Some(cfg) = self.cfg.borrow().clone() else {
            return;
        };
        let items: Vec<Item> = text
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let mut sent = 0u64;
        for chunk in items.chunks(BATCH) {
            if let Err(e) = self.send(&cfg, chunk).await {
                self.stats.lock().last_error = Some(e);
                break;
            }
            sent += chunk.len() as u64;
        }
        if sent as usize >= items.len() {
            let _ = std::fs::remove_file(&self.spool);
            let mut s = self.stats.lock();
            s.sent += sent;
            s.spooled = 0;
            s.last_sent_at = Some(chrono::Utc::now());
        } else if sent > 0 {
            // Reescreve só o que faltou.
            let rest: Vec<String> = items[sent as usize..]
                .iter()
                .filter_map(|i| serde_json::to_string(i).ok())
                .collect();
            let _ = std::fs::write(&self.spool, rest.join("\n") + "\n");
            let mut s = self.stats.lock();
            s.sent += sent;
            s.spooled = s.spooled.saturating_sub(sent);
        }
        self.refresh_spool_size();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Servidor HTTP mínimo: lê uma requisição, devolve cabeçalhos + corpo.
    async fn fake_otlp() -> (String, tokio::task::JoinHandle<(String, Vec<u8>)>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = l.local_addr().expect("addr");
        let h = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            loop {
                let n = s.read(&mut tmp).await.expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..p]).to_string();
                    let len: usize = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    while buf.len() < p + 4 + len {
                        let n = s.read(&mut tmp).await.expect("read body");
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body = buf[p + 4..p + 4 + len].to_vec();
                    s.write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await
                    .expect("write");
                    return (head, body);
                }
            }
            (String::new(), Vec::new())
        });
        (format!("http://{addr}"), h)
    }

    #[tokio::test]
    async fn posts_otlp_protobuf_with_headers() {
        let (base, server) = fake_otlp().await;
        let inst = InstanceInfo::new("inst-1".into(), "server", "1.2.3");
        inst.set_label("mac-teste");
        let cfg = ShipConfig {
            logs_url: format!("{base}/v1/logs"),
            traces_url: None,
            headers: vec![("X-Axiom-Dataset".into(), "will-ia".into())],
            bearer: Some(SecretString::from("xaat-segredo".to_string())),
        };
        let mut rec = crate::layer::base_record(tracing::Level::WARN, "will_server::x", "oi");
        rec.insert("thread_id".into(), "t-1".into());
        let status = post(&cfg, &inst, &[rec]).await.expect("post");
        assert_eq!(status, 200);
        let (head, body) = server.await.expect("server");
        assert!(head.starts_with("POST /v1/logs HTTP/1.1"));
        let lower = head.to_ascii_lowercase();
        assert!(lower.contains("content-type: application/x-protobuf"));
        assert!(lower.contains("x-axiom-dataset: will-ia"));
        assert!(lower.contains("authorization: bearer xaat-segredo"));
        let req =
            opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest::decode(
                body.as_slice(),
            )
            .expect("protobuf válido");
        let rl = &req.resource_logs[0];
        let res = rl.resource.as_ref().expect("resource");
        let get = |k: &str| {
            res.attributes
                .iter()
                .find(|a| a.key == k)
                .and_then(|a| a.value.as_ref())
                .and_then(|v| match &v.value {
                    Some(
                        opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(s),
                    ) => Some(s.clone()),
                    _ => None,
                })
        };
        assert_eq!(get("service.instance.id").as_deref(), Some("inst-1"));
        assert_eq!(get("label").as_deref(), Some("mac-teste"));
        assert_eq!(get("service.version").as_deref(), Some("1.2.3"));
        let lr = &rl.scope_logs[0].log_records[0];
        assert_eq!(lr.severity_text, "WARN");
        assert!(lr.attributes.iter().any(|a| a.key == "thread_id"));
        assert!(lr.attributes.iter().any(|a| a.key == "module"));
    }

    #[tokio::test]
    async fn client_config_round_trip() {
        let v = serde_json::json!({
            "enabled": true, "logs_url": "http://x/v1/logs", "traces_url": null,
            "headers": [["api-key", "k"]], "bearer": "b"
        });
        let c = ShipConfig::from_client_config(&v).expect("cfg");
        assert_eq!(c.headers, vec![("api-key".to_string(), "k".to_string())]);
        assert!(c.traces_url.is_none());
        assert!(c.bearer.is_some());
        assert!(ShipConfig::from_client_config(&serde_json::json!({ "enabled": false })).is_none());
    }
}
