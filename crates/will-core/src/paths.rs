use std::path::{Path, PathBuf};

/// Diretórios da aplicação.
///
/// Dois modos:
/// - **Instalado**: diretórios do SO (`directories::ProjectDirs`).
/// - **Portátil** (pendrive): tudo ao lado do binário — `will-ia.db` na raiz e
///   a pasta `data/` com segredos, sessões do WhatsApp, logs e cache. O modo
///   portátil é detectado quando existe um arquivo `portable` (ou uma pasta
///   `data/`) na pasta do executável ou na pasta acima dela (layout `bin/`).
///
/// `WILL_IA_DATA_DIR` sobrescreve tudo (útil em testes); `WILL_IA_PORTABLE=1`
/// força o modo portátil.
#[derive(Debug, Clone)]
pub struct AppPaths {
    pub data_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub cache_dir: PathBuf,
    /// Raiz do pacote portátil (pasta com `will-ia.db`, `data/`, `bin/`), se houver.
    pub portable_root: Option<PathBuf>,
}

impl AppPaths {
    pub fn resolve() -> Self {
        if let Ok(dir) = std::env::var("WILL_IA_DATA_DIR") {
            return Self::from_data_dir(PathBuf::from(dir));
        }
        if let Some(root) = Self::detect_portable_root() {
            let data = root.join("data");
            return Self {
                logs_dir: data.join("logs"),
                cache_dir: data.join("cache"),
                data_dir: data,
                portable_root: Some(root),
            };
        }
        let dirs = directories::ProjectDirs::from("dev", "will", "will-ia");
        match dirs {
            Some(d) => Self {
                data_dir: d.data_dir().to_path_buf(),
                logs_dir: d
                    .state_dir()
                    .map(|p| p.join("logs"))
                    .unwrap_or_else(|| d.data_dir().join("logs")),
                cache_dir: d.cache_dir().to_path_buf(),
                portable_root: None,
            },
            None => Self::from_data_dir(std::env::temp_dir().join("will-ia")),
        }
    }

    fn from_data_dir(base: PathBuf) -> Self {
        Self {
            logs_dir: base.join("logs"),
            cache_dir: base.join("cache"),
            data_dir: base,
            portable_root: None,
        }
    }

    /// Pasta raiz do pacote portátil, se o executável estiver num.
    pub fn detect_portable_root() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let exe_dir = exe.parent()?.to_path_buf();
        let forced = std::env::var("WILL_IA_PORTABLE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let mut candidates = vec![exe_dir.clone()];
        if let Some(parent) = exe_dir.parent() {
            candidates.push(parent.to_path_buf());
        }
        for c in &candidates {
            if c.join("portable").is_file()
                || c.join("will-ia.db").is_file()
                || (c.join("data").is_dir() && c.join("bin").is_dir())
            {
                return Some(c.clone());
            }
        }
        if forced {
            // `bin/<binário>` → raiz é a pasta acima; senão a própria pasta do executável.
            let is_bin = exe_dir.file_name().map(|n| n == "bin").unwrap_or(false);
            return Some(if is_bin {
                exe_dir.parent().map(Path::to_path_buf).unwrap_or(exe_dir)
            } else {
                exe_dir
            });
        }
        None
    }

    pub fn is_portable(&self) -> bool {
        self.portable_root.is_some()
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.logs_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        if let Some(root) = &self.portable_root {
            let marker = root.join("portable");
            if !marker.exists() {
                let _ = std::fs::write(
                    &marker,
                    "will-ia portátil: banco e pasta data/ ficam ao lado deste arquivo.\n",
                );
            }
        }
        Ok(())
    }

    /// Banco SQLite: na raiz do pacote em modo portátil; em `data_dir` no modo instalado.
    pub fn db_path(&self) -> PathBuf {
        match &self.portable_root {
            Some(root) => root.join("will-ia.db"),
            None => self.data_dir.join("will-ia.db"),
        }
    }

    pub fn whatsapp_dir(&self) -> PathBuf {
        self.data_dir.join("whatsapp")
    }

    pub fn secrets_file(&self) -> PathBuf {
        self.data_dir.join("secrets.json")
    }

    pub fn session_token_file(&self) -> PathBuf {
        self.data_dir.join("ui-session.token")
    }

    /// Resolve um caminho configurado pelo usuário (ex.: a base de conhecimento).
    /// Caminhos relativos são relativos à raiz portátil (ou a `data_dir`), o que
    /// permite levar a KB no próprio pendrive (`workspace/`).
    pub fn resolve_user_path(&self, p: &str) -> PathBuf {
        let pb = PathBuf::from(p);
        if pb.is_absolute() {
            return pb;
        }
        let base = self
            .portable_root
            .clone()
            .unwrap_or_else(|| self.data_dir.clone());
        base.join(pb)
    }
}

/// Slug usado pelo Claude Code para a pasta de projeto em `~/.claude/projects/`.
pub fn claude_project_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Caminho da transcrição de uma sessão do Claude Code.
pub fn claude_transcript_path(cwd: &Path, session_id: &str) -> Option<PathBuf> {
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();
    Some(
        home.join(".claude")
            .join("projects")
            .join(claude_project_slug(cwd))
            .join(format!("{session_id}.jsonl")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_user_path_uses_portable_root() {
        let p = AppPaths {
            data_dir: PathBuf::from("/x/data"),
            logs_dir: PathBuf::from("/x/data/logs"),
            cache_dir: PathBuf::from("/x/data/cache"),
            portable_root: Some(PathBuf::from("/x")),
        };
        assert_eq!(
            p.resolve_user_path("workspace"),
            PathBuf::from("/x/workspace")
        );
        assert_eq!(p.resolve_user_path("/abs"), PathBuf::from("/abs"));
        assert_eq!(p.db_path(), PathBuf::from("/x/will-ia.db"));
    }
}
