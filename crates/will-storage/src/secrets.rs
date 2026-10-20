//! Armazenamento de segredos. Preferência: keyring do SO. Fallback: arquivo
//! JSON 0600 no diretório de dados (com aviso), para ambientes sem Secret Service.

use secrecy::{ExposeSecret, SecretString};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

const SERVICE: &str = "will-ia";

#[async_trait::async_trait]
pub trait SecretStore: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<SecretString>, String>;
    async fn set(&self, key: &str, value: &SecretString) -> Result<(), String>;
    async fn delete(&self, key: &str) -> Result<(), String>;
    /// Descrição do backend em uso (para a UI).
    fn backend(&self) -> &'static str;
}

pub struct KeyringStore;

#[async_trait::async_trait]
impl SecretStore for KeyringStore {
    async fn get(&self, key: &str) -> Result<Option<SecretString>, String> {
        let key = key.to_string();
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(SERVICE, &key).map_err(|e| e.to_string())?;
            match entry.get_password() {
                Ok(p) => Ok(Some(SecretString::from(p))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(e) => Err(e.to_string()),
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }
    async fn set(&self, key: &str, value: &SecretString) -> Result<(), String> {
        let key = key.to_string();
        let value = value.expose_secret().to_string();
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(SERVICE, &key).map_err(|e| e.to_string())?;
            entry.set_password(&value).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
    async fn delete(&self, key: &str) -> Result<(), String> {
        let key = key.to_string();
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(SERVICE, &key).map_err(|e| e.to_string())?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }
    fn backend(&self) -> &'static str {
        "keyring"
    }
}

/// Fallback em arquivo (0600). Só usado quando o keyring não está disponível.
pub struct FileStore {
    path: PathBuf,
    cache: Mutex<Option<BTreeMap<String, String>>>,
}

impl FileStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cache: Mutex::new(None),
        }
    }
    async fn load(&self) -> Result<BTreeMap<String, String>, String> {
        let mut guard = self.cache.lock().await;
        if let Some(m) = guard.as_ref() {
            return Ok(m.clone());
        }
        let m = match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.to_string()),
        };
        *guard = Some(m.clone());
        Ok(m)
    }
    async fn save(&self, m: BTreeMap<String, String>) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(&m).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        *self.cache.lock().await = Some(m);
        Ok(())
    }
}

#[async_trait::async_trait]
impl SecretStore for FileStore {
    async fn get(&self, key: &str) -> Result<Option<SecretString>, String> {
        Ok(self
            .load()
            .await?
            .get(key)
            .map(|v| SecretString::from(v.clone())))
    }
    async fn set(&self, key: &str, value: &SecretString) -> Result<(), String> {
        let mut m = self.load().await?;
        m.insert(key.to_string(), value.expose_secret().to_string());
        self.save(m).await
    }
    async fn delete(&self, key: &str) -> Result<(), String> {
        let mut m = self.load().await?;
        m.remove(key);
        self.save(m).await
    }
    fn backend(&self) -> &'static str {
        "file"
    }
}

/// Escolhe keyring se funcionar; senão, arquivo. A decisão é feita uma vez.
pub struct AutoSecretStore {
    inner: Arc<dyn SecretStore>,
}

impl AutoSecretStore {
    pub async fn new(fallback_file: PathBuf, force_file: bool) -> Self {
        if !force_file {
            let probe = KeyringStore;
            // Um get de chave inexistente testa a disponibilidade do store.
            match probe.get("__probe__").await {
                Ok(_) => {
                    return Self {
                        inner: Arc::new(probe),
                    };
                }
                Err(e) => {
                    tracing::warn!(error = %e, "keyring indisponível; usando arquivo de segredos (0600)");
                }
            }
        }
        Self {
            inner: Arc::new(FileStore::new(fallback_file)),
        }
    }
    pub fn file_only(path: PathBuf) -> Self {
        Self {
            inner: Arc::new(FileStore::new(path)),
        }
    }
}

#[async_trait::async_trait]
impl SecretStore for AutoSecretStore {
    async fn get(&self, key: &str) -> Result<Option<SecretString>, String> {
        self.inner.get(key).await
    }
    async fn set(&self, key: &str, value: &SecretString) -> Result<(), String> {
        self.inner.set(key, value).await
    }
    async fn delete(&self, key: &str) -> Result<(), String> {
        self.inner.delete(key).await
    }
    fn backend(&self) -> &'static str {
        self.inner.backend()
    }
}

/// Mascara um segredo para logs/UI: só os 4 últimos caracteres.
pub fn mask(secret: &str) -> String {
    if secret.len() < 8 {
        "(vazio)".into()
    } else {
        format!("…{}", &secret[secret.len() - 4..])
    }
}
