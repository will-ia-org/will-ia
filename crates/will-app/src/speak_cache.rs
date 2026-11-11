//! Cache do "ouvir": roteiro + áudio (PCM16) já gerados para um turno,
//! chaveados pelo id do turno (leitura literal: `<turno>-ler`). Ouvir de novo a mesma resposta não refaz o
//! resumo nem o TTS (zero tokens): toca o que já existe.
//!
//! Dois níveis: memória (vida do app) e disco em `<config>/will-ia-app/ouvir/`
//! (`<turn>.pcm` + `<turn>.json` com o roteiro e a taxa), para sobreviver a
//! reinícios. O disco é podado aos [`MAX_FILES`] mais recentes.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

/// Quantos áudios ficam em disco (os mais antigos saem).
const MAX_FILES: usize = 200;

/// Roteiro e áudio prontos de uma resposta.
pub struct SpeakAudio {
    pub summary: String,
    pub rate: u32,
    pub pcm: Vec<u8>,
}

#[derive(Default)]
pub struct SpeakCache {
    mem: HashMap<String, Arc<SpeakAudio>>,
    /// Ids com áudio em disco (lido uma vez; mantido por `put`/poda), para
    /// `has()` não ir ao disco a cada quadro.
    known: HashSet<String>,
    known_loaded: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Meta {
    summary: String,
    rate: u32,
}

fn dir() -> PathBuf {
    crate::profiles::config_dir().join("ouvir")
}

/// Só ids com caracteres seguros viram nome de arquivo.
fn safe_id(id: &str) -> Option<&str> {
    (!id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then_some(id)
}

impl SpeakCache {
    /// Roteiro já conhecido (memória ou disco), sem carregar o áudio.
    pub fn summary(&self, turn_id: &str) -> Option<String> {
        if let Some(a) = self.mem.get(turn_id) {
            return Some(a.summary.clone());
        }
        let id = safe_id(turn_id)?;
        let meta = std::fs::read_to_string(dir().join(format!("{id}.json"))).ok()?;
        serde_json::from_str::<Meta>(&meta).ok().map(|m| m.summary)
    }

    /// Já existe áudio guardado para esta chave (memória ou disco)?
    pub fn has(&mut self, turn_id: &str) -> bool {
        if self.mem.contains_key(turn_id) {
            return true;
        }
        self.load_known();
        self.known.contains(turn_id)
    }

    /// Lista o disco uma vez: ids com o par `.json` + `.pcm`.
    fn load_known(&mut self) {
        if self.known_loaded {
            return;
        }
        self.known_loaded = true;
        let d = dir();
        let Ok(rd) = std::fs::read_dir(&d) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                if p.with_extension("pcm").is_file() {
                    self.known.insert(stem.to_string());
                }
            }
        }
    }

    /// Áudio pronto do turno, da memória ou do disco (promovido à memória).
    pub fn get(&mut self, turn_id: &str) -> Option<Arc<SpeakAudio>> {
        if let Some(a) = self.mem.get(turn_id) {
            return Some(a.clone());
        }
        let id = safe_id(turn_id)?;
        let d = dir();
        let meta = std::fs::read_to_string(d.join(format!("{id}.json"))).ok()?;
        let meta: Meta = serde_json::from_str(&meta).ok()?;
        let pcm = std::fs::read(d.join(format!("{id}.pcm"))).ok()?;
        if pcm.len() < 2 {
            return None;
        }
        let a = Arc::new(SpeakAudio {
            summary: meta.summary,
            rate: meta.rate,
            pcm,
        });
        self.mem.insert(turn_id.to_string(), a.clone());
        Some(a)
    }

    /// Guarda em memória e, best-effort, em disco.
    pub fn put(&mut self, turn_id: &str, audio: Arc<SpeakAudio>) {
        self.mem.insert(turn_id.to_string(), audio.clone());
        let Some(id) = safe_id(turn_id) else {
            return;
        };
        let d = dir();
        if std::fs::create_dir_all(&d).is_err() {
            return;
        }
        let meta = Meta {
            summary: audio.summary.clone(),
            rate: audio.rate,
        };
        let _ = std::fs::write(d.join(format!("{id}.pcm")), &audio.pcm);
        if let Ok(j) = serde_json::to_string(&meta) {
            let _ = std::fs::write(d.join(format!("{id}.json")), j);
        }
        self.load_known();
        self.known.insert(id.to_string());
        for gone in prune(&d) {
            self.known.remove(&gone);
        }
    }
}

/// Mantém só os [`MAX_FILES`] pares mais recentes em disco; devolve os ids
/// apagados.
fn prune(d: &std::path::Path) -> Vec<String> {
    let mut removed = Vec::new();
    let Ok(rd) = std::fs::read_dir(d) else {
        return removed;
    };
    let mut entries: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            (p.extension().and_then(|x| x.to_str()) == Some("json")).then(|| {
                let t = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                (t, p)
            })
        })
        .collect();
    if entries.len() <= MAX_FILES {
        return removed;
    }
    entries.sort_by_key(|(t, _)| *t);
    for (_, json) in entries.iter().take(entries.len() - MAX_FILES) {
        let _ = std::fs::remove_file(json);
        let _ = std::fs::remove_file(json.with_extension("pcm"));
        if let Some(stem) = json.file_stem().and_then(|s| s.to_str()) {
            removed.push(stem.to_string());
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_ids() {
        assert_eq!(
            safe_id("01a18413-c705-77dc-b5d0-18842eaafdd6"),
            Some("01a18413-c705-77dc-b5d0-18842eaafdd6")
        );
        assert_eq!(safe_id(""), None);
        assert_eq!(safe_id("../x"), None);
        assert_eq!(safe_id("a b"), None);
    }
}
