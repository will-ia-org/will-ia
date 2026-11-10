//! Preferências locais da interface (por máquina/usuário do app, não do
//! servidor): `ui-prefs.json` na pasta de configuração do app, ao lado de
//! `profiles.json`. Hoje guarda o estado do Gantt de Tarefas (largura da lista,
//! zoom, posição e o toggle de concluídas) para reabrir onde parou.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiPrefs {
    pub gantt: GanttPrefs,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GanttPrefs {
    /// Largura da lista de títulos (px).
    pub list_w: Option<f32>,
    /// Zoom (px por hora).
    pub px_per_hour: Option<f32>,
    /// Hora local (desde a época) na borda esquerda; `None` = seguir o "agora".
    pub view_start: Option<f64>,
    pub show_done: bool,
}

fn file() -> PathBuf {
    crate::profiles::config_dir().join("ui-prefs.json")
}

pub fn load() -> UiPrefs {
    std::fs::read_to_string(file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(p: &UiPrefs) -> Result<(), String> {
    let dir = crate::profiles::config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(p).map_err(|e| e.to_string())?;
    std::fs::write(file(), text).map_err(|e| e.to_string())
}

/// Atualiza só o bloco do Gantt, preservando o resto do arquivo.
pub fn save_gantt(g: &GanttPrefs) -> Result<(), String> {
    let mut p = load();
    p.gantt = g.clone();
    save(&p)
}
