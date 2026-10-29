//! Perfis de conexão do app: a que servidor conectar e com qual token (de
//! sessão de uma conta, obtido no login, ou o token local da API).
//! Arquivo `profiles.json` na pasta de configuração do app (0600). O perfil
//! "Esta máquina" é descoberto em tempo de execução pelo `api-port` e pelo
//! token que o servidor local grava no seu `data_dir` (`docs/26`).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    pub id: String,
    pub name: String,
    /// `http://host:porta` (sem barra final).
    pub url: String,
    pub token: String,
    /// Conta com que o token foi obtido (`POST /api/auth/login`); `None` para o
    /// token local da API.
    #[serde(default)]
    pub username: Option<String>,
    /// Senha da conta, guardada para o botão "Conectar" reautenticar sozinho
    /// (o token de sessão expira e some no logoff). Fica em claro no
    /// `profiles.json` do cliente; é uma troca consciente por comodidade.
    #[serde(default)]
    pub password: Option<String>,
    /// Certificado do servidor (PEM) fixado para `https://` (TOFU: o usuário
    /// confere a impressão digital em `/api/health` e cola o PEM aqui).
    #[serde(default)]
    pub ca_pem: Option<String>,
    /// Aceitar qualquer certificado (só para testes; o app avisa).
    #[serde(default)]
    pub insecure: bool,
}

impl Profile {
    pub const LOCAL_ID: &'static str = "local";
    /// Id do perfil vazio ("nenhum servidor"): o app está na tela de entrada.
    pub const NONE_ID: &'static str = "none";

    /// Perfil vazio: sem servidor escolhido. O cliente aponta para uma porta
    /// inválida de loopback e o app não tenta conectar nele.
    pub fn none() -> Profile {
        Profile {
            id: Self::NONE_ID.into(),
            name: crate::i18n::tr("nenhum servidor").into(),
            url: "http://127.0.0.1:0".into(),
            token: String::new(),
            username: None,
            password: None,
            ca_pem: None,
            insecure: false,
        }
    }

    pub fn is_none(&self) -> bool {
        self.id == Self::NONE_ID
    }

    /// Política de certificado derivada do perfil (só importa em `https://`).
    pub fn tls_options(&self) -> will_client::TlsOptions {
        will_client::TlsOptions {
            ca_pem: self
                .ca_pem
                .clone()
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty()),
            insecure: self.insecure,
        }
    }

    pub fn is_https(&self) -> bool {
        self.url.trim_start().starts_with("https://")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfilesFile {
    /// Id do último perfil usado.
    pub last: Option<String>,
    pub profiles: Vec<Profile>,
}

/// Pasta de configuração do app (`~/.config/will-ia-app`, `%APPDATA%\will-ia-app`…).
pub fn config_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "will-ia-app")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("will-ia-app"))
}

fn file() -> PathBuf {
    config_dir().join("profiles.json")
}

pub fn load() -> ProfilesFile {
    std::fs::read_to_string(file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(pf: &ProfilesFile) -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(pf).map_err(|e| e.to_string())?;
    let path = file();
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Servidor nesta máquina, se estiver no ar: lê `api-port` e o token que o
/// `will-ia` grava no `data_dir` (respeita `WILL_IA_DATA_DIR` e modo portátil).
pub fn local_profile() -> Option<Profile> {
    let paths = will_core::AppPaths::resolve();
    let port: u16 = std::fs::read_to_string(paths.data_dir.join("api-port"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let token = std::fs::read_to_string(paths.session_token_file())
        .ok()?
        .trim()
        .to_string();
    if token.is_empty() {
        return None;
    }
    Some(Profile {
        id: Profile::LOCAL_ID.into(),
        name: "Esta máquina".into(),
        url: format!("http://127.0.0.1:{port}"),
        token,
        username: None,
        password: None,
        ca_pem: None,
        insecure: false,
    })
}

/// Perfil a usar no arranque: explícito (`--url`), por nome (`--profile`),
/// o último usado, ou "Esta máquina".
pub fn pick(url: Option<&str>, token: Option<&str>, name: Option<&str>) -> Option<Profile> {
    if let Some(u) = url {
        return Some(Profile {
            id: "cli".into(),
            name: u.to_string(),
            url: u.trim_end_matches('/').to_string(),
            token: token.unwrap_or_default().to_string(),
            username: None,
            password: None,
            ca_pem: None,
            insecure: false,
        });
    }
    let pf = load();
    if let Some(n) = name {
        if let Some(p) = pf.profiles.iter().find(|p| p.name == n || p.id == n) {
            return Some(p.clone());
        }
        if n == Profile::LOCAL_ID || n.eq_ignore_ascii_case("local") {
            return local_profile();
        }
    }
    if let Some(last) = &pf.last {
        if last == Profile::LOCAL_ID {
            if let Some(p) = local_profile() {
                return Some(p);
            }
        }
        if let Some(p) = pf.profiles.iter().find(|p| &p.id == last) {
            return Some(p.clone());
        }
    }
    local_profile().or_else(|| pf.profiles.first().cloned())
}

pub fn remember_last(id: &str) {
    let mut pf = load();
    pf.last = Some(id.to_string());
    let _ = save(&pf);
}
