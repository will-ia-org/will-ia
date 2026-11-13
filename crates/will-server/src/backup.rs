//! Backup / Restore da **pasta de dados** da instância (`docs/10`, RF-CFG-11).
//!
//! O backup é um `.zip` com um snapshot consistente do banco (`VACUUM INTO`),
//! o cofre de segredos (`secrets.enc` + `secrets.key`, ou `secrets.json`), a
//! sessão do WhatsApp, `telemetry-defaults.json`, `instance-id`, `app.ron` e,
//! opcionalmente, o perfil do navegador. **Não** leva a base de conhecimento
//! (workspace), logs, caches, chaves de assinatura do instalador nem arquivos
//! da máquina (`api-port`, `instance.lock`, `install-manifest.json`,
//! `ui-session.token`).
//!
//! Restaurar = subir o `.zip` (`POST /api/restore`): ele é validado e fica como
//! `restore-pending.zip` na pasta de dados; o serviço se encerra e, ao voltar
//! (LaunchAgent/systemd com KeepAlive), [`apply_pending`] guarda os dados atuais
//! em `old/restore-<data>/` e extrai o arquivo **antes** de abrir o banco.

use crate::app::AppState;
use chrono::{DateTime, Utc};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use will_core::paths::AppPaths;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

pub const PENDING: &str = "restore-pending.zip";
pub const MANIFEST: &str = "backup-manifest.json";
const FORMAT: u32 = 1;
const DB_NAME: &str = "will-ia.db";
/// Arquivos de 1º nível que entram no backup (se existirem). A **chave do cofre**
/// (`secrets.key`) e o `secrets.json` em texto ficam de fora: o zip sozinho não
/// abre os segredos; quem restaura informa a chave (Backup › Chave do cofre).
const TOP_FILES: &[&str] = &[
    "secrets.enc",
    "telemetry-defaults.json",
    "instance-id",
    "app.ron",
];
/// Arquivos desta máquina que a restauração também tira do caminho (vão para
/// `old/restore-*/`), além dos de `TOP_FILES`. Backups antigos ainda os trazem.
const RESTORE_ONLY: &[&str] = &["secrets.key", "secrets.json"];
const VAULT: &str = "secrets.enc";
const VAULT_KEY: &str = "secrets.key";
/// Chave do cofre informada na restauração, à espera de [`apply_pending`].
pub const PENDING_KEY: &str = "restore-vault.key";
/// Pasta opcional: sessão do WhatsApp (pareamento). Levar para outra máquina
/// com a origem ainda ligada põe as duas disputando a mesma sessão.
pub const WHATSAPP_DIR: &str = "whatsapp";
/// Pasta opcional (grande): perfil do Chrome do agent-browser.
pub const BROWSER_DIR: &str = "browser-profile";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Manifest {
    pub format: u32,
    pub version: String,
    pub created_at: DateTime<Utc>,
    pub hostname: String,
    pub instance_id: Option<String>,
    pub include_browser: bool,
    /// Backups anteriores a esta opção sempre levavam a sessão.
    #[serde(default = "yes")]
    pub include_whatsapp: bool,
    pub files: usize,
    pub bytes: u64,
}

fn yes() -> bool {
    true
}

fn now_tag() -> String {
    Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

/// Nome sugerido para o arquivo baixado.
pub fn suggested_name() -> String {
    format!("will-ia-backup-{}.zip", Utc::now().format("%Y%m%d-%H%M"))
}

/// Tamanho aproximado (bytes) de uma pasta; para a UI dizer quanto pesa o perfil do navegador.
pub fn dir_size(p: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                total += dir_size(&e.path());
            } else if md.is_file() {
                total += md.len();
            }
        }
    }
    total
}

/// Gera o zip em `cache_dir` e devolve o caminho + manifesto.
pub async fn build(
    state: &AppState,
    include_browser: bool,
    include_whatsapp: bool,
) -> Result<(PathBuf, Manifest), String> {
    let paths = state.paths.clone();
    fs::create_dir_all(&paths.cache_dir).map_err(|e| e.to_string())?;
    cleanup_old(&paths.cache_dir);
    let tag = now_tag();
    let snap = paths.cache_dir.join(format!("backup-db-{tag}.db"));
    let _ = fs::remove_file(&snap);
    // snapshot consistente do SQLite (inclui o que ainda está no WAL)
    state
        .storage
        .snapshot_to(&snap)
        .await
        .map_err(|e| format!("snapshot do banco: {e}"))?;
    let out = paths.cache_dir.join(format!("backup-{tag}.zip"));
    let instance_id = fs::read_to_string(paths.data_dir.join("instance-id"))
        .ok()
        .map(|s| s.trim().to_string());
    let data_dir = paths.data_dir.clone();
    let out2 = out.clone();
    let snap2 = snap.clone();
    let manifest = tokio::task::spawn_blocking(move || {
        write_zip(
            &data_dir,
            &snap2,
            &out2,
            include_browser,
            include_whatsapp,
            instance_id,
        )
    })
    .await
    .map_err(|e| e.to_string())??;
    let _ = fs::remove_file(&snap);
    tracing::info!(path = %out.display(), files = manifest.files, bytes = manifest.bytes, include_browser, include_whatsapp, "backup gerado");
    Ok((out, manifest))
}

fn write_zip(
    data_dir: &Path,
    snap: &Path,
    out: &Path,
    include_browser: bool,
    include_whatsapp: bool,
    instance_id: Option<String>,
) -> Result<Manifest, String> {
    let file = fs::File::create(out).map_err(|e| e.to_string())?;
    let mut zip = ZipWriter::new(std::io::BufWriter::new(file));
    let mut files = 0usize;
    let mut bytes = 0u64;
    add_file(&mut zip, snap, DB_NAME, &mut files, &mut bytes)?;
    for name in TOP_FILES {
        let p = data_dir.join(name);
        if p.is_file() {
            add_file(&mut zip, &p, name, &mut files, &mut bytes)?;
        }
    }
    let mut dirs: Vec<&str> = Vec::new();
    if include_whatsapp {
        dirs.push(WHATSAPP_DIR);
    }
    if include_browser {
        dirs.push(BROWSER_DIR);
    }
    for d in dirs {
        let p = data_dir.join(d);
        if p.is_dir() {
            add_dir(&mut zip, &p, d, &mut files, &mut bytes)?;
        }
    }
    let manifest = Manifest {
        format: FORMAT,
        version: env!("CARGO_PKG_VERSION").to_string(),
        created_at: Utc::now(),
        hostname: gethostname::gethostname().to_string_lossy().to_string(),
        instance_id,
        include_browser,
        include_whatsapp,
        files,
        bytes,
    };
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file(MANIFEST, opts).map_err(|e| e.to_string())?;
    zip.write_all(
        serde_json::to_string_pretty(&manifest)
            .map_err(|e| e.to_string())?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    zip.finish().map_err(|e| e.to_string())?;
    Ok(manifest)
}

fn add_file<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    path: &Path,
    name: &str,
    files: &mut usize,
    bytes: &mut u64,
) -> Result<(), String> {
    let md = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    if md.len() >= 3 * 1024 * 1024 * 1024 {
        opts = opts.large_file(true);
    }
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            // arquivo travado (ex.: Chrome aberto): pula, não derruba o backup
            tracing::warn!(path = %path.display(), error = %e, "backup: arquivo pulado");
            return Ok(());
        }
    };
    zip.start_file(name, opts).map_err(|e| e.to_string())?;
    let n = std::io::copy(&mut f, zip).map_err(|e| format!("{}: {e}", path.display()))?;
    *files += 1;
    *bytes += n;
    Ok(())
}

fn add_dir<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    dir: &Path,
    prefix: &str,
    files: &mut usize,
    bytes: &mut u64,
) -> Result<(), String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        let Ok(md) = fs::symlink_metadata(&p) else {
            continue;
        };
        let name = format!("{prefix}/{}", e.file_name().to_string_lossy());
        if md.is_dir() {
            add_dir(zip, &p, &name, files, bytes)?;
        } else if md.is_file() {
            // locks/sockets do Chrome não são arquivos regulares; symlinks ficam de fora
            add_file(zip, &p, &name, files, bytes)?;
        }
    }
    Ok(())
}

/// Apaga backups antigos (> 1 h) gerados no cache.
fn cleanup_old(cache: &Path) {
    let Ok(rd) = fs::read_dir(cache) else { return };
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if !(n.starts_with("backup-") && (n.ends_with(".zip") || n.ends_with(".db"))) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t.elapsed().map(|d| d.as_secs() > 3600).unwrap_or(false))
            .unwrap_or(false);
        if old {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Lê e valida um arquivo de backup: formato conhecido e banco presente.
pub fn inspect(path: &Path) -> Result<Manifest, String> {
    let f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut ar = ZipArchive::new(f).map_err(|e| format!("não é um zip válido: {e}"))?;
    let manifest: Manifest = {
        let mut m = ar
            .by_name(MANIFEST)
            .map_err(|_| format!("não é um backup do will-ia (sem {MANIFEST})"))?;
        let mut s = String::new();
        m.read_to_string(&mut s).map_err(|e| e.to_string())?;
        serde_json::from_str(&s).map_err(|e| format!("manifesto inválido: {e}"))?
    };
    if manifest.format != FORMAT {
        return Err(format!(
            "formato de backup {} não suportado (este serviço entende {FORMAT})",
            manifest.format
        ));
    }
    ar.by_name(DB_NAME)
        .map_err(|_| format!("backup sem {DB_NAME}"))?;
    Ok(manifest)
}

/// Cofre de um backup **sem** a chave dentro: `Some(bytes de secrets.enc)`.
/// `None` = backup antigo (chave junto) ou sem cofre; não precisa de chave.
fn vault_needing_key(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut ar = ZipArchive::new(f).map_err(|e| e.to_string())?;
    if ar.by_name(VAULT_KEY).is_ok() {
        return Ok(None);
    }
    let Ok(mut e) = ar.by_name(VAULT) else {
        return Ok(None);
    };
    let mut buf = Vec::new();
    e.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(Some(buf))
}

/// Decide a chave que abre o cofre do backup: a informada; sem ela, a desta
/// máquina (restaurar um backup daqui mesmo não pede nada).
fn resolve_vault_key(
    paths: &AppPaths,
    zip: &Path,
    given: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(enc) = vault_needing_key(zip)? else {
        return Ok(None);
    };
    let given = given.map(str::trim).filter(|s| !s.is_empty());
    if let Some(k) = given {
        return will_storage::secrets::vault_key_opens(k, &enc)
            .map(|()| Some(k.to_string()))
            .map_err(|_| {
                "a chave do cofre não abre este backup: confira em Backup › Chave do cofre na máquina de origem".to_string()
            });
    }
    if let Ok(local) = fs::read_to_string(paths.master_key_file()) {
        if will_storage::secrets::vault_key_opens(&local, &enc).is_ok() {
            return Ok(Some(local.trim().to_string()));
        }
    }
    Err("este backup precisa da chave do cofre: copie em Backup › Chave do cofre na máquina de origem e informe aqui".into())
}

fn write_0600(p: &Path, body: &[u8]) -> Result<(), String> {
    fs::write(p, body).map_err(|e| format!("{}: {e}", p.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Grava os bytes recebidos como `restore-pending.zip` (depois de validar).
/// `vault_key`: chave do cofre da máquina de origem (o backup não a leva).
pub fn stage(paths: &AppPaths, bytes: &[u8], vault_key: Option<&str>) -> Result<Manifest, String> {
    let part = paths.data_dir.join(format!("{PENDING}.part"));
    fs::write(&part, bytes).map_err(|e| e.to_string())?;
    let checked = inspect(&part).and_then(|m| Ok((m, resolve_vault_key(paths, &part, vault_key)?)));
    let (m, key) = match checked {
        Ok(v) => v,
        Err(e) => {
            let _ = fs::remove_file(&part);
            return Err(e);
        }
    };
    let key_file = paths.data_dir.join(PENDING_KEY);
    let _ = fs::remove_file(&key_file);
    if let Some(k) = key {
        if let Err(e) = write_0600(&key_file, k.as_bytes()) {
            let _ = fs::remove_file(&part);
            return Err(e);
        }
    }
    fs::rename(&part, paths.data_dir.join(PENDING)).map_err(|e| e.to_string())?;
    tracing::info!(files = m.files, bytes = m.bytes, from_host = %m.hostname, created_at = %m.created_at, "restauração agendada; o serviço vai reiniciar");
    Ok(m)
}

/// O que é **desta máquina** e sobrevive à restauração (gravado ao agendar,
/// reaplicado por [`apply_keep`] depois de abrir o banco restaurado): a seção de
/// rede e a conta de quem pediu a restauração. Sem isso, restaurar à distância
/// um backup de outra máquina trancava a porta (rede voltava ao loopback da
/// origem e o usuário logado deixava de existir).
pub const KEEP: &str = "restore-keep.json";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Keep {
    pub network: will_core::settings::NetworkSettings,
    #[serde(default)]
    pub user: Option<KeepUser>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KeepUser {
    pub id: String,
    pub username: String,
    pub name: String,
    pub email: Option<String>,
    pub password_hash: String,
    pub role: String,
    pub must_change_password: bool,
}

/// Grava `restore-keep.json` (0600: leva o hash da senha).
pub fn write_keep(paths: &AppPaths, keep: &Keep) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(keep).map_err(|e| e.to_string())?;
    write_0600(&paths.data_dir.join(KEEP), &body)
}

/// Depois de abrir o banco: reaplica rede e conta guardadas em `restore-keep.json`.
pub async fn apply_keep(paths: &AppPaths, storage: &will_storage::Storage) {
    let p = paths.data_dir.join(KEEP);
    let Ok(raw) = fs::read(&p) else { return };
    let _ = fs::remove_file(&p);
    let keep: Keep = match serde_json::from_slice(&raw) {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(error = %e, "restore-keep.json ilegível; ignorado");
            return;
        }
    };
    match storage.load_settings().await {
        Ok(mut s) => {
            s.network = keep.network;
            match storage.save_settings(&s).await {
                Ok(()) => {
                    tracing::info!(bind = %s.network.bind, tls = s.network.tls, "restauração: rede desta máquina mantida")
                }
                Err(e) => tracing::warn!(error = %e, "restauração: não consegui manter a rede"),
            }
        }
        Err(e) => tracing::warn!(error = %e, "restauração: não li as configurações restauradas"),
    }
    let Some(u) = keep.user else { return };
    let r = match storage.find_user_by_login(&u.username).await {
        Ok(Some(found)) => {
            let a = storage
                .set_password(&found.id, &u.password_hash, u.must_change_password)
                .await;
            let b = storage
                .update_user(&found.id, None, None, Some(&u.role), Some(true))
                .await
                .map(|_| ());
            a.and(b)
        }
        Ok(None) => storage
            .create_user(
                &u.id,
                &u.username,
                &u.name,
                u.email.as_deref(),
                &u.password_hash,
                &u.role,
                u.must_change_password,
            )
            .await
            .map(|_| ()),
        Err(e) => Err(e),
    };
    match r {
        Ok(()) => {
            tracing::info!(user = %u.username, "restauração: conta de quem restaurou mantida")
        }
        Err(e) => {
            tracing::warn!(user = %u.username, error = %e, "restauração: não consegui manter a conta")
        }
    }
}

/// Na subida, antes de abrir o banco: aplica `restore-pending.zip` se existir.
/// `None` = nada pendente; `Some(Err)` = falhou (o arquivo vira `restore-failed-*.zip`).
pub fn apply_pending(paths: &AppPaths) -> Option<Result<Manifest, String>> {
    let pending = paths.data_dir.join(PENDING);
    if !pending.is_file() {
        return None;
    }
    let r = apply(paths, &pending);
    // sobra só quando a restauração falhou antes de usá-la
    let _ = fs::remove_file(paths.data_dir.join(PENDING_KEY));
    match &r {
        Ok(m) => {
            let _ = fs::remove_file(&pending);
            tracing::info!(files = m.files, from_host = %m.hostname, created_at = %m.created_at, "restauração aplicada");
        }
        Err(e) => {
            let failed = paths
                .data_dir
                .join(format!("restore-failed-{}.zip", now_tag()));
            let _ = fs::rename(&pending, &failed);
            tracing::error!(error = %e, kept = %failed.display(), "restauração falhou; dados atuais mantidos");
        }
    }
    Some(r)
}

fn apply(paths: &AppPaths, pending: &Path) -> Result<Manifest, String> {
    let manifest = inspect(pending)?;
    let data = &paths.data_dir;
    let keep = data.join("old").join(format!("restore-{}", now_tag()));
    fs::create_dir_all(&keep).map_err(|e| e.to_string())?;
    // guarda o que vai ser substituído
    let mut targets: Vec<String> = vec![
        DB_NAME.into(),
        format!("{DB_NAME}-wal"),
        format!("{DB_NAME}-shm"),
    ];
    targets.extend(TOP_FILES.iter().map(|s| s.to_string()));
    targets.extend(RESTORE_ONLY.iter().map(|s| s.to_string()));
    if manifest.include_whatsapp {
        targets.push(WHATSAPP_DIR.into());
    }
    if manifest.include_browser {
        targets.push(BROWSER_DIR.into());
    }
    for t in &targets {
        let p = data.join(t);
        if p.exists() {
            fs::rename(&p, keep.join(t)).map_err(|e| format!("guardar {t}: {e}"))?;
        }
    }
    // extrai
    let f = fs::File::open(pending).map_err(|e| e.to_string())?;
    let mut ar = ZipArchive::new(f).map_err(|e| e.to_string())?;
    for i in 0..ar.len() {
        let mut entry = ar.by_index(i).map_err(|e| e.to_string())?;
        let Some(rel) = entry.enclosed_name() else {
            tracing::warn!(
                index = i,
                "restauração: entrada com caminho inválido ignorada"
            );
            continue;
        };
        let rel = if rel == Path::new(MANIFEST) {
            PathBuf::from("restored-manifest.json")
        } else {
            rel
        };
        let dest = data.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = fs::File::create(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        std::io::copy(&mut entry, &mut out).map_err(|e| format!("{}: {e}", dest.display()))?;
        #[cfg(unix)]
        if rel.starts_with("secrets") {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(0o600));
        }
    }
    // backup sem a chave dentro: entra a que foi conferida ao agendar
    let key_file = data.join(PENDING_KEY);
    if key_file.is_file() && !data.join(VAULT_KEY).exists() {
        fs::rename(&key_file, data.join(VAULT_KEY)).map_err(|e| format!("chave do cofre: {e}"))?;
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &Path) -> AppPaths {
        AppPaths {
            data_dir: root.join("data"),
            logs_dir: root.join("data").join("logs"),
            cache_dir: root.join("cache"),
            portable_root: None,
        }
    }

    /// Cria um cofre de verdade em `dir` com um segredo e devolve a chave (base64).
    /// A chave é fixada pelo `seed` (no macOS, sem arquivo, ela viria do Keychain).
    async fn make_vault(dir: &Path, value: &str, seed: u8) -> String {
        use base64::Engine;
        use secrecy::SecretString;
        use will_storage::SecretStore;
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("secrets.key"),
            base64::engine::general_purpose::STANDARD.encode([seed; 32]),
        )
        .unwrap();
        let store = will_storage::AutoSecretStore::new(
            dir.join("secrets.json"),
            dir.join("secrets.enc"),
            dir.join("secrets.key"),
            true,
        )
        .await;
        store
            .set("openai_api_key", &SecretString::from(value.to_string()))
            .await
            .unwrap();
        fs::read_to_string(dir.join("secrets.key")).unwrap()
    }

    #[tokio::test]
    async fn backup_roundtrip_moves_old_and_extracts_new() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        fs::create_dir_all(p.data_dir.join("whatsapp").join("acc")).unwrap();
        fs::create_dir_all(&p.cache_dir).unwrap();
        let key_a = make_vault(&p.data_dir, "segredo-A", 1).await;
        let enc_a = fs::read(p.data_dir.join("secrets.enc")).unwrap();
        fs::write(p.data_dir.join("whatsapp/acc/session"), b"wa-A").unwrap();
        fs::write(p.data_dir.join("logs-should-stay.txt"), b"x").unwrap();
        let snap = p.cache_dir.join("snap.db");
        fs::write(&snap, b"sqlite-A").unwrap();
        let zip_path = p.cache_dir.join("b.zip");
        let m = write_zip(
            &p.data_dir,
            &snap,
            &zip_path,
            false,
            true,
            Some("inst".into()),
        )
        .unwrap();
        assert_eq!(m.format, FORMAT);
        assert!(m.files >= 3, "{m:?}");
        assert_eq!(
            inspect(&zip_path).unwrap().instance_id.as_deref(),
            Some("inst")
        );
        // a chave do cofre não viaja no zip
        let mut ar = ZipArchive::new(fs::File::open(&zip_path).unwrap()).unwrap();
        assert!(ar.by_name("secrets.enc").is_ok());
        assert!(ar.by_name("secrets.key").is_err());
        assert!(ar.by_name("secrets.json").is_err());
        drop(ar);

        // "outra máquina": cofre e dados diferentes, restaura por cima
        fs::remove_file(p.data_dir.join("secrets.enc")).unwrap();
        fs::remove_file(p.data_dir.join("secrets.key")).unwrap();
        let key_b = make_vault(&p.data_dir, "segredo-B", 2).await;
        let enc_b = fs::read(p.data_dir.join("secrets.enc")).unwrap();
        assert_ne!(key_a, key_b);
        fs::write(p.data_dir.join(DB_NAME), b"sqlite-B").unwrap();
        fs::write(p.data_dir.join("whatsapp/acc/session"), b"wa-B").unwrap();
        let bytes = fs::read(&zip_path).unwrap();
        // sem a chave (a daqui é outra) e com chave errada: recusa e não agenda
        let e = stage(&p, &bytes, None).unwrap_err();
        assert!(e.contains("precisa da chave"), "{e}");
        let e = stage(&p, &bytes, Some(&key_b)).unwrap_err();
        assert!(e.contains("não abre"), "{e}");
        assert!(!p.data_dir.join(PENDING).exists());
        assert!(!p.data_dir.join(PENDING_KEY).exists());
        // com a chave da origem: agenda e aplica
        stage(&p, &bytes, Some(&format!("  {key_a}\n"))).unwrap();
        assert!(p.data_dir.join(PENDING).is_file());
        let r = apply_pending(&p).expect("havia pendência");
        r.unwrap();
        assert!(!p.data_dir.join(PENDING).exists());
        assert!(!p.data_dir.join(PENDING_KEY).exists());
        assert_eq!(fs::read(p.data_dir.join(DB_NAME)).unwrap(), b"sqlite-A");
        assert_eq!(fs::read(p.data_dir.join("secrets.enc")).unwrap(), enc_a);
        assert_eq!(
            fs::read_to_string(p.data_dir.join("secrets.key"))
                .unwrap()
                .trim(),
            key_a.trim()
        );
        assert_eq!(
            fs::read(p.data_dir.join("whatsapp/acc/session")).unwrap(),
            b"wa-A"
        );
        assert!(p.data_dir.join("restored-manifest.json").is_file());
        assert!(p.data_dir.join("logs-should-stay.txt").is_file());
        // o que foi substituído ficou guardado em old/restore-*/
        let old = fs::read_dir(p.data_dir.join("old"))
            .unwrap()
            .flatten()
            .next()
            .unwrap();
        assert_eq!(fs::read(old.path().join("secrets.enc")).unwrap(), enc_b);
        assert_eq!(
            fs::read_to_string(old.path().join("secrets.key")).unwrap(),
            key_b
        );
        assert_eq!(fs::read(old.path().join(DB_NAME)).unwrap(), b"sqlite-B");
        assert!(apply_pending(&p).is_none());

        // restaurar na própria máquina: a chave daqui abre, não precisa informar
        // (`old/restore-<segundo>` do passo anterior sai para não colidir no teste)
        fs::remove_dir_all(p.data_dir.join("old")).unwrap();
        stage(&p, &bytes, None).unwrap();
        apply_pending(&p).unwrap().unwrap();
        assert_eq!(fs::read(p.data_dir.join("secrets.enc")).unwrap(), enc_a);
    }

    /// Backup sem a sessão do WhatsApp: a do destino fica como estava.
    #[tokio::test]
    async fn backup_without_whatsapp_keeps_destination_session() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        fs::create_dir_all(p.data_dir.join("whatsapp").join("acc")).unwrap();
        fs::create_dir_all(&p.cache_dir).unwrap();
        fs::write(p.data_dir.join("whatsapp/acc/session"), b"wa-origem").unwrap();
        let snap = p.cache_dir.join("snap.db");
        fs::write(&snap, b"sqlite-A").unwrap();
        let zip_path = p.cache_dir.join("b.zip");
        let m = write_zip(&p.data_dir, &snap, &zip_path, false, false, None).unwrap();
        assert!(!m.include_whatsapp);
        let mut ar = ZipArchive::new(fs::File::open(&zip_path).unwrap()).unwrap();
        assert!(ar.by_name("whatsapp/acc/session").is_err());
        drop(ar);
        fs::write(p.data_dir.join("whatsapp/acc/session"), b"wa-destino").unwrap();
        stage(&p, &fs::read(&zip_path).unwrap(), None).unwrap();
        apply_pending(&p).unwrap().unwrap();
        assert_eq!(
            fs::read(p.data_dir.join("whatsapp/acc/session")).unwrap(),
            b"wa-destino"
        );
        // manifesto antigo (sem o campo) = sessão incluída
        let old: Manifest = serde_json::from_str(
            r#"{"format":1,"version":"0.1.29","created_at":"2026-11-13T19:23:37Z","hostname":"h","instance_id":null,"include_browser":false,"files":1,"bytes":1}"#,
        )
        .unwrap();
        assert!(old.include_whatsapp);
    }

    #[test]
    fn stage_rejects_garbage() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        fs::create_dir_all(&p.data_dir).unwrap();
        let e = stage(&p, b"not a zip", None).unwrap_err();
        assert!(e.contains("zip"), "{e}");
        assert!(!p.data_dir.join(PENDING).exists());
        assert!(!p.data_dir.join(format!("{PENDING}.part")).exists());
    }
    /// Restaurar backup de outra máquina não pode trancar quem restaurou:
    /// a rede daqui e a conta de quem pediu voltam depois de abrir o banco.
    #[tokio::test]
    async fn keep_reapplies_network_and_user_after_restore() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        fs::create_dir_all(&p.data_dir).unwrap();
        // banco "restaurado": rede padrão (loopback) e um admin de outra máquina
        let storage = will_storage::Storage::open(&p.data_dir.join(DB_NAME))
            .await
            .unwrap();
        storage
            .create_user(
                "u-origem",
                "admin",
                "Admin",
                None,
                "hash-origem",
                "admin",
                true,
            )
            .await
            .unwrap();
        let network = will_core::settings::NetworkSettings {
            bind: "0.0.0.0".into(),
            tls: true,
            ..Default::default()
        };
        let user = |name: &str, hash: &str| KeepUser {
            id: format!("u-{name}"),
            username: name.into(),
            name: name.into(),
            email: None,
            password_hash: hash.into(),
            role: "admin".into(),
            must_change_password: false,
        };
        // mesmo username: troca a senha pela de quem restaurou
        write_keep(
            &p,
            &Keep {
                network: network.clone(),
                user: Some(user("admin", "hash-daqui")),
            },
        )
        .unwrap();
        apply_keep(&p, &storage).await;
        assert!(!p.data_dir.join(KEEP).exists());
        let s = storage.load_settings().await.unwrap();
        assert_eq!(s.network.bind, "0.0.0.0");
        assert!(s.network.tls);
        let u = storage.find_user_by_login("admin").await.unwrap().unwrap();
        assert_eq!(u.password_hash, "hash-daqui");
        assert!(!u.must_change_password);
        // username que não veio no backup: a conta é recriada
        write_keep(
            &p,
            &Keep {
                network,
                user: Some(user("will", "hash-will")),
            },
        )
        .unwrap();
        apply_keep(&p, &storage).await;
        let u = storage.find_user_by_login("will").await.unwrap().unwrap();
        assert_eq!(u.password_hash, "hash-will");
        // sem arquivo: não mexe em nada
        apply_keep(&p, &storage).await;
    }
}
