//! Contrato de ferramentas expostas pelo servidor MCP interno (`will_ia`).

use crate::ids::ThreadId;
use crate::module::Module;
use crate::thread::Origin;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicy {
    Auto,
    Confirm,
    Off,
}

impl ToolPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            ToolPolicy::Auto => "auto",
            ToolPolicy::Confirm => "confirm",
            ToolPolicy::Off => "off",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(ToolPolicy::Auto),
            "confirm" => Some(ToolPolicy::Confirm),
            "off" => Some(ToolPolicy::Off),
            _ => None,
        }
    }
}

/// Descrição estática de uma ferramenta.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    /// Módulos que precisam estar ativos para a ferramenta aparecer.
    pub requires: Vec<Module>,
    pub default_policy: ToolPolicy,
    /// Família (telegram, whatsapp, calendar, gmail, core) para a UI.
    pub family: String,
}

/// Quem está chamando a ferramenta.
#[derive(Debug, Clone)]
pub struct ToolContext {
    pub thread_id: Option<ThreadId>,
    pub origin: Option<Origin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    /// Texto para o agente (normalmente JSON serializado ou texto legível).
    pub text: String,
    pub is_error: bool,
    /// Dados estruturados opcionais (vão em `structuredContent`).
    pub structured: Option<serde_json::Value>,
}

impl ToolOutput {
    pub fn text(t: impl Into<String>) -> Self {
        Self {
            text: t.into(),
            is_error: false,
            structured: None,
        }
    }
    pub fn json<S: Serialize>(v: &S) -> Self {
        let value = serde_json::to_value(v).unwrap_or(serde_json::Value::Null);
        Self {
            text: serde_json::to_string_pretty(&value).unwrap_or_default(),
            is_error: false,
            structured: Some(value),
        }
    }
    pub fn error(t: impl Into<String>) -> Self {
        Self {
            text: t.into(),
            is_error: true,
            structured: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("argumentos inválidos: {0}")]
    InvalidArgs(String),
    #[error("módulo indisponível: {0}")]
    Unavailable(String),
    #[error("não permitido: {0}")]
    Forbidden(String),
    #[error("ação não aprovada pelo usuário")]
    Denied,
    #[error("{0}")]
    Failed(String),
}

#[async_trait::async_trait]
pub trait WillTool: Send + Sync {
    fn spec(&self) -> ToolSpec;
    /// Resumo legível para a tela de aprovação (gerado pelo serviço, nunca pelo agente).
    fn approval_summary(&self, args: &serde_json::Value) -> String {
        format!("{} {}", self.spec().name, truncate(&args.to_string(), 300))
    }
    async fn call(
        &self,
        ctx: ToolContext,
        args: serde_json::Value,
    ) -> Result<ToolOutput, ToolError>;
}

/// Um módulo que contribui ferramentas.
pub trait ToolProvider: Send + Sync {
    fn family(&self) -> &'static str;
    fn tools(&self) -> Vec<Arc<dyn WillTool>>;
    /// Está apto a servir agora (módulo ligado e autorizado)?
    fn available(&self) -> bool;
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Helper para extrair campo obrigatório.
pub fn arg_str<'a>(args: &'a serde_json::Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ToolError::InvalidArgs(format!("campo obrigatório ausente: {key}")))
}

pub fn arg_opt_str<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
}

pub fn arg_opt_i64(args: &serde_json::Value, key: &str) -> Option<i64> {
    args.get(key).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

pub fn arg_opt_u64(args: &serde_json::Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
}

pub fn arg_opt_bool(args: &serde_json::Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}
