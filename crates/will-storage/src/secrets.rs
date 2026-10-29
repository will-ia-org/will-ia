//! Armazenamento de segredos.
//!
//! - **macOS** (ADR-0024): `EncryptedFileStore` — os segredos ficam em **um**
//!   arquivo cifrado (`secrets.enc`, XChaCha20-Poly1305) e só a **chave-mestra**
//!   mora no Keychain, como um único item. Com um item por segredo (modelo
//!   antigo), cada binário novo disparava um diálogo "quer usar suas informações
//!   confidenciais" **por item** (20+ de uma vez). Itens antigos migram sozinhos
//!   na primeira leitura e são apagados do Keychain.
//! - **Linux/Windows**: `KeyringStore`, um item por segredo (Secret Service /
//!   Credential Manager não amarram ACL à assinatura do binário).
//! - Fallback: arquivo JSON 0600 no diretório de dados (com aviso), para
//!   ambientes sem keyring.

use base64::prelude::*;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use secrecy::{ExposeSecret, SecretString};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::Mutex;

const SERVICE: &str = "will-ia";
/// Usuário do único item do keyring que o cofre cifrado usa.
const MASTER_KEY_USER: &str = "master-key";
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;

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

// ---------------------------------------------------------------- cofre cifrado

/// Origem da chave-mestra do cofre cifrado (injetável nos testes).
#[async_trait::async_trait]
pub trait MasterKeySource: Send + Sync {
    /// Devolve a chave de 32 bytes, criando-a na primeira vez.
    async fn load_or_create(&self) -> Result<[u8; KEY_LEN], String>;
}

/// Chave-mestra como **um só** item do keyring (`will-ia` / `master-key`), em base64.
pub struct KeyringMasterKey;

#[async_trait::async_trait]
impl MasterKeySource for KeyringMasterKey {
    async fn load_or_create(&self) -> Result<[u8; KEY_LEN], String> {
        tokio::task::spawn_blocking(|| {
            let entry = keyring::Entry::new(SERVICE, MASTER_KEY_USER).map_err(|e| e.to_string())?;
            match entry.get_password() {
                Ok(b64) => {
                    let bytes = BASE64_STANDARD
                        .decode(b64.trim())
                        .map_err(|e| format!("chave-mestra inválida no keyring: {e}"))?;
                    bytes
                        .try_into()
                        .map_err(|_| "chave-mestra no keyring com tamanho errado".to_string())
                }
                Err(keyring::Error::NoEntry) => {
                    let key: [u8; KEY_LEN] = rand::random();
                    entry
                        .set_password(&BASE64_STANDARD.encode(key))
                        .map_err(|e| format!("não consegui guardar a chave-mestra: {e}"))?;
                    Ok(key)
                }
                Err(e) => Err(e.to_string()),
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

/// Envelope gravado em disco. `ct` é o JSON `{chave: valor}` cifrado.
#[derive(serde::Serialize, serde::Deserialize)]
struct Envelope {
    v: u32,
    alg: String,
    nonce: String,
    ct: String,
}

const ENVELOPE_V: u32 = 1;
const ENVELOPE_ALG: &str = "xchacha20poly1305";

/// Segredos num único arquivo cifrado; chave-mestra fora do arquivo (keyring).
/// `legacy`: cofre antigo (um item por segredo) de onde os valores migram na
/// primeira leitura — e de onde são apagados em seguida.
pub struct EncryptedFileStore {
    path: PathBuf,
    master: Arc<dyn MasterKeySource>,
    legacy: Option<Arc<dyn SecretStore>>,
    key: Mutex<Option<[u8; KEY_LEN]>>,
    cache: Mutex<Option<BTreeMap<String, String>>>,
}

impl EncryptedFileStore {
    pub fn new(
        path: PathBuf,
        master: Arc<dyn MasterKeySource>,
        legacy: Option<Arc<dyn SecretStore>>,
    ) -> Self {
        Self {
            path,
            master,
            legacy,
            key: Mutex::new(None),
            cache: Mutex::new(None),
        }
    }

    async fn key(&self) -> Result<[u8; KEY_LEN], String> {
        let mut g = self.key.lock().await;
        if let Some(k) = *g {
            return Ok(k);
        }
        let k = self.master.load_or_create().await?;
        *g = Some(k);
        Ok(k)
    }

    async fn load(&self) -> Result<BTreeMap<String, String>, String> {
        let mut guard = self.cache.lock().await;
        if let Some(m) = guard.as_ref() {
            return Ok(m.clone());
        }
        let m = match std::fs::read(&self.path) {
            Ok(bytes) => {
                let key = self.key().await?;
                decrypt_map(&key, &bytes).map_err(|e| format!("{}: {e}", self.path.display()))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(format!("{}: {e}", self.path.display())),
        };
        *guard = Some(m.clone());
        Ok(m)
    }

    async fn save(&self, m: BTreeMap<String, String>) -> Result<(), String> {
        let key = self.key().await?;
        let bytes = encrypt_map(&key, &m)?;
        write_private(&self.path, &bytes)?;
        *self.cache.lock().await = Some(m);
        Ok(())
    }
}

fn encrypt_map(key: &[u8; KEY_LEN], m: &BTreeMap<String, String>) -> Result<Vec<u8>, String> {
    let plain = serde_json::to_vec(m).map_err(|e| e.to_string())?;
    let nonce: [u8; NONCE_LEN] = rand::random();
    let cipher = XChaCha20Poly1305::new(key.into());
    let ct = cipher
        .encrypt(&XNonce::from(nonce), plain.as_slice())
        .map_err(|_| "falha ao cifrar o cofre".to_string())?;
    let env = Envelope {
        v: ENVELOPE_V,
        alg: ENVELOPE_ALG.into(),
        nonce: BASE64_STANDARD.encode(nonce),
        ct: BASE64_STANDARD.encode(ct),
    };
    serde_json::to_vec_pretty(&env).map_err(|e| e.to_string())
}

fn decrypt_map(key: &[u8; KEY_LEN], bytes: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let env: Envelope = serde_json::from_slice(bytes)
        .map_err(|e| format!("cofre cifrado ilegível (não é um envelope válido): {e}"))?;
    if env.v != ENVELOPE_V || env.alg != ENVELOPE_ALG {
        return Err(format!(
            "cofre cifrado em formato desconhecido (v{} {})",
            env.v, env.alg
        ));
    }
    let nonce = BASE64_STANDARD
        .decode(env.nonce)
        .map_err(|e| format!("nonce inválido: {e}"))?;
    let nonce: [u8; NONCE_LEN] = nonce
        .try_into()
        .map_err(|_| "nonce com tamanho errado".to_string())?;
    let ct = BASE64_STANDARD
        .decode(env.ct)
        .map_err(|e| format!("conteúdo inválido: {e}"))?;
    let cipher = XChaCha20Poly1305::new(key.into());
    let plain = cipher
        .decrypt(&XNonce::from(nonce), ct.as_slice())
        .map_err(|_| {
            "não foi possível decifrar o cofre (chave-mestra diferente ou arquivo corrompido)"
                .to_string()
        })?;
    serde_json::from_slice(&plain).map_err(|e| format!("conteúdo do cofre inválido: {e}"))
}

/// Grava com permissão 0600 e troca atômica (tmp + rename).
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("enc.tmp");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[async_trait::async_trait]
impl SecretStore for EncryptedFileStore {
    async fn get(&self, key: &str) -> Result<Option<SecretString>, String> {
        let m = self.load().await?;
        if let Some(v) = m.get(key) {
            return Ok(Some(SecretString::from(v.clone())));
        }
        // Migração preguiçosa: item antigo (um por segredo) → arquivo, e some do keyring.
        let Some(legacy) = &self.legacy else {
            return Ok(None);
        };
        let Some(v) = legacy.get(key).await? else {
            return Ok(None);
        };
        let mut m = m;
        m.insert(key.to_string(), v.expose_secret().to_string());
        self.save(m).await?;
        if let Err(e) = legacy.delete(key).await {
            tracing::warn!(key, error = %e, "segredo migrado, mas o item antigo do keyring ficou");
        } else {
            tracing::info!(key, "segredo migrado do keyring para o cofre cifrado");
        }
        Ok(Some(v))
    }
    async fn set(&self, key: &str, value: &SecretString) -> Result<(), String> {
        let mut m = self.load().await?;
        m.insert(key.to_string(), value.expose_secret().to_string());
        self.save(m).await
    }
    async fn delete(&self, key: &str) -> Result<(), String> {
        let mut m = self.load().await?;
        m.remove(key);
        self.save(m).await?;
        if let Some(legacy) = &self.legacy {
            let _ = legacy.delete(key).await;
        }
        Ok(())
    }
    fn backend(&self) -> &'static str {
        "keychain + arquivo cifrado"
    }
}

/// Escolhe o backend uma vez: macOS → cofre cifrado (chave-mestra no Keychain);
/// outros SOs → keyring por item; sem keyring → arquivo JSON 0600 (com aviso).
pub struct AutoSecretStore {
    inner: Arc<dyn SecretStore>,
}

impl AutoSecretStore {
    pub async fn new(fallback_file: PathBuf, encrypted_file: PathBuf, force_file: bool) -> Self {
        if !force_file {
            if cfg!(target_os = "macos") {
                // Um único diálogo do Keychain (a chave-mestra), em vez de um por segredo.
                let master = KeyringMasterKey;
                match master.load_or_create().await {
                    Ok(_) => {
                        return Self {
                            inner: Arc::new(EncryptedFileStore::new(
                                encrypted_file,
                                Arc::new(master),
                                Some(Arc::new(KeyringStore)),
                            )),
                        };
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "chave-mestra no Keychain indisponível; usando arquivo de segredos (0600)");
                    }
                }
            } else {
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

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedKey([u8; KEY_LEN]);
    #[async_trait::async_trait]
    impl MasterKeySource for FixedKey {
        async fn load_or_create(&self) -> Result<[u8; KEY_LEN], String> {
            Ok(self.0)
        }
    }

    /// Cofre legado falso (um "item" por segredo), para testar a migração.
    struct FakeLegacy(Mutex<BTreeMap<String, String>>);
    #[async_trait::async_trait]
    impl SecretStore for FakeLegacy {
        async fn get(&self, key: &str) -> Result<Option<SecretString>, String> {
            Ok(self
                .0
                .lock()
                .await
                .get(key)
                .map(|v| SecretString::from(v.clone())))
        }
        async fn set(&self, key: &str, value: &SecretString) -> Result<(), String> {
            self.0
                .lock()
                .await
                .insert(key.into(), value.expose_secret().into());
            Ok(())
        }
        async fn delete(&self, key: &str) -> Result<(), String> {
            self.0.lock().await.remove(key);
            Ok(())
        }
        fn backend(&self) -> &'static str {
            "fake"
        }
    }

    fn store(dir: &Path, legacy: Option<Arc<dyn SecretStore>>) -> EncryptedFileStore {
        EncryptedFileStore::new(
            dir.join("secrets.enc"),
            Arc::new(FixedKey([7u8; KEY_LEN])),
            legacy,
        )
    }

    #[tokio::test]
    async fn round_trip_set_get_delete() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), None);
        assert!(s.get("a").await.unwrap().is_none());
        s.set("a", &SecretString::from("segredo-1")).await.unwrap();
        s.set("b", &SecretString::from("segredo-2")).await.unwrap();
        assert_eq!(
            s.get("a").await.unwrap().unwrap().expose_secret(),
            "segredo-1"
        );
        // outra instância (cache frio) lê o mesmo arquivo
        let s2 = store(dir.path(), None);
        assert_eq!(
            s2.get("b").await.unwrap().unwrap().expose_secret(),
            "segredo-2"
        );
        s2.delete("a").await.unwrap();
        assert!(store(dir.path(), None).get("a").await.unwrap().is_none());
        // o arquivo não contém o segredo em claro
        let raw = std::fs::read_to_string(dir.path().join("secrets.enc")).unwrap();
        assert!(!raw.contains("segredo-2"));
        assert!(raw.contains("xchacha20poly1305"));
    }

    #[tokio::test]
    async fn wrong_key_or_corrupted_file_is_a_clear_error() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), None)
            .set("a", &SecretString::from("x"))
            .await
            .unwrap();
        let other = EncryptedFileStore::new(
            dir.path().join("secrets.enc"),
            Arc::new(FixedKey([9u8; KEY_LEN])),
            None,
        );
        let e = other.get("a").await.unwrap_err();
        assert!(e.contains("decifrar"), "{e}");
        std::fs::write(dir.path().join("secrets.enc"), b"lixo").unwrap();
        let e = store(dir.path(), None).get("a").await.unwrap_err();
        assert!(e.contains("ilegível"), "{e}");
    }

    #[tokio::test]
    async fn migrates_legacy_items_once_and_deletes_them() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = Arc::new(FakeLegacy(Mutex::new(BTreeMap::from([(
            "tok".to_string(),
            "antigo".to_string(),
        )]))));
        let s = store(dir.path(), Some(legacy.clone()));
        assert_eq!(
            s.get("tok").await.unwrap().unwrap().expose_secret(),
            "antigo"
        );
        assert!(
            legacy.0.lock().await.get("tok").is_none(),
            "item antigo deve sumir"
        );
        // já está no arquivo: uma instância sem legado também lê
        assert_eq!(
            store(dir.path(), None)
                .get("tok")
                .await
                .unwrap()
                .unwrap()
                .expose_secret(),
            "antigo"
        );
        assert!(s.get("nada").await.unwrap().is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), None)
            .set("a", &SecretString::from("x"))
            .await
            .unwrap();
        let mode = std::fs::metadata(dir.path().join("secrets.enc"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
