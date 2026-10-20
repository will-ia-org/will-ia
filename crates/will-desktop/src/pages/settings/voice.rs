//! Seção Voz: painel da sessão (iniciar/parar, segurar para falar, nível,
//! transcrições) e configuração da OpenAI Realtime.

use super::SettingsState;
use crate::pages::Cx;
use crate::service::{j, Loader};
use crate::ui::{self, SecretAction, SecretField, Tone};
use crate::voice::audio;
use axum::extract::{Query, State};
use serde_json::Value;
use will_core::Event;
use will_server::api::integrations as api;

#[derive(Default)]
pub struct VoicePage {
    transcripts: Loader<Value>,
    devices: Option<audio::DeviceLists>,
    mic_test: Option<audio::InputMonitor>,
    status: Loader<Value>,
    mic_level_smooth: f32,
    mic_peak: f32,
    api_key: SecretField,
    ptt_button_down: bool,
}

impl VoicePage {
    pub fn on_event(&mut self, ev: &Event) {
        if let Event::Voice { state, .. } = ev {
            if state == "stopped" || state == "listening" {
                self.transcripts.loaded_at = None;
            }
            if state == "stopped" {
                self.status.loaded_at = None;
            }
        }
        if matches!(ev, Event::CostRecorded { .. }) {
            self.status.loaded_at = None;
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        if self.transcripts.stale(std::time::Duration::from_secs(30)) {
            self.transcripts.start(cx.svc.call(|s| async move {
                j(api::voice_transcripts(State(s), Query(api::Limit { limit: Some(50) })).await)
            }));
        }
        self.transcripts.poll();
        if self.status.stale(std::time::Duration::from_secs(60)) {
            self.status.start(
                cx.svc
                    .call(|s| async move { j(api::voice_status(State(s)).await) }),
            );
        }
        self.status.poll();
        if self.devices.is_none() {
            self.devices = Some(audio::list_devices());
        }

        ui::heading(ui, "Voz (OpenAI Realtime)", Some("Conversa por voz pelo microfone e alto-falante deste computador; tarefas longas vão para o cérebro via ask_brain."));

        // ---- painel da sessão ----
        ui::section(ui, "Sessão de voz", |ui| {
            let v = &mut *cx.voice;
            ui.horizontal(|ui| {
                let enabled = st.saved.realtime.enabled;
                if v.session_active {
                    if ui
                        .add_enabled(
                            !v.busy(),
                            egui::Button::new(
                                egui::RichText::new("Parar").color(egui::Color32::WHITE),
                            )
                            .fill(Tone::Danger.color(true)),
                        )
                        .clicked()
                    {
                        v.stop(cx.svc);
                    }
                } else if ui
                    .add_enabled(
                        !v.busy() && enabled,
                        egui::Button::new(
                            egui::RichText::new("Iniciar").color(egui::Color32::WHITE),
                        )
                        .fill(Tone::Success.color(true)),
                    )
                    .clicked()
                {
                    v.start(cx.svc);
                }
                if v.busy() {
                    ui.spinner();
                }
                let (label, color) = crate::voice::overlay::label_for(&v.state);
                ui::badge(
                    ui,
                    if v.session_active { label } else { "inativa" },
                    if v.session_active {
                        Tone::Success
                    } else {
                        Tone::Neutral
                    },
                );
                if let Some(d) = &v.detail {
                    ui.label(egui::RichText::new(d).small().weak());
                }
                let _ = color;
                if !enabled {
                    ui.label(
                        egui::RichText::new("habilite e salve a configuração abaixo")
                            .weak()
                            .small(),
                    );
                }
            });
            for e in v.audio_errors() {
                ui::error_text(ui, &e);
            }
            if let Some(e) = &v.last_error {
                ui::error_text(ui, e);
            }
            {
                let (u, l) = v.audio_stats();
                if u > 0 || l > 0 {
                    ui.colored_label(Tone::Warning.color(true), format!("falhas de áudio nesta sessão: {u} esvaziamento(s) da fila, {l} delta(s) perdido(s)"));
                }
            }
            if let Some(last) = self
                .status
                .data
                .as_ref()
                .and_then(|s| s.get("last_session"))
                .filter(|l| !l.is_null())
            {
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .corner_radius(8.0)
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(egui::RichText::new("Última sessão").weak().small());
                        ui::wrapped_text(ui, &ui::s(last, "summary"));
                        if ui.button("Ver custos").clicked() {
                            *cx.go = Some(crate::pages::Nav::Costs);
                        }
                    });
            }
            if v.assistant_playing() && !v.barge_in {
                ui.label(
                    egui::RichText::new("assistente falando: microfone silenciado até terminar")
                        .small()
                        .weak(),
                );
            }
            // nível do microfone
            let level = (v.mic_level() * 6.0).clamp(0.0, 1.0);
            ui.add(
                egui::ProgressBar::new(level)
                    .desired_width(ui.available_width())
                    .fill(Tone::Success.color(true)),
            );
            if v.session_active {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(80));
            }
            ui.horizontal(|ui| {
                let btn = egui::Button::new(
                    egui::RichText::new(if self.ptt_button_down {
                        "Falando…"
                    } else {
                        "Segurar para falar"
                    })
                    .color(egui::Color32::WHITE),
                )
                .fill(if self.ptt_button_down {
                    Tone::Danger.color(true)
                } else {
                    Tone::Info.color(true)
                })
                .min_size(egui::vec2(200.0, 36.0));
                let r = ui.add_enabled(v.session_active, btn);
                let down = r.is_pointer_button_down_on();
                if down != self.ptt_button_down {
                    self.ptt_button_down = down;
                    v.set_ptt(cx.svc, down);
                }
                if ui
                    .add_enabled(v.session_active, egui::Button::new("Enviar agora"))
                    .clicked()
                {
                    v.commit(cx.svc);
                }
                if v.ptt_mode {
                    ui.label(
                        egui::RichText::new(
                            "Modo push-to-talk: o microfone só abre segurando o botão ou o atalho.",
                        )
                        .weak()
                        .small(),
                    );
                } else {
                    ui.label(
                        egui::RichText::new(
                            "Modo alternar: microfone aberto; o servidor detecta o fim da fala.",
                        )
                        .weak()
                        .small(),
                    );
                }
            });
            ui.add_space(6.0);
            ui.label(egui::RichText::new("Transcrições").strong());
            egui::ScrollArea::vertical()
                .id_salt("voice-transcripts")
                .max_height(220.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if let Some(list) = self.transcripts.data.as_ref().and_then(|v| v.as_array()) {
                        for t in list.iter().rev() {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new(ui::fmt_dt(&t["at"])).small().weak());
                                let role = ui::s(t, "role");
                                ui.label(
                                    egui::RichText::new(if role == "user" {
                                        "você"
                                    } else {
                                        "assistente"
                                    })
                                    .small()
                                    .color(
                                        if role == "user" {
                                            Tone::Info.color(true)
                                        } else {
                                            Tone::Success.color(true)
                                        },
                                    ),
                                );
                                ui::wrapped_text(ui, &ui::s(t, "text"));
                            });
                        }
                    }
                    for (role, text) in &v.live_transcripts {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new("agora").small().weak());
                            ui.label(
                                egui::RichText::new(if role == "user" {
                                    "você"
                                } else {
                                    "assistente"
                                })
                                .small()
                                .color(if role == "user" {
                                    Tone::Info.color(true)
                                } else {
                                    Tone::Success.color(true)
                                }),
                            );
                            ui::wrapped_text(ui, text);
                        });
                    }
                    if self
                        .transcripts
                        .data
                        .as_ref()
                        .and_then(|v| v.as_array())
                        .map(|a| a.is_empty())
                        .unwrap_or(true)
                        && v.live_transcripts.is_empty()
                    {
                        ui.label(egui::RichText::new("Nenhuma transcrição ainda.").weak());
                    }
                });
        });

        // ---- configuração ----
        ui::section(ui, "Configuração", |ui| {
            let r = &mut st.draft.realtime;
            ui::field(ui, "Habilitar", None, |ui| {
                ui::toggle(ui, &mut r.enabled);
            });
            let present = st
                .secrets_present
                .get("openai_api_key")
                .copied()
                .unwrap_or(false);
            let mut action = SecretAction::None;
            ui::field(ui, "OpenAI API key", Some("Vai para o cofre de segredos; usada pela voz e pela transcrição de áudios do Telegram."), |ui| {
                action = self.api_key.ui(ui, present, "sk-…");
            });
            match action {
                SecretAction::Save(v) => st.set_secret(cx.svc, "openai_api_key", v),
                SecretAction::Delete => st.delete_secret(cx.svc, "openai_api_key"),
                SecretAction::None => {}
            }
            let r = &mut st.draft.realtime;
            ui::field(ui, "Modelo Realtime", None, |ui| {
                ui::text_input(ui, &mut r.model, "gpt-realtime");
            });
            ui::field(ui, "Modelo de transcrição", None, |ui| {
                ui::text_input(ui, &mut r.transcription_model, "gpt-4o-mini-transcribe");
            });
            ui::field(ui, "Voz", None, |ui| {
                let voices = [
                    "alloy", "ash", "ballad", "coral", "echo", "sage", "shimmer", "verse", "marin",
                    "cedar",
                ];
                let opts: Vec<(&str, &str)> = voices.iter().map(|v| (*v, *v)).collect();
                ui::select(ui, "voice", &mut r.voice, &opts);
            });
            ui::field(ui, "Modo de ativação", None, |ui| {
                ui::select(
                    ui,
                    "activation",
                    &mut r.activation,
                    &[
                        ("toggle", "Alternar (liga/desliga)"),
                        ("push_to_talk", "Push-to-talk (segurar)"),
                    ],
                );
            });
            ui::field(
                ui,
                "Atalho global",
                Some("Ex.: Ctrl+Shift+Space, CommandOrControl+Alt+V. Não funciona em Wayland."),
                |ui| {
                    ui::text_input(ui, &mut r.hotkey, "Ctrl+Shift+Space");
                },
            );
            ui::field(
                ui,
                "Sinais sonoros",
                Some("Toca um bipe curto ao ligar e desligar a voz e em erro."),
                |ui| {
                    ui::toggle(ui, &mut r.sound_cues);
                },
            );
            ui::field(
                ui,
                "Interromper por voz (barge-in)",
                Some("Desligado: o microfone fica mudo enquanto o assistente fala, evitando que o alto-falante corte a resposta. Ligue só com fone de ouvido."),
                |ui| {
                    ui::toggle(ui, &mut r.barge_in);
                },
            );
            ui::field(
                ui,
                "Sensibilidade do detector de fala",
                Some("Maior = exige voz mais alta para considerar que você falou (0.3 sensível · 0.9 exigente)."),
                |ui| {
                    ui.add(egui::Slider::new(&mut r.vad_threshold, 0.1..=0.95).step_by(0.05));
                },
            );
            ui::field(ui, "Detecção de turno", None, |ui| {
                ui::select(
                    ui,
                    "vad",
                    &mut r.turn_detection,
                    &[
                        ("server_vad", "server_vad"),
                        ("semantic_vad", "semantic_vad"),
                    ],
                );
            });
            if let Some(d) = &self.devices {
                ui::field(ui, "Microfone", Some("Vazio = padrão do sistema."), |ui| {
                    let mut v = r.input_device.clone().unwrap_or_default();
                    let mut opts: Vec<(&str, &str)> = vec![("", "padrão do sistema")];
                    opts.extend(d.inputs.iter().map(|s| (s.as_str(), s.as_str())));
                    if ui::select(ui, "indev", &mut v, &opts) {
                        r.input_device = if v.is_empty() { None } else { Some(v) };
                    }
                });
                ui::field(
                    ui,
                    "Saída de áudio",
                    Some("Vazio = padrão do sistema."),
                    |ui| {
                        let mut v = r.output_device.clone().unwrap_or_default();
                        let mut opts: Vec<(&str, &str)> = vec![("", "padrão do sistema")];
                        opts.extend(d.outputs.iter().map(|s| (s.as_str(), s.as_str())));
                        if ui::select(ui, "outdev", &mut v, &opts) {
                            r.output_device = if v.is_empty() { None } else { Some(v) };
                        }
                    },
                );
            }
            ui::field(ui, "Testar dispositivos", Some("Use os dispositivos escolhidos acima (sem abrir sessão). Fale e veja a barra reagir; o tom confirma a saída."), |ui| {
                ui.horizontal(|ui| {
                    let testing = self.mic_test.is_some();
                    if ui.button(if testing { "Parar teste do microfone" } else { "Testar microfone" }).clicked() {
                        if testing {
                            self.mic_test = None;
                            self.mic_peak = 0.0;
                        } else {
                            self.mic_test = Some(audio::InputMonitor::start(r.input_device.as_deref()));
                        }
                    }
                    if ui.button("Tocar tom na saída").clicked() {
                        match audio::play_test_tone(r.output_device.as_deref()) {
                            Ok(name) => cx.toasts.success(format!("Tom enviado para: {name}")),
                            Err(e) => cx.toasts.error(e),
                        }
                    }
                    if ui.button("recarregar lista").clicked() {
                        self.devices = None;
                    }
                });
                if let Some(m) = &self.mic_test {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(60));
                    let l = m.level();
                    self.mic_level_smooth = self.mic_level_smooth * 0.6 + l * 0.4;
                    self.mic_peak = self.mic_peak.max(l);
                    let shown = (self.mic_level_smooth * 6.0).clamp(0.0, 1.0);
                    ui.add(egui::ProgressBar::new(shown).desired_width(ui.available_width()).fill(if shown > 0.08 { Tone::Success.color(true) } else { Tone::Neutral.color(true) }));
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("dispositivo: {}", if m.device_name.is_empty() { "?" } else { &m.device_name })).small().weak());
                        ui.label(egui::RichText::new(if self.mic_peak > 0.02 { "✓ captando som" } else { "nenhum som detectado ainda — fale ou escolha outro microfone" }).small().color(if self.mic_peak > 0.02 { Tone::Success.color(true) } else { Tone::Warning.color(true) }));
                    });
                    if let Some(e) = &m.error {
                        ui::error_text(ui, e);
                    }
                }
            });
            ui::field(ui, "Instruções da persona", None, |ui| {
                ui::text_area(ui, &mut r.instructions, 6);
            });
            ui::field(ui, "Salvar transcrições", None, |ui| {
                ui::toggle(ui, &mut r.save_transcripts);
            });
            ui::field(ui, "Limite de caracteres falados do cérebro", None, |ui| {
                ui::number(ui, &mut r.brain_speech_limit_chars, 0..=20000);
            });
            ui::field(
                ui,
                "Limite de minutos/dia",
                Some("Vazio = sem limite."),
                |ui| ui::optional_u32(ui, &mut r.daily_minutes_limit, "sem limite"),
            );
        });
        ui::section(
            ui,
            "Preços para estimativa (USD por 1 milhão de tokens)",
            |ui| {
                ui.label(egui::RichText::new("Padrões do gpt-realtime. A OpenAI não devolve o valor em dinheiro; o app calcula a partir dos tokens informados em cada resposta.").weak().small());
                let p = &mut st.draft.realtime.pricing;
                egui::Grid::new("pricing")
                    .num_columns(4)
                    .spacing([16.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Texto entrada");
                        ui.add(
                            egui::DragValue::new(&mut p.text_in)
                                .speed(0.1)
                                .range(0.0..=1000.0)
                                .prefix("$ "),
                        );
                        ui.label("Texto em cache");
                        ui.add(
                            egui::DragValue::new(&mut p.text_cached)
                                .speed(0.1)
                                .range(0.0..=1000.0)
                                .prefix("$ "),
                        );
                        ui.end_row();
                        ui.label("Texto saída");
                        ui.add(
                            egui::DragValue::new(&mut p.text_out)
                                .speed(0.1)
                                .range(0.0..=1000.0)
                                .prefix("$ "),
                        );
                        ui.label("Áudio entrada");
                        ui.add(
                            egui::DragValue::new(&mut p.audio_in)
                                .speed(0.1)
                                .range(0.0..=1000.0)
                                .prefix("$ "),
                        );
                        ui.end_row();
                        ui.label("Áudio em cache");
                        ui.add(
                            egui::DragValue::new(&mut p.audio_cached)
                                .speed(0.1)
                                .range(0.0..=1000.0)
                                .prefix("$ "),
                        );
                        ui.label("Áudio saída");
                        ui.add(
                            egui::DragValue::new(&mut p.audio_out)
                                .speed(0.1)
                                .range(0.0..=1000.0)
                                .prefix("$ "),
                        );
                        ui.end_row();
                    });
            },
        );
        st.save_bar(ui, cx.svc, "realtime");
    }
}
