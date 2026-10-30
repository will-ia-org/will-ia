use std::collections::BTreeMap;
use std::path::PathBuf;

pub use will_core::settings::PermissionMode;

/// Tudo que o pool precisa para spawnar um `claude -p`.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub claude_bin: PathBuf,
    pub cwd: PathBuf,
    /// Variáveis extras (token, WILL_IA_*, WILL_MCP_*). O ambiente herdado é mínimo.
    pub env: BTreeMap<String, String>,
    pub permission_mode: PermissionMode,
    pub allowed_tools: Vec<String>,
    pub mcp_config_path: Option<PathBuf>,
    pub strict_mcp_config: bool,
    pub model: Option<String>,
    pub max_budget_usd: Option<f64>,
    pub effort: Option<String>,
    pub include_partial_messages: bool,
    pub append_system_prompt: String,
    pub max_live: usize,
    pub idle_timeout: std::time::Duration,
    pub kill_grace: std::time::Duration,
    /// Diretório para gravar o stream bruto (`claude-<data>.log`), se ligado.
    pub raw_log_dir: Option<PathBuf>,
    /// Id curto do token do Claude em uso (o `id` do cadastro, nunca o segredo).
    /// Só para rastreio nos logs (`engine_proc.token_id`).
    pub token_id: Option<String>,
}

impl EngineConfig {
    pub fn args(&self, session_id: Option<&str>, new_session_id: &str) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "-p".into(),
            "--input-format".into(),
            "stream-json".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--replay-user-messages".into(),
            "--permission-prompts".into(),
            "none".into(),
        ];
        if self.include_partial_messages {
            a.push("--include-partial-messages".into());
        }
        if !self.append_system_prompt.trim().is_empty() {
            a.push("--append-system-prompt".into());
            a.push(self.append_system_prompt.clone());
        }
        match session_id {
            Some(s) => {
                a.push("--resume".into());
                a.push(s.to_string());
            }
            None => {
                a.push("--session-id".into());
                a.push(new_session_id.to_string());
            }
        }
        if let Some(p) = &self.mcp_config_path {
            a.push("--mcp-config".into());
            a.push(p.to_string_lossy().to_string());
            if self.strict_mcp_config {
                a.push("--strict-mcp-config".into());
            }
        }
        match self.permission_mode {
            PermissionMode::AcceptEdits => {
                a.push("--permission-mode".into());
                a.push("acceptEdits".into());
                if !self.allowed_tools.is_empty() {
                    a.push("--allowedTools".into());
                    a.extend(self.allowed_tools.iter().cloned());
                }
            }
            PermissionMode::BypassPermissions => {
                a.push("--permission-mode".into());
                a.push("bypassPermissions".into());
                a.push("--dangerously-skip-permissions".into());
            }
        }
        if let Some(m) = &self.model {
            if !m.trim().is_empty() {
                a.push("--model".into());
                a.push(m.clone());
            }
        }
        if let Some(b) = self.max_budget_usd {
            a.push("--max-budget-usd".into());
            a.push(format!("{b}"));
        }
        if let Some(e) = &self.effort {
            if !e.trim().is_empty() {
                a.push("--effort".into());
                a.push(e.clone());
            }
        }
        a
    }
}

/// Ambiente mínimo herdado do serviço para o processo filho.
pub fn minimal_env() -> BTreeMap<String, String> {
    let keep = [
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "TERM",
        "TMPDIR",
        "TEMP",
        "TMP",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "SYSTEMROOT",
        "SystemRoot",
        "COMSPEC",
        "ComSpec",
        "PATHEXT",
        "HOMEDRIVE",
        "HOMEPATH",
        "PROGRAMFILES",
        "ProgramFiles",
        "WINDIR",
        "windir",
        "NUMBER_OF_PROCESSORS",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "NVM_DIR",
        "NVM_BIN",
        "PYENV_ROOT",
        "VIRTUAL_ENV",
        "NODE_PATH",
        "ANTHROPIC_MODEL",
        "ANTHROPIC_BASE_URL",
        "CLAUDE_CONFIG_DIR",
    ];
    let mut m = BTreeMap::new();
    for k in keep {
        if let Ok(v) = std::env::var(k) {
            m.insert(k.to_string(), v);
        }
    }
    m
}
