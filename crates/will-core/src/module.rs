use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Module {
    Engine,
    Telegram,
    Realtime,
    Mcp,
    WhatsApp,
    Google,
    Calendar,
    Gmail,
    Scheduler,
}

impl Module {
    pub fn as_str(&self) -> &'static str {
        match self {
            Module::Engine => "engine",
            Module::Telegram => "telegram",
            Module::Realtime => "realtime",
            Module::Mcp => "mcp",
            Module::WhatsApp => "whatsapp",
            Module::Google => "google",
            Module::Calendar => "calendar",
            Module::Gmail => "gmail",
            Module::Scheduler => "scheduler",
        }
    }
    pub fn all() -> &'static [Module] {
        &[
            Module::Engine,
            Module::Telegram,
            Module::Realtime,
            Module::Mcp,
            Module::WhatsApp,
            Module::Google,
            Module::Calendar,
            Module::Gmail,
            Module::Scheduler,
        ]
    }
}

impl std::fmt::Display for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Estado de um módulo, como mostrado nas bolinhas da UI.
///
/// Semântica (ver `will_desktop::ui::module_label`):
/// - `Running` (verde) = **pronto**: habilitado, configurado e operacional, mesmo
///   ocioso. Motor: token ativo + KB ok (com ou sem processos vivos). Voz:
///   habilitada + chave da OpenAI (em sessão ou não). Google: pelo menos uma conta
///   conectada com token válido. Agenda/Gmail: habilitado + uma conta ok com o
///   escopo necessário. Telegram: polling no ar. WhatsApp: uma conta conectada.
///   MCP: servidor ouvindo.
/// - `Starting` (azul) = iniciando / transição.
/// - `Degraded(motivo)` (amarelo) = habilitado, mas falta algo ou algo falha;
///   sempre com motivo legível ("sem token ativo", "nenhuma conta conectada"…).
/// - `Error(motivo)` (vermelho) = falhou ao subir / erro fatal, com motivo.
/// - `Disabled` (cinza) = desligado nas configurações.
/// - `Stopped` (cinza) = parado explicitamente (ex.: Telegram após `stop`).
///   **Não** usar para "ocioso mas pronto": isso é `Running`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum ModuleStatus {
    Disabled,
    Starting,
    Running,
    Degraded(String),
    Error(String),
    Stopped,
}

impl ModuleStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, ModuleStatus::Running | ModuleStatus::Degraded(_))
    }
}
