//! Monta a `EngineConfig` a partir das settings.

use secrecy::{ExposeSecret, SecretString};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use will_core::{AppPaths, Origin, Settings, ThreadId};
use will_engine::{EngineConfig, ThreadEnv};

pub fn build(
    settings: &Settings,
    paths: &AppPaths,
    token: Option<&SecretString>,
    mcp_url: &str,
    external_env: &BTreeMap<String, String>,
) -> EngineConfig {
    let e = &settings.engine;
    let mut env = will_engine::config::minimal_env();
    // `claude` (script Node) e `node` precisam ser achados mesmo quando o
    // serviço subiu com PATH mínimo (Finder/launchd, autostart, systemd).
    env.insert(
        "PATH".into(),
        will_platform::augmented_path()
            .to_string_lossy()
            .to_string(),
    );
    if let Some(t) = token {
        env.insert(
            "CLAUDE_CODE_OAUTH_TOKEN".into(),
            t.expose_secret().to_string(),
        );
    }
    if !mcp_url.is_empty() {
        env.insert("WILL_IA_MCP_URL".into(), mcp_url.to_string());
    }
    for (k, v) in external_env {
        env.insert(k.clone(), v.clone());
    }
    let cwd = settings
        .kb
        .path
        .as_ref()
        .map(|p| paths.resolve_user_path(p))
        .unwrap_or_else(|| paths.data_dir.join("kb-nao-configurada"));
    // Browser CDP: o agent-browser fala com o proxy local sem flag nenhuma, e
    // capturas/downloads sem caminho explícito vão para `<kb>/inbox/browser/`
    // em vez de /tmp ou da raiz da KB.
    if settings.browser.enabled {
        env.insert(
            "AGENT_BROWSER_CDP".into(),
            format!("http://127.0.0.1:{}", settings.browser.listen_port),
        );
        let media = cwd.join("inbox").join("browser");
        env.insert(
            "AGENT_BROWSER_SCREENSHOT_DIR".into(),
            media.to_string_lossy().to_string(),
        );
        env.insert(
            "AGENT_BROWSER_DOWNLOAD_PATH".into(),
            media.to_string_lossy().to_string(),
        );
    }
    let mcp_config_path = Some(cwd.join(".mcp.json")).filter(|p| p.exists());
    let mut allowed = e.allowed_tools.clone();
    if settings.browser.enabled
        && settings.browser.allow_agent_browser_tool
        && !allowed.iter().any(|t| t.starts_with("Bash(agent-browser"))
    {
        allowed.push("Bash(agent-browser:*)".into());
    }
    for ext in settings.external_mcps.iter().filter(|x| x.enabled) {
        match &ext.allowed_tools {
            Some(list) if ext.auto_allow_tools => {
                for t in list {
                    allowed.push(format!("mcp__{}__{}", ext.name, t));
                }
            }
            None if ext.auto_allow_tools => allowed.push(format!("mcp__{}__*", ext.name)),
            _ => {}
        }
    }
    let claude_bin = if e.claude_bin.trim().is_empty() {
        "claude".to_string()
    } else {
        e.claude_bin.clone()
    };
    let claude_bin =
        will_platform::which(&claude_bin).unwrap_or_else(|| PathBuf::from(&claude_bin));
    EngineConfig {
        claude_bin,
        cwd,
        env,
        permission_mode: e.permission_mode,
        allowed_tools: allowed,
        mcp_config_path,
        strict_mcp_config: !e.inherit_global_mcps,
        model: e.model.clone().filter(|m| !m.trim().is_empty()),
        max_budget_usd: e.max_budget_usd,
        effort: e.effort.clone().filter(|m| !m.trim().is_empty()),
        include_partial_messages: e.include_partial_messages,
        append_system_prompt: e.append_system_prompt.clone(),
        max_live: e.max_live_processes.max(1),
        idle_timeout: Duration::from_secs(e.idle_timeout_minutes.max(1) * 60),
        kill_grace: Duration::from_secs(3),
        raw_log_dir: if e.log_raw_stream {
            Some(paths.logs_dir.clone())
        } else {
            None
        },
        // Só o `id` do cadastro (10 chars), nunca o segredo: identifica nos logs
        // qual token estava em uso quando o processo subiu.
        token_id: token.and(e.tokens.iter().find(|t| t.active).map(|t| t.id.clone())),
    }
}

pub fn thread_env(thread: ThreadId, origin: &Origin, mcp_token: Option<String>) -> ThreadEnv {
    let mut m = ThreadEnv::new();
    m.insert("WILL_IA_THREAD_ID".into(), thread.to_string());
    m.insert("WILL_IA_ORIGIN".into(), origin.kind().as_str().to_string());
    if let Some(t) = mcp_token {
        m.insert("WILL_IA_MCP_TOKEN".into(), t);
    }
    m
}
