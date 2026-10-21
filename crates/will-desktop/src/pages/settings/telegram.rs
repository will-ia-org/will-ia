//! Seção Telegram: status do bot (teste do token, threads ativas) e
//! configuração do módulo (allowlists, destino padrão, áudio, anexos).

use super::SettingsState;
use crate::help;
use crate::pages::Cx;
use crate::service::{j, Loader, Task};
use crate::ui::{self, SecretAction, SecretField, Tone};
use axum::extract::State;
use serde_json::Value;
use will_core::{Event, Module};
use will_server::api::integrations as api;

const VOICES: [&str; 10] = [
    "alloy", "ash", "ballad", "coral", "echo", "sage", "shimmer", "verse", "marin", "cedar",
];

#[derive(Default)]
pub struct TelegramPage {
    status: Loader<Value>,
    test_task: Option<Task<Value>>,
    last_test: Option<Value>,
    token: SecretField,
    chats_buf: Option<String>,
    users_buf: Option<String>,
}

impl TelegramPage {
    pub fn on_event(&mut self, ev: &Event) {
        match ev {
            Event::SettingsChanged { section } if section == "telegram" => {
                self.status.loaded_at = None
            }
            Event::ThreadCreated { .. } | Event::ThreadDeleted { .. } => {
                self.status.loaded_at = None
            }
            _ => {}
        }
    }

    fn reload_status(&mut self, cx: &Cx) {
        self.status.start(
            cx.svc
                .call(|s| async move { j(api::telegram_status(State(s)).await) }),
        );
    }

    fn poll(&mut self, cx: &mut Cx) {
        if self.status.stale(std::time::Duration::from_secs(30)) {
            self.reload_status(cx);
        }
        self.status.poll();
        if let Some(t) = &self.test_task {
            if let Some(r) = t.take() {
                self.test_task = None;
                match r {
                    Ok(v) => {
                        if ui::b(&v, "ok") {
                            cx.toasts
                                .success(format!("Bot OK: @{}", ui::s(&v, "username")));
                        } else {
                            cx.toasts
                                .error(format!("Teste do bot falhou: {}", ui::s(&v, "error")));
                        }
                        self.last_test = Some(v);
                    }
                    Err(e) => {
                        cx.toasts.error(format!("Teste do bot: {e}"));
                        self.last_test = None;
                    }
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        self.poll(cx);
        ui::heading_help(ui, "Telegram", Some("Supergrupo com tópicos: 1 tópico = 1 thread. Deny-by-default: só chats E usuários na allowlist conversam."), &help::TELEGRAM);

        // ---- bot ----
        ui::section(ui, "Bot", |ui| {
            let status = cx
                .snap
                .modules
                .get(&Module::Telegram)
                .cloned()
                .unwrap_or(will_core::ModuleStatus::Stopped);
            ui.horizontal(|ui| {
                ui.label("Módulo:");
                ui::badge(ui, &ui::module_label(&status), ui::tone_for_module(&status));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let testing = self.test_task.is_some();
                let has_token = st.secret_present("telegram_bot_token");
                if ui
                    .add_enabled(!testing && has_token, egui::Button::new("Testar bot"))
                    .on_disabled_hover_text("Salve o token do bot primeiro.")
                    .clicked()
                {
                    self.test_task = Some(
                        cx.svc
                            .call(|s| async move { j(api::telegram_test(State(s), None).await) }),
                    );
                }
                if testing {
                    ui.spinner();
                }
                if let Some(v) = &self.last_test {
                    if ui::b(v, "ok") {
                        let name = ui::s(v, "first_name");
                        ui.colored_label(
                            Tone::Success.color(ui.visuals().dark_mode),
                            format!(
                                "✓ @{} · id {}{}",
                                ui::s(v, "username"),
                                ui::i(v, "id"),
                                if name.is_empty() {
                                    String::new()
                                } else {
                                    format!(" · {name}")
                                }
                            ),
                        );
                    } else {
                        ui::error_text(ui, &ui::s(v, "error"));
                    }
                }
            });
            if !st.secret_present("telegram_bot_token") {
                ui.label(
                    egui::RichText::new(
                        "Sem token ainda? O ícone ? no título tem o passo a passo (BotFather, grupo, IDs).",
                    )
                    .weak()
                    .small(),
                );
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Threads do Telegram").strong());
                if ui.button("atualizar").clicked() {
                    self.status.loaded_at = None;
                }
                if self.status.loading() {
                    ui.spinner();
                }
            });
            if let Some(e) = &self.status.error {
                ui::error_text(ui, e);
            }
            let threads = self
                .status
                .data
                .as_ref()
                .and_then(|v| v.get("threads"))
                .and_then(|t| t.as_array())
                .cloned()
                .unwrap_or_default();
            if threads.is_empty() {
                ui.label(egui::RichText::new("Nenhuma thread ainda. Cada chat (ou tópico de supergrupo) vira uma thread ao receber a primeira mensagem.").weak().small());
            } else {
                egui::Grid::new("tg-threads")
                    .num_columns(4)
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("Thread key").weak().small());
                        ui.label(egui::RichText::new("Thread").weak().small());
                        ui.label(egui::RichText::new("Título").weak().small());
                        ui.label(egui::RichText::new("Estado").weak().small());
                        ui.end_row();
                        for t in &threads {
                            ui.monospace(ui::s(t, "thread_key"));
                            let tid = ui::s(t, "thread_id");
                            ui.monospace(if tid.is_empty() {
                                "—".to_string()
                            } else {
                                tid.chars().take(8).collect()
                            });
                            let title = ui::s(t, "title");
                            let topic = ui::s(t, "topic_name");
                            ui.label(if !title.is_empty() {
                                title
                            } else if !topic.is_empty() {
                                topic
                            } else {
                                "—".into()
                            });
                            ui.horizontal(|ui| {
                                if ui::b(t, "alive") {
                                    ui::badge(ui, "vivo", Tone::Success);
                                } else {
                                    ui::badge(ui, "dormente", Tone::Neutral);
                                }
                                let pending = ui::i(t, "pending");
                                if pending > 0 {
                                    ui::badge(ui, &format!("{pending} pendente(s)"), Tone::Info);
                                }
                                if ui::b(t, "session_expired") {
                                    ui::badge(ui, "sessão expirada", Tone::Warning);
                                }
                            });
                            ui.end_row();
                        }
                    });
            }
        });

        // ---- configuração ----
        ui::section(ui, "Configuração", |ui| {
            ui::field(
                ui,
                "Habilitar",
                Some("Long polling no bot; 1 tópico = 1 thread."),
                |ui| {
                    ui::toggle(ui, &mut st.draft.telegram.enabled);
                },
            );
            let present = st.secret_present("telegram_bot_token");
            let mut action = SecretAction::None;
            ui::field(
                ui,
                "Token do bot",
                Some("Do @BotFather. Vai para o cofre de segredos; nunca para o banco ou logs."),
                |ui| {
                    action = self.token.ui(ui, present, "123456789:AA…");
                },
            );
            match action {
                SecretAction::Save(v) => st.set_secret(cx.svc, "telegram_bot_token", v),
                SecretAction::Delete => st.delete_secret(cx.svc, "telegram_bot_token"),
                SecretAction::None => {}
            }

            let t = &mut st.draft.telegram;
            ui::field(
                ui,
                "Chats permitidos",
                Some("IDs (negativos para grupos), separados por vírgula ou linha."),
                |ui| {
                    let mut buf = self.chats_buf.take().unwrap_or_else(|| {
                        t.allowed_chats
                            .iter()
                            .map(|i| i.to_string())
                            .collect::<Vec<_>>()
                            .join("\n")
                    });
                    if ui::lines_editor(ui, &mut buf, 3).changed() {
                        t.allowed_chats = ui::parse_ids(&buf);
                    }
                    self.chats_buf = Some(buf);
                },
            );
            ui::field(
                ui,
                "Usuários permitidos",
                Some("IDs de usuário, separados por vírgula ou linha."),
                |ui| {
                    let mut buf = self.users_buf.take().unwrap_or_else(|| {
                        t.allowed_users
                            .iter()
                            .map(|i| i.to_string())
                            .collect::<Vec<_>>()
                            .join("\n")
                    });
                    if ui::lines_editor(ui, &mut buf, 3).changed() {
                        t.allowed_users = ui::parse_ids(&buf);
                    }
                    self.users_buf = Some(buf);
                },
            );
            ui::field(ui, "Destino padrão", Some("Chat (e tópico, opcional) usado por telegram_send quando o agente não indica destino e pelo anúncio de boot."), |ui| {
                ui.horizontal(|ui| {
                    ui.label("chat");
                    ui::optional_i64(ui, &mut t.default_chat_id, "ex.: -1001234567890");
                    ui.label("tópico");
                    ui::optional_i64(ui, &mut t.default_topic_id, "opcional");
                });
            });
            ui::field(
                ui,
                "Anunciar no boot",
                Some("Manda uma mensagem ao destino padrão quando o serviço sobe."),
                |ui| {
                    ui::toggle(ui, &mut t.announce_boot);
                },
            );
            ui::field(
                ui,
                "Não autorizados",
                Some("O que fazer com mensagens fora da allowlist (além de /start)."),
                |ui| {
                    ui::select(
                        ui,
                        "tg-unauth",
                        &mut t.unauthorized_behavior,
                        &[("ignore", "Ignorar"), ("reply", "Responder aviso")],
                    );
                },
            );
            ui::field(
                ui,
                "Formato da resposta",
                Some("HTML converte Markdown do Claude para a formatação do Telegram."),
                |ui| {
                    ui::select(
                        ui,
                        "tg-fmt",
                        &mut t.reply_format,
                        &[("text", "Texto puro"), ("html", "HTML")],
                    );
                },
            );
            ui::field(ui, "Resumo em áudio", Some("Respostas longas ganham uma versão falada (TTS da OpenAI; requer a API key da seção Voz)."), |ui| {
                ui::toggle(ui, &mut t.audio_summary);
            });
            ui.add_enabled_ui(t.audio_summary, |ui| {
                ui::field(
                    ui,
                    "Mínimo de caracteres",
                    Some("Só respostas com pelo menos este tamanho ganham áudio."),
                    |ui| {
                        ui::number(ui, &mut t.audio_min_chars, 0..=10_000);
                    },
                );
                ui::field(ui, "Voz do áudio", None, |ui| {
                    let opts: Vec<(&str, &str)> = VOICES.iter().map(|v| (*v, *v)).collect();
                    ui::select(ui, "tg-voice", &mut t.audio_voice, &opts);
                });
                ui::field(ui, "Velocidade", Some("0,5 a 2,0."), |ui| {
                    ui.add(
                        egui::DragValue::new(&mut t.audio_speed)
                            .range(0.5..=2.0)
                            .speed(0.05)
                            .fixed_decimals(2),
                    );
                });
            });
            ui::field(ui, "Transcrever voice notes", Some("Áudios recebidos viram texto antes de ir ao cérebro (requer a API key da OpenAI)."), |ui| {
                ui::toggle(ui, &mut t.transcribe_voice_notes);
            });
            ui::field(
                ui,
                "Pasta de anexos",
                Some("Relativa à base de conhecimento; arquivos recebidos são salvos aqui."),
                |ui| {
                    ui::text_input(ui, &mut t.attachments_dir, "inbox/telegram");
                },
            );
            ui::field(ui, "Atraso para apagar status (s)", Some("Tempo que a mensagem de status (\"pensando…\") fica visível depois da resposta."), |ui| {
                ui::number(ui, &mut t.status_delete_delay_secs, 0..=600);
            });
        });
        st.save_bar(ui, cx.svc, "telegram");
    }
}
