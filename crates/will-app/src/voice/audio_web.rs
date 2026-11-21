//! Pipeline de áudio da voz no navegador (`docs/33`): a mesma interface do
//! `AudioPipeline` do desktop (`audio.rs`), sobre a Web Audio da página
//! (`window.willIaAudio`, definido no `index.html`). O microfone vai em PCM16
//! 24 kHz para o `VoiceLink`; a fala do assistente volta pelo mesmo caminho.
//! O cancelamento de eco é o do próprio navegador.

use crate::service::BgTask;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use will_client::{AudioOut, VoiceLink};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = start)]
    fn js_start(on_pcm: &js_sys::Function) -> js_sys::Promise;
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = play)]
    fn js_play(bytes: &[u8]);
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = stopPlayback)]
    fn js_stop_playback();
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = setMuted)]
    fn js_set_muted(muted: bool);
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = level)]
    fn js_level() -> f32;
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = playing)]
    fn js_playing() -> bool;
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = takeError)]
    fn js_take_error() -> String;
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = stop)]
    fn js_stop();
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = recStart)]
    fn js_rec_start() -> js_sys::Promise;
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = recLevel)]
    fn js_rec_level() -> f32;
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = recError)]
    fn js_rec_error() -> String;
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = recStop)]
    fn js_rec_stop() -> Vec<u8>;
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = clipLoad)]
    fn js_clip_load(bytes: &[u8], channels: u32, rate: u32) -> bool;
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = clipSetPlaying)]
    fn js_clip_set_playing(on: bool);
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = clipPlaying)]
    fn js_clip_playing() -> bool;
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = clipPos)]
    fn js_clip_pos() -> f64;
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = clipSeek)]
    fn js_clip_seek(secs: f64);
    #[wasm_bindgen(js_namespace = willIaAudio, js_name = clipClose)]
    fn js_clip_close();
}

/// Player das gravações no navegador: a mesma interface do `ClipPlayer` do
/// desktop (`clip_player.rs`), tocando pela Web Audio da página. Um por vez.
pub struct ClipPlayer {
    duration: f64,
}

impl ClipPlayer {
    /// Carrega o clipe e começa pausado.
    pub fn new(clip: super::clip_player::Clip, _output_name: Option<&str>) -> Result<Self, String> {
        let duration = clip.duration_secs();
        let mut bytes = Vec::with_capacity(clip.samples.len() * 2);
        for s in clip.samples.iter() {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        if !js_clip_load(&bytes, clip.channels as u32, clip.rate) {
            return Err("gravação sem áudio".into());
        }
        Ok(Self { duration })
    }

    pub fn playing(&self) -> bool {
        js_clip_playing()
    }

    pub fn set_playing(&self, on: bool) {
        js_clip_set_playing(on);
    }

    pub fn duration_secs(&self) -> f64 {
        self.duration
    }

    pub fn position_secs(&self) -> f64 {
        js_clip_pos()
    }

    pub fn seek_secs(&self, secs: f64) {
        js_clip_seek(secs);
    }
}

impl Drop for ClipPlayer {
    fn drop(&mut self) {
        js_clip_close();
    }
}

/// Gravador do botão "Falar" no navegador: a mesma interface do `Recorder` do
/// desktop. A página grava em memória e devolve PCM16 mono 16 kHz ao parar.
pub struct Recorder {
    /// Sempre `None`: no navegador o microfone abre depois (pedido de
    /// permissão) e o erro chega por [`Self::runtime_error`].
    pub error: Option<String>,
    level: Arc<std::sync::atomic::AtomicU32>,
    stopped: bool,
}

impl Recorder {
    pub fn start(_input_name: Option<&str>) -> Self {
        let _ = js_rec_start();
        Self {
            error: None,
            level: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            stopped: false,
        }
    }

    pub fn level_handle(&self) -> Arc<std::sync::atomic::AtomicU32> {
        self.level.clone()
    }

    /// Erro da captura, se houver. Chamado a cada quadro pelo ditado, então
    /// também é aqui que o nível do microfone é atualizado (não há thread de áudio).
    pub fn runtime_error(&self) -> Option<String> {
        self.level
            .store(js_rec_level().to_bits(), Ordering::Relaxed);
        Some(js_rec_error()).filter(|e| !e.is_empty())
    }

    /// Para a captura e devolve o WAV (ou `None` se gravou quase nada).
    pub fn stop(mut self) -> Option<Vec<u8>> {
        self.stopped = true;
        let bytes = js_rec_stop();
        let samples: Vec<i16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|s| i16::from_le_bytes(*s))
            .collect();
        if samples.len() < (super::audio::DICTATION_RATE as usize) / 4 {
            return None;
        }
        Some(super::audio::wav_pcm16_mono(
            &samples,
            super::audio::DICTATION_RATE,
        ))
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Descartado sem `stop` (ditado cancelado): fecha o microfone.
        if !self.stopped {
            let _ = js_rec_stop();
        }
    }
}

pub struct AudioPipeline {
    /// Microfone fechado pelo push-to-talk.
    muted: Arc<AtomicBool>,
    _on_pcm: Closure<dyn FnMut(js_sys::Uint8Array)>,
    out_task: BgTask,
    /// No navegador o microfone abre de forma assíncrona (o usuário responde
    /// ao pedido de permissão): os erros chegam depois, por [`Self::take_error`].
    pub input_error: Option<String>,
    pub output_error: Option<String>,
}

impl AudioPipeline {
    pub fn start(
        _rt: &tokio::runtime::Handle,
        link: Arc<VoiceLink>,
        _input_name: Option<&str>,
        _output_name: Option<&str>,
        start_muted: bool,
        barge_in: bool,
    ) -> Self {
        let muted = Arc::new(AtomicBool::new(start_muted));
        let m = muted.clone();
        let l = link.clone();
        let on_pcm =
            Closure::<dyn FnMut(js_sys::Uint8Array)>::new(move |pcm: js_sys::Uint8Array| {
                // Sem "interromper falando", o microfone fica fechado enquanto o
                // assistente fala (mesma regra do desktop).
                if m.load(Ordering::Relaxed) || (!barge_in && js_playing()) {
                    return;
                }
                l.audio_in(bytes::Bytes::from(pcm.to_vec()));
            });
        js_set_muted(false);
        // A promessa resolve quando o microfone abre (ou falha: `take_error`).
        let _ = js_start(on_pcm.as_ref().unchecked_ref());
        let mut rx = link.subscribe_audio_out();
        let out_task = BgTask::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(AudioOut::Pcm(b)) => js_play(&b),
                    Ok(AudioOut::Control(v)) => {
                        if v.get("type").and_then(|t| t.as_str()) == Some("stop_playback") {
                            js_stop_playback();
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
        Self {
            muted,
            _on_pcm: on_pcm,
            out_task,
            input_error: None,
            output_error: None,
        }
    }

    /// Erro da abertura do microfone, uma única vez (permissão negada, página sem HTTPS…).
    pub fn take_error(&self) -> Option<String> {
        Some(js_take_error()).filter(|e| !e.is_empty())
    }

    pub fn playing(&self) -> bool {
        js_playing()
    }

    pub fn stats(&self) -> (u64, u64) {
        (0, 0)
    }

    pub fn set_muted(&self, m: bool) {
        self.muted.store(m, Ordering::Relaxed);
    }

    pub fn level(&self) -> f32 {
        js_level()
    }
}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        self.out_task.abort();
        js_stop();
    }
}
