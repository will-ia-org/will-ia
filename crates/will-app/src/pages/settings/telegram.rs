//! Seção Telegram: status do bot (teste do token, threads ativas) e
//! configuração do módulo (allowlists, destino padrão, áudio, anexos).

use super::SettingsState;
use crate::help;
use crate::i18n::{tr, trf};
use crate::pages::Cx;
use crate::service::{Loader, Task};
use crate::ui::{self, SecretAction, SecretField, Tone};
use serde_json::Value;
use will_core::{Event, Module};

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
        self.status
            .start(cx.svc.call(|c| async move { c.telegram_status().await }));
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
                                .success(trf("Bot OK: @{}", &[&ui::s(&v, "username")]));
                        } else {
                            cx.toasts
                                .error(trf("Teste do bot falhou: {}", &[&ui::s(&v, "error")]));
                        }
                        self.last_test = Some(v);
                    }
                    Err(e) => {
                        cx.toasts.error(trf("Teste do bot: {}", &[&e]));
                        self.last_test = None;
                    }
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        self.poll(cx);
        ui::heading_help(ui, tr("Telegram"), Some(tr("Supergrupo com tópicos: 1 tópico = 1 thread. Deny-by-default: só chats E usuários na allowlist conversam.")), &help::TELEGRAM);

        // ---- bot ----
        ui::section(ui, tr("Bot"), |ui| {
            let status = cx
                .snap
                .modules
                .get(&Module::Telegram)
                .cloned()
                .unwrap_or(will_core::ModuleStatus::Stopped);
            ui.horizontal(|ui| {
                ui.label(tr("Módulo:"));
                ui::badge(ui, &ui::module_label(&status), ui::tone_for_module(&status));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let testing = self.test_task.is_some();
                let has_token = st.secret_present("telegram_bot_token");
                if ui
                    .add_enabled(!testing && has_token, egui::Button::new(tr("Testar bot")))
                    .on_disabled_hover_text(tr("Salve o token do bot primeiro."))
                    .clicked()
                {
                    self.test_task = Some(cx.svc.call(|c| async move { c.telegram_test().await }));
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
                    egui::RichText::new(tr(
                        "Sem token ainda? O ícone ? no título tem o passo a passo (BotFather, grupo, IDs).",
                    ))
                    .weak()
                    .small(),
                );
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tr("Threads do Telegram")).strong());
                if ui.button(tr("atualizar")).clicked() {
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
                ui.label(egui::RichText::new(tr("Nenhuma thread ainda. Cada chat (ou tópico de supergrupo) vira uma thread ao receber a primeira mensagem.")).weak().small());
            } else {
                egui::Grid::new("tg-threads")
                    .num_columns(4)
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(tr("Thread key")).weak().small());
                        ui.label(egui::RichText::new(tr("Thread")).weak().small());
                        ui.label(egui::RichText::new(tr("Título")).weak().small());
                        ui.label(egui::RichText::new(tr("Estado")).weak().small());
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
                                    ui::badge(ui, tr("vivo"), Tone::Success);
                                } else {
                                    ui::badge(ui, tr("dormente"), Tone::Neutral);
                                }
                                let pending = ui::i(t, "pending");
                                if pending > 0 {
                                    ui::badge(ui, &trf("{} pendente(s)", &[&pending]), Tone::Info);
                                }
                                if ui::b(t, "session_expired") {
                                    ui::badge(ui, tr("sessão expirada"), Tone::Warning);
                                }
                            });
                            ui.end_row();
                        }
                    });
            }
        });

        // ---- configuração ----
        ui::section(ui, tr("Configuração"), |ui| {
            ui::field(
                ui,
                tr("Habilitar"),
                Some(tr("Long polling no bot; 1 tópico = 1 thread.")),
                |ui| {
                    ui::toggle(ui, &mut st.draft.telegram.enabled);
                },
            );
            let present = st.secret_present("telegram_bot_token");
            let mut action = SecretAction::None;
            ui::field(
                ui,
                tr("Token do bot"),
                Some(tr(
                    "Do @BotFather. Vai para o cofre de segredos; nunca para o banco ou logs.",
                )),
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
                tr("Chats permitidos"),
                Some(tr(
                    "IDs (negativos para grupos), separados por vírgula ou linha.",
                )),
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
                tr("Usuários permitidos"),
                Some(tr("IDs de usuário, separados por vírgula ou linha.")),
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
            ui::field(ui, tr("Destino padrão"), Some(tr("Chat (e tópico, opcional) usado por telegram_send quando o agente não indica destino e pelo anúncio de boot.")), |ui| {
                ui.horizontal(|ui| {
                    ui.label(tr("chat"));
                    ui::optional_i64(ui, &mut t.default_chat_id, tr("ex.: -1001234567890"));
                    ui.label(tr("tópico"));
                    ui::optional_i64(ui, &mut t.default_topic_id, tr("opcional"));
                });
            });
            ui::field(
                ui,
                tr("Anunciar no boot"),
                Some(tr(
                    "Manda uma mensagem ao destino padrão quando o serviço sobe.",
                )),
                |ui| {
                    ui::toggle(ui, &mut t.announce_boot);
                },
            );
            ui::field(
                ui,
                tr("Não autorizados"),
                Some(tr(
                    "O que fazer com mensagens fora da allowlist (além de /start).",
                )),
                |ui| {
                    ui::select(
                        ui,
                        "tg-unauth",
                        &mut t.unauthorized_behavior,
                        &[("ignore", tr("Ignorar")), ("reply", tr("Responder aviso"))],
                    );
                },
            );
            ui::field(
                ui,
                tr("Formato da resposta"),
                Some(tr(
                    "HTML converte Markdown do Claude para a formatação do Telegram.",
                )),
                |ui| {
                    ui::select(
                        ui,
                        "tg-fmt",
                        &mut t.reply_format,
                        &[("text", tr("Texto puro")), ("html", tr("HTML"))],
                    );
                },
            );
            ui::field(ui, tr("Resumo em áudio"), Some(tr("Respostas longas ganham uma versão falada (TTS da OpenAI; requer a API key da seção Voz).")), |ui| {
                ui::toggle(ui, &mut t.audio_summary);
            });
            ui.add_enabled_ui(t.audio_summary, |ui| {
                ui::field(
                    ui,
                    tr("Mínimo de caracteres"),
                    Some(tr("Só respostas com pelo menos este tamanho ganham áudio.")),
                    |ui| {
                        ui::number(ui, &mut t.audio_min_chars, 0..=10_000);
                    },
                );
                ui::field(ui, tr("Voz do áudio"), None, |ui| {
                    let opts: Vec<(&str, &str)> = VOICES.iter().map(|v| (*v, *v)).collect();
                    ui::select(ui, "tg-voice", &mut t.audio_voice, &opts);
                });
                ui::field(ui, tr("Velocidade"), Some(tr("0,5 a 2,0.")), |ui| {
                    ui.add(
                        egui::DragValue::new(&mut t.audio_speed)
                            .range(0.5..=2.0)
                            .speed(0.05)
                            .fixed_decimals(2),
                    );
                });
            });
            ui::field(ui, tr("Transcrever voice notes"), Some(tr("Áudios recebidos viram texto antes de ir ao cérebro (requer a API key da OpenAI).")), |ui| {
                ui::toggle(ui, &mut t.transcribe_voice_notes);
            });
            ui::field(
                ui,
                tr("Pasta de anexos"),
                Some(tr(
                    "Relativa à base de conhecimento; arquivos recebidos são salvos aqui.",
                )),
                |ui| {
                    ui::text_input(ui, &mut t.attachments_dir, "inbox/telegram");
                },
            );
            ui::field(ui, tr("Atraso para apagar status (s)"), Some(tr("Tempo que a mensagem de status (\"pensando…\") fica visível depois da resposta.")), |ui| {
                ui::number(ui, &mut t.status_delete_delay_secs, 0..=600);
            });
        });
        st.save_bar(ui, cx.svc, "telegram");
    }
}
