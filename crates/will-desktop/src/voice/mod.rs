//! Controle de voz no app: sessão Realtime + áudio cpal + indicador flutuante.

pub mod audio;
pub mod overlay;

use crate::service::{j, Service, Task};
use audio::AudioPipeline;
use overlay::Shared;
use serde_json::Value;
use std::sync::Arc;
use will_core::settings::RealtimeSettings;
use will_realtime::RealtimeManager;

pub struct VoiceController {
    pub overlay: Shared,
    pipeline: Option<AudioPipeline>,
    pub session_active: bool,
    pub state: String,
    pub detail: Option<String>,
    pub ptt_down: bool,
    pub ptt_mode: bool,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    pub barge_in: bool,
    pub sound_cues: bool,
    pub last_error: Option<String>,
    pub live_transcripts: Vec<(String, String)>,
    start_task: Option<Task<Value>>,
    stop_task: Option<Task<Value>>,
    level_smooth: f32,
    /// Mensagens de controle da sessão (`transcript`, `state`) vindas do `RealtimeManager`.
    ctrl_rx: Option<std::sync::mpsc::Receiver<Value>>,
    ctrl_task: Option<tokio::task::JoinHandle<()>>,
    last_stats: (u64, u64),
}

impl VoiceController {
    pub fn new() -> Self {
        Self {
            overlay: Shared::default(),
            pipeline: None,
            session_active: false,
            state: "stopped".into(),
            detail: None,
            ptt_down: false,
            ptt_mode: false,
            input_device: None,
            output_device: None,
            barge_in: false,
            sound_cues: true,
            last_error: None,
            live_transcripts: Vec::new(),
            start_task: None,
            stop_task: None,
            level_smooth: 0.0,
            ctrl_rx: None,
            ctrl_task: None,
            last_stats: (0, 0),
        }
    }

    /// (underruns, deltas perdidos) da sessão atual.
    pub fn audio_stats(&self) -> (u64, u64) {
        self.pipeline.as_ref().map(|p| p.stats()).unwrap_or((0, 0))
    }

    fn subscribe_controls(&mut self, svc: &Arc<Service>, m: &Arc<RealtimeManager>) {
        if let Some(t) = self.ctrl_task.take() {
            t.abort();
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let mut sub = m.subscribe_audio_out();
        let weak = Arc::downgrade(svc);
        let task = svc.rt().spawn(async move {
            loop {
                match sub.recv().await {
                    Ok(will_realtime::AudioOut::Control(v)) => {
                        if tx.send(v).is_err() {
                            break;
                        }
                        if let Some(s) = weak.upgrade() {
                            s.repaint();
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });
        self.ctrl_rx = Some(rx);
        self.ctrl_task = Some(task);
    }

    fn drain_controls(&mut self) {
        let Some(rx) = &self.ctrl_rx else { return };
        let mut items = Vec::new();
        while let Ok(v) = rx.try_recv() {
            items.push(v);
        }
        for v in items {
            if v.get("type").and_then(|t| t.as_str()) == Some("transcript") {
                let role = v
                    .get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or("assistant")
                    .to_string();
                let text = v
                    .get("text")
                    .and_then(|r| r.as_str())
                    .unwrap_or("")
                    .to_string();
                if !text.is_empty() {
                    self.overlay.lock().last_transcript = Some(text.clone());
                    self.live_transcripts.push((role, text));
                    if self.live_transcripts.len() > 60 {
                        self.live_transcripts.remove(0);
                    }
                }
            }
        }
    }

    pub fn apply_settings(&mut self, s: &RealtimeSettings) {
        self.ptt_mode = s.activation == "push_to_talk";
        self.input_device = s.input_device.clone();
        self.output_device = s.output_device.clone();
        self.barge_in = s.barge_in;
        self.sound_cues = s.sound_cues;
        self.overlay.lock().ptt_mode = self.ptt_mode;
    }

    pub fn busy(&self) -> bool {
        self.start_task.as_ref().is_some_and(|t| t.pending())
            || self.stop_task.as_ref().is_some_and(|t| t.pending())
    }

    fn manager(svc: &Arc<Service>) -> Option<Arc<RealtimeManager>> {
        svc.state.realtime.read().clone()
    }

    /// Abre a sessão (no serviço) e o áudio (aqui).
    pub fn start(&mut self, svc: &Arc<Service>) {
        if self.busy() {
            return;
        }
        self.last_error = None;
        self.start_task = Some(svc.call(|st| async move {
            j(will_server::api::integrations::voice_start(axum::extract::State(st)).await)
        }));
    }

    fn cue(&self, notes: &'static [(f32, u32)]) {
        if !self.sound_cues {
            return;
        }
        let out = self.output_device.clone();
        std::thread::spawn(move || {
            if let Err(e) = audio::play_cue(out.as_deref(), notes, 0.2) {
                tracing::debug!(error = %e, "sinal sonoro falhou");
            }
        });
    }

    pub fn stop(&mut self, svc: &Arc<Service>) {
        let was_active = self.session_active || self.pipeline.is_some();
        self.pipeline = None;
        if was_active {
            self.cue(audio::CUE_OFF);
        }
        self.ptt_down = false;
        if let Some(t) = self.ctrl_task.take() {
            t.abort();
        }
        self.ctrl_rx = None;
        self.stop_task = Some(svc.call(|st| async move {
            j(will_server::api::integrations::voice_stop(axum::extract::State(st)).await)
        }));
    }

    pub fn toggle(&mut self, svc: &Arc<Service>) {
        if self.session_active || self.pipeline.is_some() {
            self.stop(svc);
        } else {
            self.start(svc);
        }
    }

    /// Push-to-talk: pressionar abre o microfone e avisa o serviço; soltar envia.
    pub fn set_ptt(&mut self, svc: &Arc<Service>, down: bool) {
        if !self.session_active {
            return;
        }
        self.ptt_down = down;
        self.overlay.lock().ptt_down = down;
        if let Some(p) = &self.pipeline {
            p.set_muted(self.ptt_mode && !down);
        }
        if let Some(m) = Self::manager(svc) {
            m.ptt(down);
        }
    }

    pub fn commit(&self, svc: &Arc<Service>) {
        if let Some(m) = Self::manager(svc) {
            m.commit();
        }
    }

    pub fn mic_level(&self) -> f32 {
        self.level_smooth
    }

    /// O alto-falante está tocando fala do assistente (microfone silenciado se barge-in desligado).
    pub fn assistant_playing(&self) -> bool {
        self.pipeline.as_ref().is_some_and(|p| p.playing())
    }

    pub fn audio_errors(&self) -> Vec<String> {
        let mut v = vec![];
        if let Some(p) = &self.pipeline {
            v.extend(p.input_error.clone());
            v.extend(p.output_error.clone());
        }
        v
    }

    /// Chamado a cada quadro: consome tarefas, atualiza nível e indicador.
    /// Devolve `Some(msg)` para a GUI mostrar um toast.
    pub fn tick(&mut self, svc: &Arc<Service>) -> Option<Result<String, String>> {
        let mut out = None;
        if let Some(t) = &self.start_task {
            if let Some(r) = t.take() {
                self.start_task = None;
                match r {
                    Ok(_) => {
                        self.session_active = true;
                        if let Some(m) = Self::manager(svc) {
                            self.subscribe_controls(svc, &m);
                            let p = AudioPipeline::start(
                                svc.rt(),
                                m,
                                self.input_device.as_deref(),
                                self.output_device.as_deref(),
                                self.ptt_mode,
                                self.barge_in,
                            );
                            self.cue(audio::CUE_ON);
                            let errs: Vec<String> = p
                                .input_error
                                .iter()
                                .chain(p.output_error.iter())
                                .cloned()
                                .collect();
                            self.pipeline = Some(p);
                            out = Some(if errs.is_empty() {
                                Ok("Sessão de voz iniciada.".into())
                            } else {
                                Err(format!(
                                    "Sessão aberta, mas o áudio falhou: {}",
                                    errs.join("; ")
                                ))
                            });
                        }
                    }
                    Err(e) => {
                        self.last_error = Some(e.clone());
                        self.cue(audio::CUE_ERROR);
                        out = Some(Err(e));
                    }
                }
            }
        }
        if let Some(t) = &self.stop_task {
            if let Some(r) = t.take() {
                self.stop_task = None;
                self.session_active = false;
                self.state = "stopped".into();
                out = Some(r.map(|_| "Sessão de voz encerrada.".into()));
            }
        }
        self.drain_controls();
        let stats = self.audio_stats();
        if stats != self.last_stats {
            tracing::warn!(
                underruns = stats.0,
                lagged = stats.1,
                "áudio: falhas de reprodução"
            );
            self.last_stats = stats;
        }
        if let Some(p) = &self.pipeline {
            let l = p.level();
            self.level_smooth = self.level_smooth * 0.7 + l * 0.3;
        } else {
            self.level_smooth = 0.0;
        }
        {
            let mut o = self.overlay.lock();
            o.level = self.level_smooth;
            o.state = self.state.clone();
            o.detail = self.detail.clone();
            o.ptt_down = self.ptt_down;
        }
        out
    }

    /// Evento `Voice{state, detail}` do barramento.
    pub fn on_voice_event(&mut self, state: &str, detail: Option<&str>) {
        match state {
            "stopped" => {
                if self.session_active || self.pipeline.is_some() {
                    self.cue(audio::CUE_OFF);
                }
                self.session_active = false;
                self.pipeline = None;
                self.ptt_down = false;
            }
            "error" => {
                self.last_error = detail.map(String::from);
            }
            _ => {}
        }
        self.state = state.to_string();
        self.detail = detail.map(String::from);
    }

    pub fn overlay_visible(&self) -> bool {
        self.session_active || self.pipeline.is_some()
    }
}
