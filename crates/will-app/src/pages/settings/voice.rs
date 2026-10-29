//! Seção Voz: painel da sessão (iniciar/parar, segurar para falar, nível,
//! transcrições) e configuração da OpenAI Realtime.

use super::SettingsState;
use crate::help;
use crate::i18n::{tr, trf};
use crate::pages::Cx;
use crate::service::Loader;
use crate::ui::{self, SecretAction, SecretField, Tone};
use crate::voice::audio;
use serde_json::Value;
use will_core::Event;

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
            self.transcripts.start(cx.svc.call(|c| async move {
                c.voice_transcripts(&[("limit", "50".to_string())]).await
            }));
        }
        self.transcripts.poll();
        if self.status.stale(std::time::Duration::from_secs(60)) {
            self.status
                .start(cx.svc.call(|c| async move { c.voice_status().await }));
        }
        self.status.poll();
        if self.devices.is_none() {
            self.devices = Some(audio::list_devices());
        }

        ui::heading(ui, tr("Voz (OpenAI Realtime)"), Some(tr("Conversa por voz pelo microfone e alto-falante deste computador; tarefas longas vão para o cérebro via ask_brain.")));

        let tab = ui::tabs(
            ui,
            "voice",
            &[
                ui::Tab::new("session", tr("Sessão")),
                ui::Tab::new("config", tr("Configuração")),
                ui::Tab::new("tts", tr("Ouvir resumo (TTS)")),
                ui::Tab::new("pricing", tr("Preços")),
            ],
        );

        // ---- painel da sessão ----
        if tab == "session" {
            ui::section_help(ui, tr("Sessão de voz"), Some(&help::VOICE_SESSION), |ui| {
                let v = &mut *cx.voice;
                ui.horizontal(|ui| {
                    let enabled = st.saved.realtime.enabled;
                    if v.session_active {
                        if ui
                            .add_enabled(
                                !v.busy(),
                                egui::Button::new(
                                    egui::RichText::new(tr("Parar")).color(egui::Color32::WHITE),
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
                                egui::RichText::new(tr("Iniciar")).color(egui::Color32::WHITE),
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
                        if v.session_active {
                            label
                        } else {
                            tr("inativa")
                        },
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
                            egui::RichText::new(tr("habilite e salve a configuração abaixo"))
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
                        ui.colored_label(Tone::Warning.color(true), trf("falhas de áudio nesta sessão: {} esvaziamento(s) da fila, {} delta(s) perdido(s)", &[&u, &l]));
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
                            ui.label(egui::RichText::new(tr("Última sessão")).weak().small());
                            ui::wrapped_text(ui, &ui::s(last, "summary"));
                            if ui.button(tr("Ver custos")).clicked() {
                                *cx.go = Some(crate::pages::Nav::Costs);
                            }
                        });
                }
                // Sessões de voz no servidor (uma por usuário conectado).
                if let Some(stv) = self.status.data.as_ref() {
                    let max = stv
                        .get("max_sessions")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0);
                    let active = stv
                        .get("active_sessions")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0);
                    let sessions: Vec<&Value> = stv
                        .get("sessions")
                        .and_then(|x| x.as_array())
                        .map(|a| a.iter().filter(|s| ui::b(s, "active")).collect())
                        .unwrap_or_default();
                    if max > 0 || !sessions.is_empty() {
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new(tr("Sessões no servidor"))
                                    .weak()
                                    .small(),
                            );
                            ui::badge(
                                ui,
                                &format!("{active}/{max}"),
                                if max > 0 && active >= max {
                                    Tone::Warning
                                } else {
                                    Tone::Neutral
                                },
                            );
                            for s in &sessions {
                                let user = ui::s(s, "user");
                                let (label, _) =
                                    crate::voice::overlay::label_for(&ui::s(s, "state"));
                                ui::badge(ui, &format!("{user} · {label}"), Tone::Info);
                            }
                            if sessions.is_empty() {
                                ui.label(egui::RichText::new(tr("nenhuma ativa")).weak().small());
                            }
                        });
                    }
                }
                if v.assistant_playing() && !v.barge_in {
                    ui.label(
                        egui::RichText::new(tr(
                            "assistente falando: microfone silenciado até terminar",
                        ))
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
                            tr("Falando…")
                        } else {
                            tr("Segurar para falar")
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
                        .add_enabled(v.session_active, egui::Button::new(tr("Enviar agora")))
                        .clicked()
                    {
                        v.commit(cx.svc);
                    }
                });
                ui.add_space(6.0);
                ui.label(egui::RichText::new(tr("Transcrições")).strong());
                egui::ScrollArea::vertical()
                    .id_salt("voice-transcripts")
                    .max_height(220.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        if let Some(list) =
                            self.transcripts.data.as_ref().and_then(|v| v.as_array())
                        {
                            for t in list.iter().rev() {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(
                                        egui::RichText::new(ui::fmt_dt(&t["at"])).small().weak(),
                                    );
                                    let role = ui::s(t, "role");
                                    ui.label(
                                        egui::RichText::new(if role == "user" {
                                            tr("você")
                                        } else {
                                            tr("assistente")
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
                                ui.label(egui::RichText::new(tr("agora")).small().weak());
                                ui.label(
                                    egui::RichText::new(if role == "user" {
                                        tr("você")
                                    } else {
                                        tr("assistente")
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
                            ui.label(egui::RichText::new(tr("Nenhuma transcrição ainda.")).weak());
                        }
                    });
            });
        }
        // ---- configuração ----
        if tab == "config" {
            ui::section_help(ui, tr("Configuração"), Some(&help::VOICE_CONFIG), |ui| {
                let r = &mut st.draft.realtime;
                ui::field(ui, tr("Habilitar"), None, |ui| {
                    ui::toggle(ui, &mut r.enabled);
                });
                let present = st
                    .secrets_present
                    .get("openai_api_key")
                    .copied()
                    .unwrap_or(false);
                let mut action = SecretAction::None;
                ui::field(ui, tr("OpenAI API key"), Some(tr("Vai para o cofre de segredos; usada pela voz e pela transcrição de áudios do Telegram.")), |ui| {
                action = self.api_key.ui(ui, present, "sk-…");
            });
                match action {
                    SecretAction::Save(v) => st.set_secret(cx.svc, "openai_api_key", v),
                    SecretAction::Delete => st.delete_secret(cx.svc, "openai_api_key"),
                    SecretAction::None => {}
                }
                let r = &mut st.draft.realtime;
                ui::field(ui, tr("Modelo Realtime"), None, |ui| {
                    ui::text_input(ui, &mut r.model, "gpt-realtime");
                });
                ui::field(ui, tr("Modelo de transcrição"), None, |ui| {
                    ui::text_input(ui, &mut r.transcription_model, "gpt-4o-mini-transcribe");
                });
                ui::field(ui, tr("Voz"), None, |ui| {
                    let voices = [
                        "alloy", "ash", "ballad", "coral", "echo", "sage", "shimmer", "verse",
                        "marin", "cedar",
                    ];
                    let opts: Vec<(&str, &str)> = voices.iter().map(|v| (*v, *v)).collect();
                    ui::select(ui, "voice", &mut r.voice, &opts);
                });
                ui::field(ui, tr("Modo de ativação"), None, |ui| {
                    ui::select(
                        ui,
                        "activation",
                        &mut r.activation,
                        &[
                            ("toggle", tr("Alternar (liga/desliga)")),
                            ("push_to_talk", tr("Push-to-talk (segurar)")),
                        ],
                    );
                });
                ui::field(
                    ui,
                    tr("Atalho global"),
                    Some(tr(
                        "Ex.: Ctrl+Shift+Space, CommandOrControl+Alt+V. Não funciona em Wayland.",
                    )),
                    |ui| {
                        ui::text_input(ui, &mut r.hotkey, "Ctrl+Shift+Space");
                    },
                );
                ui::field(
                    ui,
                    tr("Sinais sonoros"),
                    Some(tr(
                        "Toca um bipe curto ao ligar e desligar a voz e em erro.",
                    )),
                    |ui| {
                        ui::toggle(ui, &mut r.sound_cues);
                    },
                );
                ui::field(
                ui,
                tr("Interromper por voz (barge-in)"),
                Some(tr("Desligado: o microfone fica mudo enquanto o assistente fala, evitando que o alto-falante corte a resposta. Ligue só com fone de ouvido.")),
                |ui| {
                    ui::toggle(ui, &mut r.barge_in);
                },
            );
                ui::field(
                ui,
                tr("Sensibilidade do detector de fala"),
                Some(tr("Maior = exige voz mais alta para considerar que você falou (0.3 sensível · 0.9 exigente).")),
                |ui| {
                    ui.add(egui::Slider::new(&mut r.vad_threshold, 0.1..=0.95).step_by(0.05));
                },
            );
                ui::field(ui, tr("Detecção de turno"), None, |ui| {
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
                    ui::field(
                        ui,
                        tr("Microfone"),
                        Some(tr("Vazio = padrão do sistema.")),
                        |ui| {
                            let mut v = r.input_device.clone().unwrap_or_default();
                            let mut opts: Vec<(&str, &str)> = vec![("", tr("padrão do sistema"))];
                            opts.extend(d.inputs.iter().map(|s| (s.as_str(), s.as_str())));
                            if ui::select(ui, "indev", &mut v, &opts) {
                                r.input_device = if v.is_empty() { None } else { Some(v) };
                            }
                        },
                    );
                    ui::field(
                        ui,
                        tr("Saída de áudio"),
                        Some(tr("Vazio = padrão do sistema.")),
                        |ui| {
                            let mut v = r.output_device.clone().unwrap_or_default();
                            let mut opts: Vec<(&str, &str)> = vec![("", tr("padrão do sistema"))];
                            opts.extend(d.outputs.iter().map(|s| (s.as_str(), s.as_str())));
                            if ui::select(ui, "outdev", &mut v, &opts) {
                                r.output_device = if v.is_empty() { None } else { Some(v) };
                            }
                        },
                    );
                }
                ui::field(ui, tr("Testar dispositivos"), Some(tr("Use os dispositivos escolhidos acima (sem abrir sessão). Fale e veja a barra reagir; o tom confirma a saída.")), |ui| {
                ui.horizontal(|ui| {
                    let testing = self.mic_test.is_some();
                    if ui.button(if testing { tr("Parar teste do microfone") } else { tr("Testar microfone") }).clicked() {
                        if testing {
                            self.mic_test = None;
                            self.mic_peak = 0.0;
                        } else {
                            self.mic_test = Some(audio::InputMonitor::start(r.input_device.as_deref()));
                        }
                    }
                    if ui.button(tr("Tocar tom na saída")).clicked() {
                        match audio::play_test_tone(r.output_device.as_deref()) {
                            Ok(name) => cx.toasts.success(trf("Tom enviado para: {}", &[&name])),
                            Err(e) => cx.toasts.error(e),
                        }
                    }
                    if ui.button(tr("recarregar lista")).clicked() {
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
                        let dev: &str = if m.device_name.is_empty() { "?" } else { &m.device_name };
                        ui.label(egui::RichText::new(trf("dispositivo: {}", &[&dev])).small().weak());
                        ui.label(egui::RichText::new(if self.mic_peak > 0.02 { tr("✓ captando som") } else { tr("nenhum som detectado ainda — fale ou escolha outro microfone") }).small().color(if self.mic_peak > 0.02 { Tone::Success.color(true) } else { Tone::Warning.color(true) }));
                    });
                    if let Some(e) = &m.error {
                        ui::error_text(ui, e);
                    }
                }
            });
                ui::field(ui, tr("Instruções da persona"), None, |ui| {
                    ui::text_area(ui, &mut r.instructions, 6);
                });
                ui::field(ui, tr("Salvar transcrições"), None, |ui| {
                    ui::toggle(ui, &mut r.save_transcripts);
                });
                ui::field(
                    ui,
                    tr("Limite de caracteres falados do cérebro"),
                    None,
                    |ui| {
                        ui::number(ui, &mut r.brain_speech_limit_chars, 0..=20000);
                    },
                );
                ui::field(
                    ui,
                    tr("Limite de minutos/dia"),
                    Some(tr("Vazio = sem limite.")),
                    |ui| ui::optional_u32(ui, &mut r.daily_minutes_limit, tr("sem limite")),
                );
                ui::field(
                    ui,
                    tr("Sessões simultâneas"),
                    Some(tr(
                        "Uma por usuário conectado; cada sessão tem custo próprio na OpenAI.",
                    )),
                    |ui| {
                        ui::number(ui, &mut r.max_sessions, 1..=50);
                    },
                );
            });
        }
        // ---- ouvir resumo (TTS) ----
        if tab == "tts" {
            ui::section_help(
                ui,
                tr("Ouvir resumo (TTS)"),
                Some(&help::VOICE_SPEAK),
                |ui| {
                    let sm = &mut st.draft.realtime.summary;
                    ui.label(
                egui::RichText::new(tr(
                    "Botão \"ouvir\" nas respostas da thread: a resposta vira um roteiro fluido (sem código, links ou marcação) e é lida em voz alta. Usa a chave da OpenAI acima; só aparece com o módulo de voz ligado.",
                ))
                .small()
                .weak(),
            );
                    ui.add_space(4.0);
                    ui::field(ui, tr("Habilitar"), None, |ui| {
                        ui::toggle(ui, &mut sm.enabled);
                    });
                    ui::field(
                        ui,
                        tr("Modelo do resumo"),
                        Some(tr("Modelo de chat que escreve o roteiro falado.")),
                        |ui| {
                            ui::text_input(ui, &mut sm.model, "gpt-4o-mini");
                        },
                    );
                    ui::field(
                ui,
                tr("Prompt do resumo"),
                Some(tr(
                    "Instruções de sistema: o que resumir, o que não ler (código, URLs, IDs) e o tom. A resposta completa vai como mensagem do usuário.",
                )),
                |ui| {
                    ui::text_area(ui, &mut sm.prompt, 10);
                    let is_default = sm.prompt.trim()
                        == will_core::settings::default_speak_summary_prompt().trim();
                    if ui
                        .add_enabled(!is_default, egui::Button::new(tr("Restaurar padrão")))
                        .clicked()
                    {
                        sm.prompt = will_core::settings::default_speak_summary_prompt().into();
                    }
                },
            );
                    ui::field(
                        ui,
                        tr("Modelo de voz (TTS)"),
                        Some(tr("Modelo de síntese de voz da OpenAI.")),
                        |ui| {
                            ui::text_input(ui, &mut sm.tts_model, "gpt-4o-mini-tts");
                        },
                    );
                    ui::field(
                        ui,
                        tr("Voz da leitura"),
                        Some(tr("Vazio = a mesma voz da sessão de voz.")),
                        |ui| {
                            let same = tr("mesma da sessão de voz");
                            let mut opts: Vec<(&str, &str)> = vec![("", same)];
                            opts.extend(
                                [
                                    "alloy", "ash", "ballad", "coral", "echo", "sage", "shimmer",
                                    "verse", "marin", "cedar",
                                ]
                                .iter()
                                .map(|v| (*v, *v)),
                            );
                            ui::select(ui, "summary-voice", &mut sm.voice, &opts);
                        },
                    );
                    ui::field(
                        ui,
                        tr("Velocidade"),
                        Some(tr("1,0 = normal; 0,5 a 2,0.")),
                        |ui| {
                            ui.add(
                                egui::DragValue::new(&mut sm.speed)
                                    .speed(0.05)
                                    .range(0.5..=2.0)
                                    .fixed_decimals(2)
                                    .suffix("×"),
                            );
                        },
                    );
                },
            );
        }
        if tab == "pricing" {
            ui::section_help(
                ui,
                tr("Preços para estimativa (USD por 1 milhão de tokens)"),
                Some(&help::VOICE_PRICING),
                |ui| {
                    let p = &mut st.draft.realtime.pricing;
                    egui::Grid::new("pricing")
                        .num_columns(4)
                        .spacing([16.0, 8.0])
                        .show(ui, |ui| {
                            ui.label(tr("Texto entrada"));
                            ui.add(
                                egui::DragValue::new(&mut p.text_in)
                                    .speed(0.1)
                                    .range(0.0..=1000.0)
                                    .prefix("$ "),
                            );
                            ui.label(tr("Texto em cache"));
                            ui.add(
                                egui::DragValue::new(&mut p.text_cached)
                                    .speed(0.1)
                                    .range(0.0..=1000.0)
                                    .prefix("$ "),
                            );
                            ui.end_row();
                            ui.label(tr("Texto saída"));
                            ui.add(
                                egui::DragValue::new(&mut p.text_out)
                                    .speed(0.1)
                                    .range(0.0..=1000.0)
                                    .prefix("$ "),
                            );
                            ui.label(tr("Áudio entrada"));
                            ui.add(
                                egui::DragValue::new(&mut p.audio_in)
                                    .speed(0.1)
                                    .range(0.0..=1000.0)
                                    .prefix("$ "),
                            );
                            ui.end_row();
                            ui.label(tr("Áudio em cache"));
                            ui.add(
                                egui::DragValue::new(&mut p.audio_cached)
                                    .speed(0.1)
                                    .range(0.0..=1000.0)
                                    .prefix("$ "),
                            );
                            ui.label(tr("Áudio saída"));
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
        }
        if tab != "session" {
            st.save_bar(ui, cx.svc, "realtime");
        }
    }
}
