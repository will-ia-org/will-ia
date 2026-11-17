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
    /// `prompt`: texto de sistema já composto (geral + thread, ver
    /// [`compose_system_prompt`]) e se o `claude` deve remontá-lo em vez de
    /// reusar o que ficou gravado na sessão.
    pub fn args(
        &self,
        session_id: Option<&str>,
        new_session_id: &str,
        prompt: &SpawnPrompt,
    ) -> Vec<String> {
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
        if !prompt.text.trim().is_empty() {
            a.push("--append-system-prompt".into());
            a.push(prompt.text.clone());
        }
        if prompt.snapshot_off {
            a.push("--system-prompt-snapshot".into());
            a.push("off".into());
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

/// Prompt de sistema de um spawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnPrompt {
    /// Texto passado em `--append-system-prompt` (vazio = não passa a flag).
    pub text: String,
    /// Passa `--system-prompt-snapshot off`: o `claude` grava o prompt de
    /// sistema na primeira mensagem da sessão e, num `--resume`, ignora o texto
    /// novo até a conversa ser compactada. Desligar o snapshot faz o texto
    /// atual valer já.
    pub snapshot_off: bool,
}

/// Tags que separam as duas camadas do prompt de sistema.
pub const GENERAL_PROMPT_TAG: &str = "instrucoes_gerais";
pub const THREAD_PROMPT_TAG: &str = "instrucoes_desta_thread";

/// Soma o prompt geral (Configurações → Motor) ao prompt da thread.
///
/// Sem prompt de thread, o geral vai como está. Com prompt de thread, cada
/// camada vai na sua tag, o geral primeiro:
///
/// ```text
/// <instrucoes_gerais>
/// …
/// </instrucoes_gerais>
///
/// <instrucoes_desta_thread>
/// …
/// </instrucoes_desta_thread>
/// ```
pub fn compose_system_prompt(general: &str, thread: &str) -> String {
    let general = general.trim();
    let thread = thread.trim();
    if thread.is_empty() {
        return general.to_string();
    }
    let block = |tag: &str, body: &str| format!("<{tag}>\n{body}\n</{tag}>");
    let thread_block = block(THREAD_PROMPT_TAG, thread);
    if general.is_empty() {
        return thread_block;
    }
    format!("{}\n\n{}", block(GENERAL_PROMPT_TAG, general), thread_block)
}

/// Impressão digital do prompt de sistema com que uma sessão nasceu
/// (SHA-256, 16 bytes em hex). Comparada a cada `--resume` para saber se o
/// texto mudou depois que a conversa começou.
pub fn prompt_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(text.as_bytes());
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// Primeira versão do `claude` com `--system-prompt-snapshot`.
const SNAPSHOT_FLAG_SINCE: (u32, u32, u32) = (2, 1, 257);

/// Lê `2.1.325` de uma saída como `2.1.325 (Claude Code)`.
pub fn parse_claude_version(out: &str) -> Option<(u32, u32, u32)> {
    let first = out.split_whitespace().next()?;
    let mut it = first.split('.').map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// O `claude` instalado aceita `--system-prompt-snapshot`? Uma flag
/// desconhecida derruba o processo, então só passamos depois de conferir a
/// versão. Versão ilegível conta como "não aceita".
pub fn supports_snapshot_flag(version_output: &str) -> bool {
    parse_claude_version(version_output).is_some_and(|v| v >= SNAPSHOT_FLAG_SINCE)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_alone_goes_untouched() {
        assert_eq!(
            compose_system_prompt("  regra geral \n", "  "),
            "regra geral"
        );
        assert_eq!(compose_system_prompt("", ""), "");
    }

    #[test]
    fn thread_prompt_is_added_in_tags_after_the_general() {
        let out = compose_system_prompt("regra geral", "só fale de finanças\n");
        assert_eq!(
            out,
            "<instrucoes_gerais>\nregra geral\n</instrucoes_gerais>\n\n\
             <instrucoes_desta_thread>\nsó fale de finanças\n</instrucoes_desta_thread>"
        );
        assert_eq!(
            compose_system_prompt("", "x"),
            "<instrucoes_desta_thread>\nx\n</instrucoes_desta_thread>"
        );
    }

    fn cfg() -> EngineConfig {
        EngineConfig {
            claude_bin: "claude".into(),
            cwd: ".".into(),
            env: BTreeMap::new(),
            permission_mode: PermissionMode::AcceptEdits,
            allowed_tools: vec![],
            mcp_config_path: None,
            strict_mcp_config: true,
            model: None,
            max_budget_usd: None,
            effort: None,
            include_partial_messages: false,
            append_system_prompt: "geral".into(),
            max_live: 1,
            idle_timeout: std::time::Duration::from_secs(60),
            kill_grace: std::time::Duration::from_secs(1),
            raw_log_dir: None,
            token_id: None,
        }
    }

    /// Valor que segue uma flag na linha de comando.
    fn value_of<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        let i = args.iter().position(|a| a == flag)?;
        args.get(i + 1).map(String::as_str)
    }

    #[test]
    fn args_carry_one_composed_prompt_and_the_snapshot_flag_only_when_asked() {
        let c = cfg();
        let text = compose_system_prompt(&c.append_system_prompt, "da thread");
        let fresh = c.args(
            None,
            "nova",
            &SpawnPrompt {
                text: text.clone(),
                snapshot_off: false,
            },
        );
        assert_eq!(
            fresh
                .iter()
                .filter(|a| *a == "--append-system-prompt")
                .count(),
            1
        );
        assert_eq!(
            value_of(&fresh, "--append-system-prompt"),
            Some(text.as_str())
        );
        assert_eq!(value_of(&fresh, "--session-id"), Some("nova"));
        assert!(!fresh.iter().any(|a| a == "--system-prompt-snapshot"));

        let resumed = c.args(
            Some("antiga"),
            "nova",
            &SpawnPrompt {
                text,
                snapshot_off: true,
            },
        );
        assert_eq!(value_of(&resumed, "--resume"), Some("antiga"));
        assert_eq!(value_of(&resumed, "--system-prompt-snapshot"), Some("off"));

        let empty = c.args(None, "nova", &SpawnPrompt::default());
        assert!(!empty.iter().any(|a| a == "--append-system-prompt"));
    }

    #[test]
    fn hash_changes_with_the_text() {
        assert_eq!(prompt_hash("a"), prompt_hash("a"));
        assert_ne!(prompt_hash("a"), prompt_hash("b"));
        assert_eq!(prompt_hash("a").len(), 32);
    }

    #[test]
    fn snapshot_flag_depends_on_the_version() {
        assert!(supports_snapshot_flag("2.1.325 (Claude Code)"));
        assert!(supports_snapshot_flag("2.1.257"));
        assert!(supports_snapshot_flag("3.0.0 (Claude Code)"));
        assert!(!supports_snapshot_flag("2.1.256 (Claude Code)"));
        assert!(!supports_snapshot_flag("1.9.999"));
        assert!(!supports_snapshot_flag("claude: command not found"));
        assert!(!supports_snapshot_flag(""));
    }
}
