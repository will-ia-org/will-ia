//! Confinamento de arquivos na base de conhecimento (KB): pastas de download
//! e caminhos de anexos/uploads. Usado pelo Gmail e pelo Drive.

use std::path::{Component, Path, PathBuf};
use will_core::ToolError;

/// `<kb>/<configured>` se `configured` é relativo e sem `..`; senão `<kb>/<fallback>`.
pub(crate) fn confined_dir(kb: &Path, configured: &str, fallback: &str) -> PathBuf {
    let rel = Path::new(configured.trim());
    let safe = !configured.trim().is_empty()
        && rel
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    kb.join(if safe { rel } else { Path::new(fallback) })
}

/// Resolve um caminho (relativo à KB ou absoluto) garantindo que é um arquivo
/// **dentro** da KB. `what` entra nas mensagens ("anexo", "arquivo").
pub(crate) fn resolve_in_kb(kb: &Path, p: &str, what: &str) -> Result<PathBuf, ToolError> {
    let candidate = {
        let path = Path::new(p.trim());
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            kb.join(path)
        }
    };
    let kb_c = kb
        .canonicalize()
        .map_err(|e| ToolError::Failed(format!("base de conhecimento inacessível: {e}")))?;
    let c = candidate
        .canonicalize()
        .map_err(|e| ToolError::InvalidArgs(format!("{what} '{p}' não encontrado: {e}")))?;
    if !c.starts_with(&kb_c) {
        return Err(ToolError::Forbidden(format!(
            "{what} '{p}' fora da base de conhecimento"
        )));
    }
    if !c.is_file() {
        return Err(ToolError::InvalidArgs(format!(
            "{what} '{p}' não é um arquivo"
        )));
    }
    Ok(c)
}

/// Caminho relativo à KB (string vazia se não estiver dentro dela).
pub(crate) fn relative_to_kb(kb: &Path, path: &Path) -> String {
    path.strip_prefix(kb)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Evita sobrescrever: `nome.ext`, `nome (2).ext`, `nome (3).ext`…
pub(crate) fn unique_path(dir: &Path, filename: &str) -> PathBuf {
    let first = dir.join(filename);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match filename.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (filename.to_string(), String::new()),
    };
    for n in 2..1000 {
        let p = dir.join(format!("{stem} ({n}){ext}"));
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{stem}-{}{ext}", uuid::Uuid::new_v4()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confined_dir_falls_back_outside_kb() {
        let kb = Path::new("/kb");
        assert_eq!(
            confined_dir(kb, "inbox/drive", "inbox/x"),
            PathBuf::from("/kb/inbox/drive")
        );
        assert_eq!(
            confined_dir(kb, "../etc", "inbox/x"),
            PathBuf::from("/kb/inbox/x")
        );
        assert_eq!(
            confined_dir(kb, "/abs", "inbox/x"),
            PathBuf::from("/kb/inbox/x")
        );
        assert_eq!(
            confined_dir(kb, "  ", "inbox/x"),
            PathBuf::from("/kb/inbox/x")
        );
    }

    #[test]
    fn resolve_in_kb_rejects_outside_and_missing() {
        let dir = tempfile::tempdir().unwrap();
        let kb = dir.path().join("kb");
        std::fs::create_dir_all(kb.join("inbox")).unwrap();
        std::fs::write(kb.join("inbox/a.txt"), b"x").unwrap();
        std::fs::write(dir.path().join("fora.txt"), b"x").unwrap();
        assert!(resolve_in_kb(&kb, "inbox/a.txt", "arquivo").is_ok());
        assert!(matches!(
            resolve_in_kb(&kb, "../fora.txt", "arquivo"),
            Err(ToolError::Forbidden(_))
        ));
        assert!(matches!(
            resolve_in_kb(&kb, "inbox/nao-existe.txt", "arquivo"),
            Err(ToolError::InvalidArgs(_))
        ));
        assert!(matches!(
            resolve_in_kb(&kb, "inbox", "arquivo"),
            Err(ToolError::InvalidArgs(_))
        ));
    }

    #[test]
    fn unique_path_numbers_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(unique_path(dir.path(), "a.txt"), dir.path().join("a.txt"));
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "a.txt"),
            dir.path().join("a (2).txt")
        );
        std::fs::write(dir.path().join("a (2).txt"), b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "a.txt"),
            dir.path().join("a (3).txt")
        );
        assert_eq!(unique_path(dir.path(), "semext"), dir.path().join("semext"));
    }
}
