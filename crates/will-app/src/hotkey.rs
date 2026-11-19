//! Atalho global de voz (`global-hotkey`): Windows, macOS e X11. Em Wayland o
//! registro falha e o módulo fica desligado (aviso no log).

use crate::i18n::trf;
use crate::service::{Service, UiCommand};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::str::FromStr;
use std::sync::Arc;

/// Sistema operacional, para dar nome às teclas modificadoras.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyboard {
    Mac,
    Windows,
    Linux,
}

impl Keyboard {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Keyboard::Mac
        } else if cfg!(target_os = "windows") {
            Keyboard::Windows
        } else {
            Keyboard::Linux
        }
    }
}

/// Escreve um atalho com os nomes das teclas do teclado de `kb`. O texto
/// guardado nas configurações é o mesmo nos três sistemas (`Ctrl+Alt+R`); o
/// que muda é o nome impresso na tecla: no Mac, Alt é **Option (⌥)** e a tecla
/// "super" é **Command (⌘)**; no Windows é a tecla **Win**; no Linux, **Super**.
pub fn display_for(text: &str, kb: Keyboard) -> String {
    text.split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| {
            let lower = p.to_ascii_lowercase();
            let name = match (lower.as_str(), kb) {
                ("ctrl" | "control", Keyboard::Mac) => "Control (⌃)",
                ("ctrl" | "control", _) => "Ctrl",
                ("alt" | "option", Keyboard::Mac) => "Option (⌥)",
                ("alt" | "option", _) => "Alt",
                ("shift", Keyboard::Mac) => "Shift (⇧)",
                ("shift", _) => "Shift",
                ("cmd" | "command" | "super" | "win" | "meta", Keyboard::Mac) => "Command (⌘)",
                ("cmd" | "command" | "super" | "win" | "meta", Keyboard::Windows) => "Win",
                ("cmd" | "command" | "super" | "win" | "meta", Keyboard::Linux) => "Super",
                ("commandorcontrol" | "cmdorctrl", Keyboard::Mac) => "Command (⌘)",
                ("commandorcontrol" | "cmdorctrl", _) => "Ctrl",
                ("space" | "espaço" | "espaco", _) => "Espaço",
                _ => {
                    // `KeyR` → `R`, `Digit1` → `1`; o resto como foi escrito.
                    let key = p
                        .strip_prefix("Key")
                        .or_else(|| p.strip_prefix("Digit"))
                        .filter(|k| k.len() == 1)
                        .unwrap_or(p);
                    return key.to_ascii_uppercase();
                }
            };
            name.to_string()
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// [`display_for`] no teclado deste computador.
pub fn display(text: &str) -> String {
    display_for(text, Keyboard::current())
}

pub struct Hotkeys {
    manager: Option<GlobalHotKeyManager>,
    current: Option<HotKey>,
    pub error: Option<String>,
    /// Atalho do ditado ("Falar"): registro próprio, ao lado do da voz.
    dictation: Option<HotKey>,
    dictation_id: Arc<std::sync::atomic::AtomicU32>,
    pub dictation_error: Option<String>,
    /// Atalho da gravação de conversa: começa e, de novo, para.
    recording: Option<HotKey>,
    recording_id: Arc<std::sync::atomic::AtomicU32>,
    pub recording_error: Option<String>,
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
        let dictation_id = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let did = dictation_id.clone();
        let recording_id = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let rid = recording_id.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
            if let Some(s) = weak.upgrade() {
                let d = did.load(std::sync::atomic::Ordering::Relaxed);
                if d != 0 && e.id() == d {
                    if e.state() == HotKeyState::Pressed {
                        s.send_cmd(UiCommand::DictationToggle);
                    }
                    return;
                }
                let r = rid.load(std::sync::atomic::Ordering::Relaxed);
                if r != 0 && e.id() == r {
                    if e.state() == HotKeyState::Pressed {
                        tracing::info!("atalho da gravação pressionado");
                        s.send_cmd(UiCommand::RecordingToggle);
                    }
                    return;
                }
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
            dictation: None,
            dictation_id,
            dictation_error: None,
            recording: None,
            recording_id,
            recording_error: None,
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
                    self.error = Some(trf("não foi possível registrar '{}': {}", &[&text, &e]));
                    tracing::warn!(error = %e, hotkey = %norm, "registro do atalho global falhou");
                }
            },
            Err(e) => {
                self.error = Some(trf("atalho inválido '{}': {}", &[&text, &e]));
            }
        }
    }

    /// (Re)registra o atalho do ditado. Texto vazio = nenhum.
    pub fn apply_dictation(&mut self, text: &str) {
        let Some(m) = &self.manager else { return };
        if let Some(c) = self.dictation.take() {
            let _ = m.unregister(c);
        }
        self.dictation_id
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.dictation_error = None;
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let norm = Self::normalize(text);
        match HotKey::from_str(&norm) {
            Ok(hk) => match m.register(hk) {
                Ok(()) => {
                    tracing::info!(hotkey = %norm, "atalho do ditado registrado");
                    self.dictation_id
                        .store(hk.id(), std::sync::atomic::Ordering::Relaxed);
                    self.dictation = Some(hk);
                }
                Err(e) => {
                    self.dictation_error =
                        Some(trf("não foi possível registrar '{}': {}", &[&text, &e]));
                    tracing::warn!(error = %e, hotkey = %norm, "registro do atalho do ditado falhou");
                }
            },
            Err(e) => {
                self.dictation_error = Some(trf("atalho inválido '{}': {}", &[&text, &e]));
            }
        }
    }

    /// (Re)registra o atalho da gravação de conversa. Texto vazio = nenhum.
    pub fn apply_recording(&mut self, text: &str) {
        let Some(m) = &self.manager else { return };
        if let Some(c) = self.recording.take() {
            let _ = m.unregister(c);
        }
        self.recording_id
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.recording_error = None;
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let norm = Self::normalize(text);
        match HotKey::from_str(&norm) {
            Ok(hk) => match m.register(hk) {
                Ok(()) => {
                    tracing::info!(hotkey = %norm, "atalho da gravação registrado");
                    self.recording_id
                        .store(hk.id(), std::sync::atomic::Ordering::Relaxed);
                    self.recording = Some(hk);
                }
                Err(e) => {
                    self.recording_error =
                        Some(trf("não foi possível registrar '{}': {}", &[&text, &e]));
                    tracing::warn!(error = %e, hotkey = %norm, "registro do atalho da gravação falhou");
                }
            },
            Err(e) => {
                self.recording_error = Some(trf("atalho inválido '{}': {}", &[&text, &e]));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_is_written_with_the_keys_of_each_keyboard() {
        assert_eq!(
            display_for("Ctrl+Alt+R", Keyboard::Mac),
            "Control (⌃) + Option (⌥) + R"
        );
        assert_eq!(
            display_for("Ctrl+Alt+R", Keyboard::Windows),
            "Ctrl + Alt + R"
        );
        assert_eq!(
            display_for("Control+Alt+KeyR", Keyboard::Linux),
            "Ctrl + Alt + R"
        );
        assert_eq!(
            display_for("Cmd+Shift+Space", Keyboard::Mac),
            "Command (⌘) + Shift (⇧) + Espaço"
        );
        assert_eq!(display_for("Super+F1", Keyboard::Windows), "Win + F1");
        assert_eq!(
            display_for("CommandOrControl+Digit1", Keyboard::Linux),
            "Ctrl + 1"
        );
        assert_eq!(display_for("", Keyboard::Mac), "");
    }

    #[test]
    fn the_same_text_registers_on_every_system() {
        assert_eq!(Hotkeys::normalize("Ctrl+Alt+R"), "Control+Alt+KeyR");
        assert_eq!(Hotkeys::normalize("Control+Option+r"), "Control+Alt+KeyR");
    }
}
