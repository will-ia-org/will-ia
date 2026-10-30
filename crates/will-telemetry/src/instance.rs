//! Identidade da instância: id persistido em disco + host/SO/versão/processo.

use std::path::Path;

#[derive(Debug)]
pub struct InstanceInfo {
    pub instance_id: String,
    pub host: String,
    pub os: &'static str,
    pub arch: &'static str,
    pub version: &'static str,
    /// `server` | `app` | `setup`.
    pub process: &'static str,
    pub label: parking_lot::RwLock<String>,
    /// Id da instância do servidor, quando este processo é o app (correlação).
    pub peer: parking_lot::RwLock<Option<String>>,
    /// Atributos extras do resource (ex.: `net.public_ip`).
    pub extra: parking_lot::RwLock<std::collections::BTreeMap<String, String>>,
    pub started_at: std::time::Instant,
}

impl InstanceInfo {
    pub fn new(instance_id: String, process: &'static str, version: &'static str) -> Self {
        Self {
            instance_id,
            host: gethostname::gethostname().to_string_lossy().into_owned(),
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            version,
            process,
            label: parking_lot::RwLock::new(String::new()),
            peer: parking_lot::RwLock::new(None),
            extra: parking_lot::RwLock::new(Default::default()),
            started_at: std::time::Instant::now(),
        }
    }

    pub fn set_label(&self, label: &str) {
        *self.label.write() = label.trim().to_string();
    }

    pub fn set_extra(&self, key: &str, value: Option<String>) {
        let mut e = self.extra.write();
        match value.filter(|v| !v.is_empty()) {
            Some(v) => {
                e.insert(key.to_string(), v);
            }
            None => {
                e.remove(key);
            }
        }
    }

    pub fn set_peer(&self, id: Option<String>) {
        *self.peer.write() = id.filter(|s| !s.is_empty());
    }

    pub fn uptime_s(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Acrescenta os campos fixos a um evento.
    pub fn decorate(&self, rec: &mut crate::Record) {
        rec.insert("instance_id".into(), self.instance_id.clone().into());
        rec.insert("host".into(), self.host.clone().into());
        rec.insert("os".into(), self.os.into());
        rec.insert("arch".into(), self.arch.into());
        rec.insert("version".into(), self.version.into());
        rec.insert("process".into(), self.process.into());
        let label = self.label.read();
        if !label.is_empty() {
            rec.insert("label".into(), label.clone().into());
        }
        if let Some(p) = self.peer.read().as_ref() {
            rec.insert("server_instance_id".into(), p.clone().into());
        }
        for (k, v) in self.extra.read().iter() {
            rec.insert(k.clone(), v.clone().into());
        }
    }
}

/// Lê (ou cria) o id da instância em `<data_dir>/instance-id` — um UUID v4
/// estável entre reinícios e versões, compartilhado por servidor e app.
pub fn instance_id(data_dir: &Path) -> String {
    let path = data_dir.join("instance-id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim();
        if uuid::Uuid::parse_str(s).is_ok() {
            return s.to_string();
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = std::fs::create_dir_all(data_dir);
    let _ = std::fs::write(&path, &id);
    id
}

/// IP público desta máquina (texto puro de serviços públicos), com timeout
/// curto; `None` sem rede. Usado como rótulo padrão da instância.
pub async fn detect_public_ip() -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    for url in [
        "https://api.ipify.org",
        "https://checkip.amazonaws.com",
        "https://ifconfig.me/ip",
    ] {
        if let Ok(r) = client.get(url).send().await {
            if let Ok(t) = r.text().await {
                let t = t.trim();
                if !t.is_empty() && t.len() < 64 && t.parse::<std::net::IpAddr>().is_ok() {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}
