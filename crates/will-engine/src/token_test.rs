//! Testa um token num `claude -p` descartável.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, Clone, serde::Serialize)]
pub struct TokenTestResult {
    pub ok: bool,
    pub error: Option<String>,
    pub output_tail: String,
}

pub async fn test_token(
    claude_bin: &Path,
    cwd: &Path,
    token: &str,
    base_env: &BTreeMap<String, String>,
    timeout: Duration,
) -> TokenTestResult {
    let mut env = base_env.clone();
    env.insert("CLAUDE_CODE_OAUTH_TOKEN".into(), token.to_string());
    let mut cmd = Command::new(claude_bin);
    // Sem ferramentas e sem MCP: barato e rápido. Sem --max-budget-usd: o teste real já estourou 0.05.
    cmd.args([
        "-p",
        "Responda apenas: OK",
        "--output-format",
        "text",
        "--tools",
        "",
        "--strict-mcp-config",
        "--permission-mode",
        "plan",
    ])
    .current_dir(cwd)
    .env_clear()
    .envs(&env)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    will_core::process::hide_console(&mut cmd);
    will_platform::process::die_with_parent(&mut cmd);
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    let child = match cmd.spawn() {
        Ok(c) => {
            will_platform::process::attach_to_job(&c);
            c
        }
        Err(e) => {
            tracing::error!(error = %e, bin = %claude_bin.display(), "teste de token: não foi possível executar o claude");
            return TokenTestResult {
                ok: false,
                error: Some(format!("não foi possível executar o claude: {e}")),
                output_tail: String::new(),
            };
        }
    };
    let started = std::time::Instant::now();
    let out = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::error!(error = %e, "teste de token: falha ao aguardar o claude");
            return TokenTestResult {
                ok: false,
                error: Some(e.to_string()),
                output_tail: String::new(),
            };
        }
        Err(_) => {
            tracing::warn!(
                timeout_secs = timeout.as_secs(),
                "teste de token: tempo esgotado"
            );
            return TokenTestResult {
                ok: false,
                error: Some("tempo esgotado".into()),
                output_tail: String::new(),
            };
        }
    };
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lower = text.to_lowercase();
    let bad = [
        "unauthor",
        "401",
        "invalid api key",
        "invalid token",
        "expired",
        "failed to authenticate",
        "rate limit",
        "limit reached",
        "quota",
        "not logged in",
        "authentication_error",
    ];
    let is_bad = bad.iter().any(|b| lower.contains(b));
    let tail = will_core::tools::truncate(text.trim(), 400);
    tracing::info!(
        ok = out.status.success() && !is_bad,
        exit_code = ?out.status.code(),
        duration_ms = started.elapsed().as_millis() as u64,
        "teste de token concluído"
    );
    if out.status.success() && !is_bad {
        TokenTestResult {
            ok: true,
            error: None,
            output_tail: tail,
        }
    } else {
        let first = text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("sem saída")
            .to_string();
        TokenTestResult {
            ok: false,
            error: Some(will_core::tools::truncate(&first, 200)),
            output_tail: tail,
        }
    }
}
