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
        ]
    }
}

impl std::fmt::Display for Module {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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
