//! Gera `<kb>/.mcp.json` (will_ia + externos) com segredos como `${WILL_MCP_*}`,
//! grava `.claude/settings.local.json` e importa edições manuais.

use crate::app::AppState;
use secrecy::ExposeSecret;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use will_core::settings::{mcp_secret_env_name, ConfigValue, ExternalMcpServer};
use will_core::Settings;

fn cv_to_value(server: &str, key: &str, v: &ConfigValue) -> Value {
    match v {
        ConfigValue::Plain(s) => Value::String(s.clone()),
        ConfigValue::Secret { .. } => {
            Value::String(format!("${{{}}}", mcp_secret_env_name(server, key)))
        }
    }
}

/// JSON do `.mcp.json`.
pub fn render(settings: &Settings, mcp_url: &str, mcp_enabled: bool) -> Value {
    let mut servers = Map::new();
    if mcp_enabled && !mcp_url.is_empty() {
        servers.insert(
            "will_ia".into(),
            json!({"type": "http", "url": mcp_url, "headers": {"Authorization": "Bearer ${WILL_IA_MCP_TOKEN}"}}),
        );
    }
    for ext in settings
        .external_mcps
        .iter()
        .filter(|e| e.enabled && !e.name.is_empty())
    {
        let mut o = Map::new();
        match ext.kind.as_str() {
            "stdio" => {
                o.insert("type".into(), json!("stdio"));
                o.insert(
                    "command".into(),
                    json!(ext.command.clone().unwrap_or_default()),
                );
                if !ext.args.is_empty() {
                    o.insert("args".into(), json!(ext.args));
                }
                if !ext.env.is_empty() {
                    let env: Map<String, Value> = ext
                        .env
                        .iter()
                        .map(|(k, v)| (k.clone(), cv_to_value(&ext.name, k, v)))
                        .collect();
                    o.insert("env".into(), Value::Object(env));
                }
                if let Some(cwd) = &ext.cwd {
                    if !cwd.trim().is_empty() {
                        o.insert("cwd".into(), json!(cwd));
                    }
                }
            }
            kind => {
                o.insert("type".into(), json!(kind));
                o.insert("url".into(), json!(ext.url.clone().unwrap_or_default()));
                if !ext.headers.is_empty() {
                    let h: Map<String, Value> = ext
                        .headers
                        .iter()
                        .map(|(k, v)| (k.clone(), cv_to_value(&ext.name, k, v)))
                        .collect();
                    o.insert("headers".into(), Value::Object(h));
                }
            }
        }
        servers.insert(ext.name.clone(), Value::Object(o));
    }
    json!({ "mcpServers": servers })
}

fn hash(s: &str) -> String {
    let d = Sha256::digest(s.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Converte uma entrada de `mcpServers` (formato Claude Code) em `ExternalMcpServer`.
pub fn external_from_json(name: &str, v: &Value) -> Option<ExternalMcpServer> {
    let o = v.as_object()?;
    let mut e = ExternalMcpServer {
        name: name.to_string(),
        ..Default::default()
    };
    let kind = o
        .get("type")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if o.contains_key("url") {
                "http".into()
            } else {
                "stdio".into()
            }
        });
    e.kind = kind;
    e.command = o
        .get("command")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    e.args = o
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    e.cwd = o.get("cwd").and_then(|c| c.as_str()).map(|s| s.to_string());
    e.url = o.get("url").and_then(|c| c.as_str()).map(|s| s.to_string());
    let to_cv = |k: &str, val: &Value| -> ConfigValue {
        let s = val.as_str().unwrap_or_default().to_string();
        let lk = k.to_lowercase();
        if s.starts_with("${") {
            ConfigValue::Plain(s)
        } else if lk.contains("token")
            || lk.contains("secret")
            || lk.contains("key")
            || lk == "authorization"
            || lk.contains("password")
        {
            ConfigValue::Secret {
                secret: format!("mcp:{name}:{k}"),
            }
        } else {
            ConfigValue::Plain(s)
        }
    };
    if let Some(env) = o.get("env").and_then(|x| x.as_object()) {
        e.env = env.iter().map(|(k, v)| (k.clone(), to_cv(k, v))).collect();
    }
    if let Some(h) = o.get("headers").and_then(|x| x.as_object()) {
        e.headers = h.iter().map(|(k, v)| (k.clone(), to_cv(k, v))).collect();
    }
    Some(e)
}

/// Valores literais de segredos presentes num JSON importado (para migrar ao keyring).
pub fn literal_secrets(name: &str, v: &Value) -> Vec<(String, String)> {
    let mut out = vec![];
    let Some(o) = v.as_object() else { return out };
    for section in ["env", "headers"] {
        if let Some(m) = o.get(section).and_then(|x| x.as_object()) {
            for (k, val) in m {
                let lk = k.to_lowercase();
                let s = val.as_str().unwrap_or_default();
                if !s.starts_with("${")
                    && (lk.contains("token")
                        || lk.contains("secret")
                        || lk.contains("key")
                        || lk == "authorization"
                        || lk.contains("password"))
                    && !s.is_empty()
                {
                    out.push((format!("mcp:{name}:{k}"), s.to_string()));
                }
            }
        }
    }
    out
}

/// Resolve `WILL_MCP_*` a partir do keyring para os externos habilitados.
pub async fn external_env(state: &AppState) -> BTreeMap<String, String> {
    let settings = state.settings();
    let mut m = BTreeMap::new();
    for ext in settings.external_mcps.iter().filter(|e| e.enabled) {
        for (k, v) in ext.env.iter().chain(ext.headers.iter()) {
            if let ConfigValue::Secret { secret } = v {
                match state.secrets.get(secret).await {
                    Ok(Some(s)) => {
                        m.insert(
                            mcp_secret_env_name(&ext.name, k),
                            s.expose_secret().to_string(),
                        );
                    }
                    Ok(None) => {
                        tracing::warn!(server = %ext.name, key = %k, "segredo de MCP externo ausente no keyring")
                    }
                    Err(e) => {
                        tracing::warn!(server = %ext.name, key = %k, error = %e, "falha ao ler segredo")
                    }
                }
            }
        }
    }
    m
}

/// Importa servidores desconhecidos de um `mcpServers` para as settings (em memória). Retorna nomes importados.
pub async fn import_servers(state: &AppState, servers: &Map<String, Value>) -> Vec<String> {
    let mut imported = vec![];
    let known: Vec<String> = state
        .settings
        .read()
        .external_mcps
        .iter()
        .map(|e| e.name.clone())
        .collect();
    for (name, v) in servers {
        if name == "will_ia" || known.contains(name) {
            continue;
        }
        if let Some(e) = external_from_json(name, v) {
            for (key, val) in literal_secrets(name, v) {
                let _ = state
                    .secrets
                    .set(&key, &secrecy::SecretString::from(val))
                    .await;
            }
            state.settings.write().external_mcps.push(e);
            imported.push(name.clone());
        }
    }
    imported
}

/// Regrava `.mcp.json` e `settings.local.json` na KB (se configurada). Importa edições manuais antes.
pub async fn write(state: &AppState) -> Result<Option<PathBuf>, String> {
    let Some(kb) = state.kb_path() else {
        return Ok(None);
    };
    if !kb.is_dir() {
        return Ok(None);
    }
    let path = kb.join(".mcp.json");
    // edições manuais?
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let last = state.storage.mcp_json_hash().await.unwrap_or(None);
        if last.as_deref() != Some(&hash(&existing)) {
            if let Ok(Value::Object(o)) = serde_json::from_str::<Value>(&existing) {
                if let Some(Value::Object(servers)) = o.get("mcpServers") {
                    let imported = import_servers(state, servers).await;
                    if !imported.is_empty() {
                        tracing::info!(
                            ?imported,
                            "servidores importados do .mcp.json editado à mão"
                        );
                        state.bus.log(
                            "info",
                            "mcp",
                            format!("Importados do .mcp.json: {}", imported.join(", ")),
                        );
                        let _ = state.save_settings().await;
                    }
                }
            }
        }
    }
    let settings = state.settings();
    let mcp_enabled = settings.mcp.enabled;
    let url = state.mcp_url.read().clone();
    let v = render(&settings, &url, mcp_enabled);
    let text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())? + "\n";
    std::fs::write(&path, &text).map_err(|e| format!("gravar .mcp.json: {e}"))?;
    let _ = state.storage.set_mcp_json_hash(&hash(&text)).await;
    write_settings_local(&kb)?;
    Ok(Some(path))
}

fn write_settings_local(kb: &Path) -> Result<(), String> {
    let p = kb.join(".claude/settings.local.json");
    let mut v: Value = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    if let Value::Object(o) = &mut v {
        o.insert("enableAllProjectMcpServers".into(), json!(true));
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(
        &p,
        serde_json::to_string_pretty(&v).map_err(|e| e.to_string())? + "\n",
    )
    .map_err(|e| e.to_string())
}

/// Lê os MCPs do `~/.claude.json` (escopo user) como `mcpServers`.
pub fn read_claude_user_mcps() -> Result<Map<String, Value>, String> {
    let home = directories::BaseDirs::new()
        .ok_or("home não encontrada")?
        .home_dir()
        .to_path_buf();
    let p = home.join(".claude.json");
    let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let v: Value = serde_json::from_str(&s).map_err(|e| e.to_string())?;
    Ok(v.get("mcpServers")
        .and_then(|m| m.as_object())
        .cloned()
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_secret_refs_not_values() {
        let mut s = Settings::fresh();
        let mut e = ExternalMcpServer {
            name: "trello".into(),
            kind: "http".into(),
            url: Some("https://x/mcp".into()),
            ..Default::default()
        };
        e.headers.insert(
            "Authorization".into(),
            ConfigValue::Secret {
                secret: "mcp:trello:Authorization".into(),
            },
        );
        s.external_mcps.push(e);
        let v = render(&s, "http://127.0.0.1:1/mcp", true);
        let txt = v.to_string();
        assert!(txt.contains("${WILL_MCP_TRELLO_AUTHORIZATION}"));
        assert!(txt.contains("${WILL_IA_MCP_TOKEN}"));
        assert!(v["mcpServers"]["will_ia"]["url"] == "http://127.0.0.1:1/mcp");
    }

    #[test]
    fn imports_and_detects_literal_secrets() {
        let v = json!({"type":"stdio","command":"npx","args":["-y","x"],"env":{"GITHUB_TOKEN":"ghp_123","MODE":"a"}});
        let e = external_from_json("github", &v).unwrap();
        assert!(matches!(
            e.env.get("GITHUB_TOKEN"),
            Some(ConfigValue::Secret { .. })
        ));
        assert!(matches!(e.env.get("MODE"), Some(ConfigValue::Plain(_))));
        assert_eq!(
            literal_secrets("github", &v),
            vec![("mcp:github:GITHUB_TOKEN".to_string(), "ghp_123".to_string())]
        );
    }
}
