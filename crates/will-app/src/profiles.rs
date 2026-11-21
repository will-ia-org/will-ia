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
#[cfg(not(target_arch = "wasm32"))]
pub fn config_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "will-ia-app")
        .map(|d| d.config_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("will-ia-app"))
}

/// No navegador não há pasta: o caminho só serve de rótulo (o que o app
/// guarda vai para o armazenamento local da página).
#[cfg(target_arch = "wasm32")]
pub fn config_dir() -> PathBuf {
    PathBuf::from("navegador")
}

#[cfg(not(target_arch = "wasm32"))]
fn file() -> PathBuf {
    config_dir().join("profiles.json")
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load() -> ProfilesFile {
    std::fs::read_to_string(file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
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

/// Chave dos perfis no armazenamento local do navegador.
#[cfg(target_arch = "wasm32")]
const STORAGE_KEY: &str = "will-ia-app/profiles";

#[cfg(target_arch = "wasm32")]
pub fn load() -> ProfilesFile {
    crate::platform::storage_get(STORAGE_KEY)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// No navegador a senha **não** é guardada (só o token de sessão): o
/// armazenamento local da página é menos protegido que um arquivo 0600.
#[cfg(target_arch = "wasm32")]
pub fn save(pf: &ProfilesFile) -> Result<(), String> {
    let mut pf = pf.clone();
    for p in &mut pf.profiles {
        p.password = None;
    }
    let text = serde_json::to_string(&pf).map_err(|e| e.to_string())?;
    crate::platform::storage_set(STORAGE_KEY, &text)
}

/// Servidor nesta máquina, se estiver no ar: lê `api-port` e o token que o
/// `will-ia` grava no `data_dir` (respeita `WILL_IA_DATA_DIR` e modo portátil).
/// "Esta máquina" no navegador: quem serve a página entrega o token local em
/// `window.__WILL_IA_LOCAL_TOKEN__`, e **só** quando ela é aberta por
/// `localhost` na própria máquina do servidor. De fora (iPad, outra máquina)
/// a variável não existe e o caminho é entrar com usuário e senha.
#[cfg(target_arch = "wasm32")]
pub fn local_profile() -> Option<Profile> {
    let window = web_sys::window()?;
    let token = js_sys::Reflect::get(&window, &"__WILL_IA_LOCAL_TOKEN__".into())
        .ok()?
        .as_string()
        .filter(|t| !t.trim().is_empty())?;
    Some(Profile {
        id: Profile::LOCAL_ID.into(),
        name: "Esta máquina".into(),
        url: crate::platform::web_origin()?,
        token: token.trim().to_string(),
        username: None,
        password: None,
        ca_pem: None,
        insecure: false,
    })
}

#[cfg(not(target_arch = "wasm32"))]
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
    // Com TLS ligado no servidor (`api-scheme`, gravado ao lado de `api-port`)
    // a porta só fala https. O certificado costuma ser o autoassinado, e em
    // loopback não há rede no meio para interceptar: aceita sem fixar.
    let https = std::fs::read_to_string(paths.data_dir.join("api-scheme"))
        .is_ok_and(|s| s.trim() == "https");
    let scheme = if https { "https" } else { "http" };
    Some(Profile {
        id: Profile::LOCAL_ID.into(),
        name: "Esta máquina".into(),
        url: format!("{scheme}://127.0.0.1:{port}"),
        token,
        username: None,
        password: None,
        ca_pem: None,
        insecure: https,
    })
}

/// Perfil de arranque do app web: o último usado **deste endereço** (a página
/// só fala com o servidor que a serviu) ou, na primeira vez, um perfil sem
/// token que abre o formulário de entrada já com o endereço preenchido.
#[cfg(target_arch = "wasm32")]
pub fn pick_web() -> Option<Profile> {
    // Aberto na própria máquina do servidor: entra direto, como no desktop.
    if let Some(p) = local_profile() {
        return Some(p);
    }
    let origin = crate::platform::web_origin()?;
    let pf = load();
    let saved = pf.profiles.iter().filter(|p| p.url == origin);
    let last = pf.last.as_deref();
    if let Some(p) = saved
        .clone()
        .find(|p| Some(p.id.as_str()) == last)
        .or_else(|| saved.clone().next())
    {
        return Some(p.clone());
    }
    Some(Profile {
        id: "web".into(),
        name: "will-ia".into(),
        url: origin,
        token: String::new(),
        username: None,
        password: None,
        ca_pem: None,
        insecure: false,
    })
}

/// Perfil a usar no arranque: explícito (`--url`), por nome (`--profile`),
/// o último usado, ou "Esta máquina".
#[cfg(not(target_arch = "wasm32"))]
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
