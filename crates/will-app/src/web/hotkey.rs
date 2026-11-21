//! Versão do navegador de `hotkey.rs`: uma página não registra atalhos
//! globais do sistema. A mesma interface, sem efeito.

use crate::service::Service;
use std::sync::Arc;

/// Texto do atalho como vem das configurações (sem o mapa de teclado do SO).
pub fn display(text: &str) -> String {
    text.to_string()
}

#[derive(Default)]
pub struct Hotkeys {
    pub error: Option<String>,
    pub dictation_error: Option<String>,
    pub recording_error: Option<String>,
}

impl Hotkeys {
    pub fn new(_svc: &Arc<Service>) -> Self {
        Self::default()
    }
    pub fn apply(&mut self, _text: &str) {}
    pub fn apply_dictation(&mut self, _text: &str) {}
    pub fn apply_recording(&mut self, _text: &str) {}
}
