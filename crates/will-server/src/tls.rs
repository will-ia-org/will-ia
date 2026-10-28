//! TLS embutido (`docs/26`, etapa 5): certificado próprio ou autoassinado
//! gerado com `rcgen` em `<data_dir>/tls/`. O app fixa a CA (PEM) no perfil.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// Resolve (ou gera) os arquivos de certificado e chave.
pub fn ensure_files(
    data_dir: &Path,
    cert_path: Option<&str>,
    key_path: Option<&str>,
    bind: &str,
    extra_names: &[String],
) -> Result<TlsFiles, String> {
    if let (Some(c), Some(k)) = (
        cert_path.map(str::trim).filter(|s| !s.is_empty()),
        key_path.map(str::trim).filter(|s| !s.is_empty()),
    ) {
        let (c, k) = (PathBuf::from(c), PathBuf::from(k));
        if !c.is_file() || !k.is_file() {
            return Err(format!(
                "certificado/chave não encontrados: {} / {}",
                c.display(),
                k.display()
            ));
        }
        return Ok(TlsFiles { cert: c, key: k });
    }
    let dir = data_dir.join("tls");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    let names_file = dir.join("names.txt");
    let mut names = vec!["localhost".to_string()];
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            names.push(h);
        }
    }
    if let Ok(h) = hostname() {
        if !names.contains(&h) {
            names.push(h);
        }
    }
    if bind != "0.0.0.0" && bind != "::" && !bind.is_empty() {
        names.push(bind.to_string());
    }
    for n in extra_names
        .iter()
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
    {
        if !names.iter().any(|x| x == n) {
            names.push(n.to_string());
        }
    }
    let wanted = names.join("\n");
    let current = std::fs::read_to_string(&names_file).unwrap_or_default();
    if cert.is_file() && key.is_file() && current.trim() == wanted.trim() {
        return Ok(TlsFiles { cert, key });
    }
    let certified = rcgen::generate_simple_self_signed(names).map_err(|e| e.to_string())?;
    std::fs::write(&names_file, &wanted).map_err(|e| e.to_string())?;
    std::fs::write(&cert, certified.cert.pem()).map_err(|e| e.to_string())?;
    std::fs::write(&key, certified.signing_key.serialize_pem()).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600));
    }
    tracing::warn!(cert = %cert.display(), "certificado TLS autoassinado gerado; o app precisa fixar este certificado no perfil");
    Ok(TlsFiles { cert, key })
}

fn hostname() -> Result<String, String> {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .map_err(|e| e.to_string())
}

/// SHA-256 do certificado DER (como o app mostra/fixa).
pub fn fingerprint(cert_pem_path: &Path) -> Option<String> {
    let pem = std::fs::read(cert_pem_path).ok()?;
    let der = pem_to_der(&pem)?;
    let mut h = Sha256::new();
    h.update(&der);
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    Some(hex)
}

fn pem_to_der(pem: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(pem).ok()?;
    let body: String = text
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(body).ok()
}
