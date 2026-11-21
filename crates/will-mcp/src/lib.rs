//! will-mcp — servidor MCP interno `will_ia` (Streamable HTTP em loopback),
//! registro de ferramentas com políticas de aprovação, identidade por bearer
//! efêmero e cliente para testar MCPs externos.

pub mod core_tools;
pub mod external;
pub mod registry;
pub mod server;
pub mod tokens;

pub use registry::{ToolListing, ToolRegistry};
pub use server::{CallRecord, McpServer, McpServerDeps};
pub use tokens::{Caller, TokenRegistry};

/// Redige valores sensíveis de um JSON de argumentos (para logs/aprovação).
pub fn redact_args(v: &serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(m) => serde_json::Value::Object(
            m.iter()
                .map(|(k, val)| {
                    let lk = k.to_lowercase();
                    if lk.contains("token")
                        || lk.contains("secret")
                        || lk.contains("password")
                        || lk.contains("authorization")
                        || lk == "key"
                        || lk.ends_with("_key")
                    {
                        (k.clone(), serde_json::Value::String("***".into()))
                    } else {
                        (k.clone(), redact_args(val))
                    }
                })
                .collect(),
        ),
        serde_json::Value::Array(a) => {
            serde_json::Value::Array(a.iter().map(redact_args).collect())
        }
        // Corta por caractere: `&s[..2000]` estourava quando o byte 2000 caía
        // no meio de um acento ou emoji e derrubava o servidor.
        serde_json::Value::String(s) if s.len() > 2000 => {
            serde_json::Value::String(will_core::tools::truncate(s, 2000))
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod redact_tests {
    use super::redact_args;

    /// O corte em 2000 caía no meio de um caractere de vários bytes (acento,
    /// emoji) e o servidor entrava em pânico ao redigir os argumentos.
    #[test]
    fn long_strings_are_cut_on_a_character_boundary() {
        for ch in ["á", "✅", "🔵"] {
            // 1999 bytes ASCII + caractere multibyte atravessando o byte 2000.
            let s = format!("{}{}", "a".repeat(1999), ch.repeat(50));
            let out = redact_args(&serde_json::json!({ "text": s }));
            let cut = out["text"].as_str().unwrap();
            assert!(cut.ends_with('…'));
            assert_eq!(cut.chars().count(), 2000);
        }
    }
}
