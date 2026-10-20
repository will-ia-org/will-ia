//! Erros do módulo Google e conversão para `ToolError` (mensagens acionáveis).

use will_core::ToolError;

#[derive(Debug, thiserror::Error)]
pub enum GoogleError {
    /// `client_id`/`client_secret` ausentes ou inválidos.
    #[error("configuração: {0}")]
    Config(String),
    /// Refresh token revogado/expirado (`invalid_grant`).
    #[error("conta {0} precisa reautorizar")]
    Reauthorize(String),
    /// Conta inexistente no banco.
    #[error("conta Google não encontrada: {0}")]
    AccountNotFound(String),
    #[error("http: {0}")]
    Http(String),
    /// Resposta de erro da API Google (status + mensagem/razão).
    #[error("Google API {status}: {message}")]
    Api {
        status: u16,
        reason: Option<String>,
        message: String,
    },
    #[error("banco: {0}")]
    Storage(String),
    #[error("segredo: {0}")]
    Secret(String),
    #[error("io: {0}")]
    Io(String),
    #[error("{0}")]
    Invalid(String),
    #[error("fluxo OAuth: {0}")]
    OAuth(String),
}

impl From<will_storage::StorageError> for GoogleError {
    fn from(e: will_storage::StorageError) -> Self {
        GoogleError::Storage(e.to_string())
    }
}

impl From<reqwest::Error> for GoogleError {
    fn from(e: reqwest::Error) -> Self {
        // `reqwest::Error` não inclui o corpo/URL com segredos, mas mantemos curto.
        GoogleError::Http(e.without_url().to_string())
    }
}

impl From<std::io::Error> for GoogleError {
    fn from(e: std::io::Error) -> Self {
        GoogleError::Io(e.to_string())
    }
}

impl From<GoogleError> for ToolError {
    fn from(e: GoogleError) -> Self {
        match e {
            GoogleError::Config(m) => ToolError::Unavailable(format!("Google não configurado: {m}")),
            GoogleError::Reauthorize(acc) => ToolError::Failed(format!(
                "conta Google {acc} precisa reautorizar (Configurações → Conta Google → Reautorizar)"
            )),
            GoogleError::AccountNotFound(a) => ToolError::InvalidArgs(format!("conta Google não encontrada: {a}")),
            GoogleError::Invalid(m) => ToolError::InvalidArgs(m),
            GoogleError::Api { status: 403, reason, message } => {
                let r = reason.unwrap_or_default();
                if r.contains("insufficientPermissions") || r == "forbidden" || message.contains("insufficient") {
                    ToolError::Forbidden(format!(
                        "escopo insuficiente na conta Google ({message}); reautorize concedendo a permissão necessária"
                    ))
                } else {
                    ToolError::Forbidden(message)
                }
            }
            GoogleError::Api { status: 404, message, .. } => {
                ToolError::Failed(format!("não encontrado no Google: {message}"))
            }
            GoogleError::Api { status, message, .. } => ToolError::Failed(format!("Google API {status}: {message}")),
            other => ToolError::Failed(other.to_string()),
        }
    }
}

impl From<String> for GoogleError {
    fn from(s: String) -> Self {
        GoogleError::Invalid(s)
    }
}
