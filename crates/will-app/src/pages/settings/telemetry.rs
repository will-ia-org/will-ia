//! Aba Telemetria (Configurações → Geral): envio opcional dos logs e spans
//! do servidor (e do app) por **OTLP/HTTP** — Axiom como preset, ou qualquer
//! endpoint OpenTelemetry. Cards: Destino (ligar, provedor, dataset/cabeçalhos,
//! token, avançado), O que enviar (nível, rótulo, traces, app, relatórios de
//! falha, privacidade) e Status (`GET /api/telemetry/status`, evento de teste).

use super::SettingsState;
use crate::help;
use crate::i18n::{tr, trf};
use crate::icons;
use crate::pages::Cx;
use crate::service::{Loader, Task};
use crate::ui::{self, SecretAction, SecretField, Tone};
use serde_json::Value;
use will_core::Event;

/// Chave do segredo no cofre (`POST /api/secrets`).
const TOKEN_KEY: &str = "telemetry_token";
const AXIOM_URL: &str = "https://app.axiom.co";

#[derive(Default)]
pub struct TelemetryPage {
    status: Loader<Value>,
    test_task: Option<Task<Value>>,
    /// Resultado do último teste: `Ok(json {ok, message})` ou erro de transporte.
    last_test: Option<Result<Value, String>>,
    token: SecretField,
}

impl TelemetryPage {
    pub fn on_event(&mut self, ev: &Event) {
        if let Event::SettingsChanged { section } = ev {
            if section == "telemetry" {
                self.status.loaded_at = None;
            }
        }
    }

    fn poll(&mut self, cx: &mut Cx) {
        if self.status.stale(std::time::Duration::from_secs(30)) {
            self.status
                .start(cx.svc.call(|c| async move { c.telemetry_status().await }));
        }
        self.status.poll();
        if let Some(t) = &self.test_task {
            if let Some(r) = t.take() {
                self.test_task = None;
                match &r {
                    Ok(v) if ui::b(v, "ok") => cx.toasts.success(tr("Evento de teste enviado.")),
                    Ok(v) => cx.toasts.error(trf(
                        "Teste da telemetria falhou: {}",
                        &[&ui::s(v, "message")],
                    )),
                    Err(e) => cx.toasts.error(trf("Teste da telemetria: {}", &[e])),
                }
                self.last_test = Some(r);
                // o teste mexe nos contadores
                self.status.loaded_at = None;
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        self.poll(cx);
        self.axiom_card(ui, cx, st);
        self.what_card(ui, st);
        st.save_bar(ui, cx.svc, "telemetry");
        self.status_card(ui, cx, st);
    }

    // ---- card 1: Axiom ----
    fn axiom_card(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        let present = st.secret_present(TOKEN_KEY);
        ui::section_help(ui, tr("Destino (OTLP)"), Some(&help::TELEMETRY), |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                if st.saved.telemetry.enabled {
                    ui::badge(ui, tr("ativada"), Tone::Success);
                } else {
                    ui::badge(ui, tr("desativada"), Tone::Neutral);
                }
                if present {
                    ui::badge(ui, tr("token salvo ✓"), Tone::Success);
                } else {
                    ui::badge(ui, tr("sem token"), Tone::Warning);
                }
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(tr(
                            "Desligada por padrão. Liga só se você quiser ver os logs desta instância de fora.",
                        ))
                        .weak()
                        .small(),
                    )
                    .wrap(),
                );
            });
            ui.add_space(6.0);

            let t = &mut st.draft.telemetry;
            ui::field(
                ui,
                tr("Ativada"),
                Some(tr(
                    "Envia logs e spans do servidor em lotes (OTLP/HTTP), em segundo plano.",
                )),
                |ui| {
                    ui::toggle(ui, &mut t.enabled);
                },
            );
            ui::field(
                ui,
                tr("Provedor"),
                Some(tr(
                    "O protocolo é sempre OTLP/HTTP (OpenTelemetry). Axiom = preset pronto; OTLP genérico = qualquer backend (Collector, Grafana Cloud, New Relic, OpenObserve…).",
                )),
                |ui| {
                    if ui::select(
                        ui,
                        "telemetry-provider",
                        &mut t.provider,
                        &[("axiom", "Axiom"), ("otlp", "OTLP genérico")],
                    ) && t.provider == "axiom"
                        && t.endpoint.trim().is_empty()
                    {
                        t.endpoint = "https://api.axiom.co".into();
                    }
                },
            );
            let axiom = t.provider != "otlp";
            if axiom {
                ui::field(
                    ui,
                    tr("Dataset"),
                    Some(tr("Nome do dataset no Axiom que vai receber os eventos.")),
                    |ui| {
                        ui::text_input(ui, &mut t.dataset, "will-ia");
                    },
                );
            } else {
                ui::field(
                    ui,
                    tr("Cabeçalhos"),
                    Some(tr(
                        "Um por linha, no formato Nome: valor (ex.: X-Scope-OrgID: 123). O token abaixo, se houver, vai como Authorization: Bearer.",
                    )),
                    |ui| {
                        let mut text = t.headers.join("\n");
                        if ui::text_area(ui, &mut text, 3).changed() {
                            t.headers = text.split('\n').map(String::from).collect();
                        }
                    },
                );
            }
            let mut action = SecretAction::None;
            ui::field(
                ui,
                tr("Token"),
                Some(if axiom {
                    tr("API token com permissão de ingest no dataset. Vai para o cofre de segredos; nunca para o banco, logs ou eventos.")
                } else {
                    tr("Opcional: enviado como Authorization: Bearer. Vai para o cofre de segredos; nunca para o banco, logs ou eventos.")
                }),
                |ui| {
                    action = self
                        .token
                        .ui(ui, present, if axiom { "xaat-…" } else { "" });
                },
            );
            match action {
                SecretAction::Save(v) => st.set_secret(cx.svc, TOKEN_KEY, v),
                SecretAction::Delete => st.delete_secret(cx.svc, TOKEN_KEY),
                SecretAction::None => {}
            }
            if axiom {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.hyperlink_to(tr("Criar conta/token no Axiom"), AXIOM_URL)
                        .on_hover_text(AXIOM_URL);
                    ui.label(
                        egui::RichText::new(tr(
                            "· plano grátis: 500 GB/mês, 30 dias de retenção. Crie o dataset e um token de ingest; o passo a passo está no ícone ?.",
                        ))
                        .weak()
                        .small(),
                    );
                });
            }

            ui.add_space(6.0);
            let t = &mut st.draft.telemetry;
            if axiom {
                ui.collapsing(tr("Avançado"), |ui| {
                    ui::field(
                        ui,
                        tr("Endpoint"),
                        Some(tr(
                            "Base da API do Axiom. Só mude para uma região/instalação própria (ex.: https://api.eu.axiom.co).",
                        )),
                        |ui| {
                            ui::text_input(ui, &mut t.endpoint, "https://api.axiom.co");
                        },
                    );
                });
            } else {
                ui::field(
                    ui,
                    tr("Endpoint"),
                    Some(tr(
                        "Base OTLP/HTTP; o serviço acrescenta /v1/logs e /v1/traces (ex.: http://localhost:4318).",
                    )),
                    |ui| {
                        ui::text_input(ui, &mut t.endpoint, "http://localhost:4318");
                    },
                );
            }
        });
    }

    // ---- card 2: o que enviar ----
    fn what_card(&mut self, ui: &mut egui::Ui, st: &mut SettingsState) {
        ui::section(ui, tr("O que enviar"), |ui| {
            let t = &mut st.draft.telemetry;
            ui::field(
                ui,
                tr("Nível mínimo"),
                Some(tr(
                    "error/warn são leves; info é o padrão; debug consome a cota rápido.",
                )),
                |ui| {
                    ui::select(
                        ui,
                        "telemetry-level",
                        &mut t.level,
                        &[
                            ("error", "error"),
                            ("warn", "warn"),
                            ("info", "info"),
                            ("debug", "debug"),
                        ],
                    );
                },
            );
            ui::field(
                ui,
                tr("Rótulo da instância"),
                Some(tr(
                    "Nome livre desta máquina; vai em cada evento para distinguir instâncias no painel.",
                )),
                |ui| {
                    ui::text_input(ui, &mut t.label, tr("ex.: mac-do-will"));
                },
            );
            ui::field(
                ui,
                tr("Enviar traces"),
                Some(tr(
                    "Spans de turno, ferramenta, mensagem e integrações vão como traces OTLP (duração e correlação com os logs).",
                )),
                |ui| {
                    ui::toggle(ui, &mut t.send_traces);
                },
            );
            ui::field(
                ui,
                tr("Enviar logs deste app"),
                Some(tr(
                    "O aplicativo também manda os próprios logs (conexão, interface), marcados como source=app.",
                )),
                |ui| {
                    ui::toggle(ui, &mut t.ship_app_logs);
                },
            );
            ui::field(
                ui,
                tr("Enviar relatórios de falha"),
                Some(tr(
                    "Ao subir, o servidor envia will-ia-fatal.log, service.err.log e o log do instalador, se existirem.",
                )),
                |ui| {
                    ui::toggle(ui, &mut t.ship_crash_reports);
                },
            );
            privacy_note(ui);
        });
    }

    // ---- card 3: status ----
    fn status_card(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        ui::section(ui, tr("Status"), |ui| {
            let testing = self.test_task.is_some();
            let has_token = st.secret_present(TOKEN_KEY);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                if ui
                    .add_enabled(
                        !testing && has_token,
                        egui::Button::new(ui::icon_text(
                            ui,
                            icons::SEND,
                            tr("Enviar evento de teste"),
                            None,
                        )),
                    )
                    .on_disabled_hover_text(tr("Salve o token do Axiom primeiro."))
                    .on_hover_text(tr(
                        "Manda um evento com o token e o dataset salvos e mostra a resposta HTTP.",
                    ))
                    .clicked()
                {
                    self.test_task = Some(cx.svc.call(|c| async move { c.telemetry_test().await }));
                }
                if ui
                    .add_enabled(
                        !self.status.loading(),
                        egui::Button::new(ui::icon_text(ui, icons::REFRESH, tr("Atualizar"), None)),
                    )
                    .clicked()
                {
                    self.status.loaded_at = None;
                }
                if testing || self.status.loading() {
                    ui.spinner();
                }
            });
            if let Some(r) = &self.last_test {
                ui.add_space(4.0);
                match r {
                    Ok(v) if ui::b(v, "ok") => {
                        ui.colored_label(
                            Tone::Success.color(ui.visuals().dark_mode),
                            format!("✓ {}", ui::s(v, "message")),
                        );
                    }
                    Ok(v) => ui::error_text(ui, &ui::s(v, "message")),
                    Err(e) => ui::error_text(ui, e),
                }
            }
            ui.add_space(8.0);

            if let Some(e) = &self.status.error {
                ui::error_text(ui, e);
            }
            let Some(v) = self.status.data.clone() else {
                if self.status.error.is_none() {
                    ui::busy(ui, tr("carregando status…"));
                }
                return;
            };

            let sent = ui::i(&v, "sent");
            let failed = ui::i(&v, "failed");
            let spooled = ui::i(&v, "spooled");
            let spool_bytes = ui::i(&v, "spool_bytes");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                if ui::b(&v, "enabled") {
                    ui::badge(ui, tr("ativada"), Tone::Success);
                } else {
                    ui::badge(ui, tr("desativada"), Tone::Neutral);
                }
                if ui::b(&v, "configured") {
                    ui::badge(ui, tr("configurada"), Tone::Success);
                } else {
                    ui::badge(ui, tr("falta token ou dataset"), Tone::Warning);
                }
                ui::chip2(ui, tr("enviados"), &sent.to_string(), Tone::Info);
                ui::chip2(
                    ui,
                    tr("falhas"),
                    &failed.to_string(),
                    if failed > 0 {
                        Tone::Danger
                    } else {
                        Tone::Neutral
                    },
                );
                ui::chip2(
                    ui,
                    tr("na fila"),
                    &if spooled > 0 {
                        format!("{spooled} · {}", fmt_bytes(spool_bytes))
                    } else {
                        "0".to_string()
                    },
                    if spooled > 0 {
                        Tone::Warning
                    } else {
                        Tone::Neutral
                    },
                );
            });
            ui.add_space(8.0);

            ui::field(
                ui,
                tr("ID da instância"),
                Some(tr(
                    "Gerado uma vez por instalação; vai em cada evento (instance_id).",
                )),
                |ui| {
                    let id = ui::s(&v, "instance_id");
                    ui.horizontal(|ui| {
                        ui::mono(ui, if id.is_empty() { "—" } else { &id });
                        if !id.is_empty()
                            && ui::icon_button(ui, icons::CONTENT_COPY, tr("Copiar")).clicked()
                        {
                            ui.ctx().copy_text(id.clone());
                            cx.toasts.success(tr("ID da instância copiado."));
                        }
                    });
                },
            );
            ui::field(ui, tr("Último envio"), None, |ui| {
                let when = ui::fmt_dt(&v["last_sent_at"]);
                if when.is_empty() {
                    ui.label(egui::RichText::new(tr("nenhum ainda")).weak());
                } else {
                    let rel = v["last_sent_at"]
                        .as_str()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|d| ui::fmt_relative(&d.with_timezone(&chrono::Utc)))
                        .unwrap_or_default();
                    ui.label(if rel.is_empty() {
                        when
                    } else {
                        trf("{} (há {})", &[&when, &rel])
                    });
                }
            });
            ui::field(ui, tr("Último erro"), None, |ui| {
                let err = ui::s(&v, "last_error");
                if err.is_empty() {
                    ui.label(egui::RichText::new(tr("nenhum")).weak());
                } else {
                    ui::error_text(ui, &err);
                }
            });
            ui::field(ui, tr("Destino"), None, |ui| {
                ui::mono(
                    ui,
                    &format!("{} · {}", ui::s(&v, "endpoint"), ui::s(&v, "dataset")),
                );
            });
        });
    }
}

/// Aviso de privacidade: caixa discreta com ícone, dentro do card "O que enviar".
fn privacy_note(ui: &mut egui::Ui) {
    let dark = ui.visuals().dark_mode;
    let color = Tone::Info.color(dark);
    ui.add_space(2.0);
    egui::Frame::new()
        .fill(color.linear_multiply(if dark { 0.14 } else { 0.08 }))
        .stroke(egui::Stroke::new(1.0, color.linear_multiply(0.5)))
        .corner_radius(8.0)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui::icon_colored(ui, icons::INFO, 16.0, color);
                ui.add_space(4.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    ui.label(egui::RichText::new(tr("Privacidade")).font(ui::semibold(13.5)));
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(tr(
                                "Os eventos levam instante, nível, módulo, mensagem de erro e ids (thread, turno, instância). Nunca vão tokens ou segredos, e textos de conversas, e-mails, mensagens do WhatsApp ou conteúdo de arquivos não são enviados.",
                            ))
                            .weak()
                            .small(),
                        )
                        .wrap(),
                    );
                });
            });
        });
}

fn fmt_bytes(b: i64) -> String {
    let b = b.max(0) as f64;
    if b < 1024.0 {
        format!("{b:.0} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else {
        format!("{:.1} MB", b / (1024.0 * 1024.0))
    }
}
