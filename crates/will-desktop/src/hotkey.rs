//! Atalho global de voz (`global-hotkey`): Windows, macOS e X11. Em Wayland o
//! registro falha e o módulo fica desligado (aviso no log).

use crate::service::{Service, UiCommand};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::str::FromStr;
use std::sync::Arc;

pub struct Hotkeys {
    manager: Option<GlobalHotKeyManager>,
    current: Option<HotKey>,
    pub error: Option<String>,
}

impl Hotkeys {
    pub fn new(svc: &Arc<Service>) -> Self {
        let manager = match GlobalHotKeyManager::new() {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!(error = %e, "atalho global indisponível");
                None
            }
        };
        let weak = Arc::downgrade(svc);
        GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
            if let Some(s) = weak.upgrade() {
                match e.state() {
                    HotKeyState::Pressed => s.send_cmd(UiCommand::HotkeyDown),
                    HotKeyState::Released => s.send_cmd(UiCommand::HotkeyUp),
                }
            }
        }));
        Self {
            manager,
            current: None,
            error: None,
        }
    }

    /// Normaliza o texto do usuário: `Ctrl+Shift+Space`, `Cmd+Alt+V`, `CommandOrControl+Space`.
    fn normalize(s: &str) -> String {
        s.split('+')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .map(|p| match p.to_ascii_lowercase().as_str() {
                "space" | "espaço" | "espaco" => "Space".to_string(),
                "ctrl" | "control" => "Control".to_string(),
                "cmd" | "command" | "super" | "win" | "meta" => "Super".to_string(),
                "commandorcontrol" | "cmdorctrl" => "CommandOrControl".to_string(),
                "alt" | "option" => "Alt".to_string(),
                "shift" => "Shift".to_string(),
                other => {
                    // teclas de uma letra → KeyX; dígitos → DigitN; demais passam direto
                    if other.len() == 1 && other.chars().all(|c| c.is_ascii_alphabetic()) {
                        format!("Key{}", other.to_ascii_uppercase())
                    } else if other.len() == 1 && other.chars().all(|c| c.is_ascii_digit()) {
                        format!("Digit{other}")
                    } else {
                        p.to_string()
                    }
                }
            })
            .collect::<Vec<_>>()
            .join("+")
    }

    /// (Re)registra o atalho. Texto vazio = nenhum.
    pub fn apply(&mut self, text: &str) {
        let Some(m) = &self.manager else { return };
        if let Some(c) = self.current.take() {
            let _ = m.unregister(c);
        }
        self.error = None;
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let norm = Self::normalize(text);
        match HotKey::from_str(&norm) {
            Ok(hk) => match m.register(hk) {
                Ok(()) => {
                    tracing::info!(hotkey = %norm, "atalho global registrado");
                    self.current = Some(hk);
                }
                Err(e) => {
                    self.error = Some(format!("não foi possível registrar '{text}': {e}"));
                    tracing::warn!(error = %e, hotkey = %norm, "registro do atalho global falhou");
                }
            },
            Err(e) => {
                self.error = Some(format!("atalho inválido '{text}': {e}"));
            }
        }
    }
}
