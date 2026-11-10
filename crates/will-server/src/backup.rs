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
/// Arquivos de 1º nível que entram no backup (se existirem).
const TOP_FILES: &[&str] = &[
    "secrets.enc",
    "secrets.key",
    "secrets.json",
    "telemetry-defaults.json",
    "instance-id",
    "app.ron",
];
/// Pastas que entram sempre.
const TOP_DIRS: &[&str] = &["whatsapp"];
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
    pub files: usize,
    pub bytes: u64,
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
pub async fn build(state: &AppState, include_browser: bool) -> Result<(PathBuf, Manifest), String> {
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
        write_zip(&data_dir, &snap2, &out2, include_browser, instance_id)
    })
    .await
    .map_err(|e| e.to_string())??;
    let _ = fs::remove_file(&snap);
    tracing::info!(path = %out.display(), files = manifest.files, bytes = manifest.bytes, include_browser, "backup gerado");
    Ok((out, manifest))
}

fn write_zip(
    data_dir: &Path,
    snap: &Path,
    out: &Path,
    include_browser: bool,
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
    let mut dirs: Vec<&str> = TOP_DIRS.to_vec();
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

/// Grava os bytes recebidos como `restore-pending.zip` (depois de validar).
pub fn stage(paths: &AppPaths, bytes: &[u8]) -> Result<Manifest, String> {
    let part = paths.data_dir.join(format!("{PENDING}.part"));
    fs::write(&part, bytes).map_err(|e| e.to_string())?;
    let m = match inspect(&part) {
        Ok(m) => m,
        Err(e) => {
            let _ = fs::remove_file(&part);
            return Err(e);
        }
    };
    fs::rename(&part, paths.data_dir.join(PENDING)).map_err(|e| e.to_string())?;
    tracing::info!(files = m.files, bytes = m.bytes, from_host = %m.hostname, created_at = %m.created_at, "restauração agendada; o serviço vai reiniciar");
    Ok(m)
}

/// Na subida, antes de abrir o banco: aplica `restore-pending.zip` se existir.
/// `None` = nada pendente; `Some(Err)` = falhou (o arquivo vira `restore-failed-*.zip`).
pub fn apply_pending(paths: &AppPaths) -> Option<Result<Manifest, String>> {
    let pending = paths.data_dir.join(PENDING);
    if !pending.is_file() {
        return None;
    }
    let r = apply(paths, &pending);
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
    targets.extend(TOP_DIRS.iter().map(|s| s.to_string()));
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

    #[test]
    fn backup_roundtrip_moves_old_and_extracts_new() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        fs::create_dir_all(p.data_dir.join("whatsapp").join("acc")).unwrap();
        fs::create_dir_all(&p.cache_dir).unwrap();
        fs::write(p.data_dir.join("secrets.enc"), b"enc-A").unwrap();
        fs::write(p.data_dir.join("secrets.key"), b"key-A").unwrap();
        fs::write(p.data_dir.join("whatsapp/acc/session"), b"wa-A").unwrap();
        fs::write(p.data_dir.join("logs-should-stay.txt"), b"x").unwrap();
        let snap = p.cache_dir.join("snap.db");
        fs::write(&snap, b"sqlite-A").unwrap();
        let zip_path = p.cache_dir.join("b.zip");
        let m = write_zip(&p.data_dir, &snap, &zip_path, false, Some("inst".into())).unwrap();
        assert_eq!(m.format, FORMAT);
        assert!(m.files >= 4, "{m:?}");
        assert_eq!(
            inspect(&zip_path).unwrap().instance_id.as_deref(),
            Some("inst")
        );

        // "outra máquina": dados diferentes, restaura por cima
        fs::write(p.data_dir.join("secrets.enc"), b"enc-B").unwrap();
        fs::write(p.data_dir.join(DB_NAME), b"sqlite-B").unwrap();
        fs::write(p.data_dir.join("whatsapp/acc/session"), b"wa-B").unwrap();
        let bytes = fs::read(&zip_path).unwrap();
        stage(&p, &bytes).unwrap();
        assert!(p.data_dir.join(PENDING).is_file());
        let r = apply_pending(&p).expect("havia pendência");
        r.unwrap();
        assert!(!p.data_dir.join(PENDING).exists());
        assert_eq!(fs::read(p.data_dir.join(DB_NAME)).unwrap(), b"sqlite-A");
        assert_eq!(fs::read(p.data_dir.join("secrets.enc")).unwrap(), b"enc-A");
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
        assert_eq!(fs::read(old.path().join("secrets.enc")).unwrap(), b"enc-B");
        assert_eq!(fs::read(old.path().join(DB_NAME)).unwrap(), b"sqlite-B");
        assert!(apply_pending(&p).is_none());
    }

    #[test]
    fn stage_rejects_garbage() {
        let tmp = tempfile::tempdir().unwrap();
        let p = paths(tmp.path());
        fs::create_dir_all(&p.data_dir).unwrap();
        let e = stage(&p, b"not a zip").unwrap_err();
        assert!(e.contains("zip"), "{e}");
        assert!(!p.data_dir.join(PENDING).exists());
        assert!(!p.data_dir.join(format!("{PENDING}.part")).exists());
    }
}
