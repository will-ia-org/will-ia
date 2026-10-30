//! Teste de conexão a MCPs externos (stdio/http) com o cliente rmcp.

use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::ServiceExt;
use std::collections::BTreeMap;
use std::time::Duration;
use will_core::settings::McpTestResult;

#[derive(Debug, Clone)]
pub struct ResolvedExternal {
    pub name: String,
    pub kind: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
    pub url: Option<String>,
    pub headers: BTreeMap<String, String>,
}

/// Faz `initialize` + `tools/list` e retorna o resultado do teste.
pub async fn test_external(def: &ResolvedExternal, timeout: Duration) -> McpTestResult {
    let at = Some(chrono::Utc::now());
    let started = std::time::Instant::now();
    tracing::info!(server = %def.name, kind = %def.kind, timeout_secs = timeout.as_secs(), "testando MCP externo");
    let fut = async {
        match def.kind.as_str() {
            "stdio" => {
                let cmd_s = def.command.clone().ok_or("comando ausente")?;
                let mut cmd = tokio::process::Command::new(&cmd_s);
                cmd.args(&def.args).envs(&def.env).kill_on_drop(true);
                will_core::process::hide_console(&mut cmd);
                if let Some(cwd) = &def.cwd {
                    cmd.current_dir(cwd);
                }
                let (transport, stderr) = TokioChildProcess::builder(cmd)
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| format!("spawn: {e}"))?;
                let stderr_task = stderr.map(|err| {
                    tokio::spawn(async move {
                        use tokio::io::AsyncReadExt;
                        let mut s = String::new();
                        let mut err = err;
                        let _ = err.read_to_string(&mut s).await;
                        s
                    })
                });
                let client = match ().serve(transport).await {
                    Ok(c) => c,
                    Err(e) => {
                        let tail = match stderr_task {
                            Some(t) => tokio::time::timeout(Duration::from_millis(500), t)
                                .await
                                .ok()
                                .and_then(|r| r.ok())
                                .unwrap_or_default(),
                            None => String::new(),
                        };
                        return Err(format!(
                            "initialize: {e} {}",
                            will_core::tools::truncate(tail.trim(), 300)
                        ));
                    }
                };
                let info = client
                    .peer_info()
                    .and_then(|i| i.server_info.as_ref().map(|s| s.name.clone()));
                let tools = client
                    .list_all_tools()
                    .await
                    .map_err(|e| format!("tools/list: {e}"))?;
                let names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
                let _ = client.cancel().await;
                Ok::<_, String>((info, names))
            }
            "http" | "sse" => {
                let url = def.url.clone().ok_or("url ausente")?;
                let mut cfg = StreamableHttpClientTransportConfig::with_uri(url);
                let mut hm = std::collections::HashMap::new();
                for (k, v) in &def.headers {
                    if k.eq_ignore_ascii_case("authorization") {
                        // rmcp prefixa "Bearer " sozinho
                        let raw = v
                            .trim()
                            .strip_prefix("Bearer ")
                            .or_else(|| v.trim().strip_prefix("bearer "))
                            .unwrap_or(v.trim());
                        cfg = cfg.auth_header(raw.to_string());
                    } else if let (Ok(name), Ok(val)) = (
                        http::HeaderName::try_from(k.as_str()),
                        http::HeaderValue::try_from(v.as_str()),
                    ) {
                        hm.insert(name, val);
                    }
                }
                if !hm.is_empty() {
                    cfg = cfg.custom_headers(hm);
                }
                let client_http = reqwest::Client::builder()
                    .timeout(Duration::from_secs(20))
                    .build()
                    .map_err(|e| e.to_string())?;
                let transport = StreamableHttpClientTransport::with_client(client_http, cfg);
                let client = ().serve(transport).await.map_err(|e| format!("initialize: {e}"))?;
                let info = client
                    .peer_info()
                    .and_then(|i| i.server_info.as_ref().map(|s| s.name.clone()));
                let tools = client
                    .list_all_tools()
                    .await
                    .map_err(|e| format!("tools/list: {e}"))?;
                let names = tools.iter().map(|t| t.name.to_string()).collect();
                let _ = client.cancel().await;
                Ok((info, names))
            }
            other => Err(format!("tipo desconhecido: {other}")),
        }
    };
    let duration_ms = || started.elapsed().as_millis() as u64;
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok((server_name, tools))) => {
            tracing::info!(server = %def.name, tools = tools.len(), duration_ms = duration_ms(), "MCP externo respondeu");
            McpTestResult {
                at,
                ok: true,
                tools,
                server_name,
                error: None,
            }
        }
        Ok(Err(e)) => {
            tracing::warn!(server = %def.name, error = %e, duration_ms = duration_ms(), "MCP externo falhou no teste");
            McpTestResult {
                at,
                ok: false,
                tools: vec![],
                server_name: None,
                error: Some(e),
            }
        }
        Err(_) => {
            tracing::warn!(server = %def.name, timeout_secs = timeout.as_secs(), "MCP externo: tempo esgotado");
            McpTestResult {
                at,
                ok: false,
                tools: vec![],
                server_name: None,
                error: Some("tempo esgotado".into()),
            }
        }
    }
}
