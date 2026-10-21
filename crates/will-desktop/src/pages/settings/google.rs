//! Seção Google: OAuth Client do próprio usuário + contas conectadas, Agenda
//! (configuração e seleção de agendas) e Gmail (configuração).

use super::SettingsState;
use crate::pages::Cx;
use crate::service::{j, Loader, Task};
use crate::ui::{self, SecretAction, SecretField, Tone};
use axum::extract::{Path, State};
use axum::Json;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;
use will_core::{Event, Module};
use will_server::api::integrations as api;

/// Chave padrão do segredo no cofre (o `client_secret_ref` pode apontar para outra).
const DEFAULT_SECRET_KEY: &str = "google_client_secret";

/// Escopos lógicos oferecidos ao conectar: (chave aceita pelo serviço, rótulo).
const SCOPE_OPTIONS: [(&str, &str); 5] = [
    ("calendar", "Agenda (ler)"),
    ("calendar_write", "Agenda (criar/editar)"),
    ("gmail", "Gmail (ler)"),
    ("gmail_send", "Gmail (enviar)"),
    ("gmail_modify", "Gmail (arquivar/marcar/rotular)"),
];

pub struct GooglePage {
    accounts: Loader<Value>,
    calendars: Loader<Value>,
    client_secret: SecretField,
    show_connect: bool,
    connect_scopes: [bool; 5],
    connect_task: Option<Task<Value>>,
    reauth_task: Option<(String, Task<Value>)>,
    disconnect_task: Option<Task<Value>>,
    confirm_disconnect: Option<(String, String)>,
    flags_task: Option<Task<Value>>,
    /// Última URL de autorização devolvida pelo serviço (conectar ou reautorizar).
    auth_url: Option<String>,
}

impl Default for GooglePage {
    fn default() -> Self {
        Self {
            accounts: Loader::default(),
            calendars: Loader::default(),
            client_secret: SecretField::default(),
            show_connect: false,
            connect_scopes: [true, false, true, false, false],
            connect_task: None,
            reauth_task: None,
            disconnect_task: None,
            confirm_disconnect: None,
            flags_task: None,
            auth_url: None,
        }
    }
}

impl GooglePage {
    pub fn on_event(&mut self, ev: &Event) {
        let reload = match ev {
            Event::ModuleStatus { module, .. } => {
                matches!(module, Module::Google | Module::Calendar | Module::Gmail)
            }
            Event::SettingsChanged { section } => {
                matches!(section.as_str(), "google" | "calendar" | "gmail")
            }
            _ => false,
        };
        if reload {
            self.accounts.loaded_at = None;
            self.calendars.loaded_at = None;
        }
    }

    fn reload_accounts(&mut self, cx: &Cx) {
        self.accounts.start(
            cx.svc
                .call(|s| async move { j(api::google_accounts(State(s)).await) }),
        );
    }

    fn reload_calendars(&mut self, cx: &Cx) {
        self.calendars.start(
            cx.svc
                .call(|s| async move { j(api::google_calendars(State(s)).await) }),
        );
    }

    fn refresh_calendars(&mut self, cx: &Cx) {
        self.calendars.start(
            cx.svc
                .call(|s| async move { j(api::google_calendars_refresh(State(s)).await) }),
        );
    }

    fn set_flags(
        &mut self,
        cx: &Cx,
        account: String,
        calendar_id: String,
        enabled: bool,
        default_for_create: bool,
    ) {
        self.flags_task = Some(cx.svc.call(move |s| async move {
            j(api::google_calendar_flags(
                State(s),
                Path((account, calendar_id)),
                Json(api::CalFlags {
                    enabled,
                    default_for_create: Some(default_for_create),
                }),
            )
            .await)
        }));
    }

    fn poll(&mut self, cx: &mut Cx) {
        if self.accounts.stale(Duration::from_secs(60)) {
            self.reload_accounts(cx);
        }
        if self.calendars.stale(Duration::from_secs(120)) {
            self.reload_calendars(cx);
        }
        self.accounts.poll();
        if self.calendars.poll() {
            if let Some(e) = self.calendars.error.clone() {
                cx.toasts.error(format!("Agendas: {e}"));
            }
        }
        if let Some(t) = &self.connect_task {
            if let Some(r) = t.take() {
                self.connect_task = None;
                match r {
                    Ok(v) => {
                        self.auth_url = Some(ui::s(&v, "auth_url"));
                        self.show_connect = false;
                        cx.toasts
                            .info("Autorize no navegador; a conta aparece aqui ao concluir.");
                    }
                    Err(e) => cx.toasts.error(format!("Conectar: {e}")),
                }
            }
        }
        if let Some((_, t)) = &self.reauth_task {
            if let Some(r) = t.take() {
                self.reauth_task = None;
                match r {
                    Ok(v) => {
                        self.auth_url = Some(ui::s(&v, "auth_url"));
                        cx.toasts.info("Autorize de novo no navegador.");
                    }
                    Err(e) => cx.toasts.error(format!("Reautorizar: {e}")),
                }
            }
        }
        if let Some(t) = &self.disconnect_task {
            if let Some(r) = t.take() {
                self.disconnect_task = None;
                match r {
                    Ok(_) => {
                        cx.toasts.success("Conta desconectada.");
                        self.reload_accounts(cx);
                        self.reload_calendars(cx);
                    }
                    Err(e) => cx.toasts.error(format!("Desconectar: {e}")),
                }
            }
        }
        if let Some(t) = &self.flags_task {
            if let Some(r) = t.take() {
                self.flags_task = None;
                match r {
                    Ok(_) => self.reload_calendars(cx),
                    Err(e) => {
                        cx.toasts.error(format!("Agenda: {e}"));
                        self.reload_calendars(cx);
                    }
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        self.poll(cx);
        ui::heading(ui, "Google", Some("Agenda e Gmail com o OAuth Client do próprio usuário. Google não cria threads: e-mails e eventos são dados para as ferramentas calendar_* e gmail_*."));

        // ---- status dos módulos ----
        ui.horizontal_wrapped(|ui| {
            for m in [Module::Google, Module::Calendar, Module::Gmail] {
                let status = cx
                    .snap
                    .modules
                    .get(&m)
                    .cloned()
                    .unwrap_or(will_core::ModuleStatus::Stopped);
                ui::dot(ui, ui::tone_for_module(&status));
                ui.label(egui::RichText::new(ui::module_name(m)).strong());
                ui.label(egui::RichText::new(ui::module_label(&status)).weak());
                ui.add_space(12.0);
            }
        });

        self.account_section(ui, cx, st);
        self.calendar_section(ui, cx, st);
        self.gmail_section(ui, cx, st);
    }

    // ---------- 1. Conta Google ----------

    fn account_section(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        let secret_key = st
            .saved
            .google
            .client_secret_ref
            .clone()
            .filter(|k| !k.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_SECRET_KEY.to_string());
        let secret_present = st.secret_present(&secret_key);
        let client_id_saved = st
            .saved
            .google
            .client_id
            .as_deref()
            .is_some_and(|c| !c.trim().is_empty());

        ui::section(ui, "Conta Google (OAuth do próprio usuário)", |ui| {
            ui.label(egui::RichText::new("O will-ia não embute credenciais do Google: você cria um cliente OAuth no seu próprio projeto. Passo a passo (console.cloud.google.com):").weak());
            ui.add_space(4.0);
            egui::CollapsingHeader::new("Passo a passo para criar o cliente OAuth").default_open(!client_id_saved).show(ui, |ui| {
                let step = |ui: &mut egui::Ui, n: &str, title: &str, body: &str| {
                    ui.horizontal_top(|ui| {
                        ui.label(egui::RichText::new(n).strong().color(Tone::Info.color(true)));
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(title).strong());
                            ui.add(egui::Label::new(egui::RichText::new(body).weak().small()).wrap());
                        });
                    });
                    ui.add_space(4.0);
                };
                step(ui, "1", "Projeto", "No topo do console, selecione ou crie um projeto (ex.: \"will-ia\").");
                step(ui, "2", "Ativar as APIs", "Menu › APIs e serviços › Biblioteca. Procure e ative \"Google Calendar API\" e \"Gmail API\".");
                step(ui, "3", "Tela de consentimento OAuth", "APIs e serviços › Tela de permissão OAuth (ou \"Google Auth Platform › Branding\"). Tipo de usuário: Externo. Preencha nome do app e e-mails de suporte. Em \"Público-alvo\" (Test users) adicione o seu próprio e-mail do Google.");
                step(ui, "4", "Publicar o app", "Ainda na tela de consentimento, clique em \"Publicar aplicativo\". Em modo Teste o Google expira o refresh token em 7 dias e você teria de reautorizar toda semana. Não é preciso enviar para verificação: use normalmente com o aviso de \"app não verificado\" (clique em Avançado › Acessar).");
                step(ui, "5", "Criar o cliente", "APIs e serviços › Credenciais › Criar credenciais › ID do cliente OAuth. Tipo de aplicativo: **App para computador** (Desktop app). Dê um nome e clique em Criar.");
                step(ui, "6", "Copiar e colar aqui", "Na janela que abre, copie o \"ID do cliente\" (termina em .apps.googleusercontent.com) e a \"Chave secreta do cliente\" (começa com GOCSPX-). Cole nos campos abaixo e salve.");
                step(ui, "7", "Conectar", "Clique em \"Conectar conta\" mais abaixo, escolha os escopos e autorize no navegador. A conta aparece na lista ao concluir.");
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Erro \"redirect_uri_mismatch\" ao autorizar?").strong());
                ui.add(egui::Label::new(egui::RichText::new("Significa que o cliente foi criado como \"Aplicativo da Web\" em vez de \"App para computador\". Só o tipo computador aceita o redirecionamento http://127.0.0.1 com porta aleatória sem cadastro. Resolva de um dos dois jeitos: crie um cliente novo do tipo App para computador (recomendado), ou fixe uma porta abaixo e cadastre exatamente a URI mostrada em \"URIs de redirecionamento autorizados\" do cliente Web.").weak().small()).wrap());
            });
            ui.add_space(6.0);

            ui::field(
                ui,
                "client_id",
                Some("Termina em .apps.googleusercontent.com"),
                |ui| {
                    ui::optional_text(
                        ui,
                        &mut st.draft.google.client_id,
                        "xxxx.apps.googleusercontent.com",
                    );
                },
            );
            let mut action = SecretAction::None;
            ui::field(
                ui,
                "client_secret",
                Some("Vai para o cofre de segredos; nunca para o banco ou logs."),
                |ui| {
                    action = self.client_secret.ui(ui, secret_present, "GOCSPX-…");
                },
            );
            match action {
                SecretAction::Save(v) => st.set_secret(cx.svc, &secret_key, v),
                SecretAction::Delete => st.delete_secret(cx.svc, &secret_key),
                SecretAction::None => {}
            }
            ui::field(
                ui,
                "Porta do callback",
                Some("Vazio = aleatória (cliente App para computador). Fixe só se usar um cliente Web e cadastre a URI ao lado no Google."),
                |ui| {
                    ui.horizontal(|ui| {
                        let g = &mut st.draft.google;
                        let mut text = g.redirect_port.map(|p| p.to_string()).unwrap_or_default();
                        if ui.add(egui::TextEdit::singleline(&mut text).hint_text("aleatória").desired_width(100.0)).changed() {
                            g.redirect_port = text.trim().parse().ok();
                        }
                        if let Some(p) = g.redirect_port {
                            let uri = format!("http://127.0.0.1:{p}/oauth/google/callback");
                            ui::mono(ui, &uri);
                            if ui.button("Copiar URI").clicked() {
                                ui.ctx().copy_text(uri);
                                cx.toasts.success("URI de redirecionamento copiada.");
                            }
                        }
                    });
                },
            );
            st.save_bar(ui, cx.svc, "google");

            ui.add_space(10.0);
            ui.separator();
            ui.label(egui::RichText::new("Contas conectadas").strong());
            ui.add_space(4.0);
            if let Some(e) = &self.accounts.error {
                ui::error_text(ui, e);
            }
            let list = self
                .accounts
                .data
                .as_ref()
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if list.is_empty() {
                if self.accounts.loading() {
                    ui::busy(ui, "carregando contas…");
                } else {
                    ui.label(egui::RichText::new("Nenhuma conta conectada.").weak());
                }
            }
            for a in &list {
                let id = ui::s(a, "id");
                let email = ui::s(a, "email");
                let status = ui::s(a, "status");
                egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(0, 4))
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(egui::RichText::new(&email).strong());
                            let (label, tone) = account_status(&status);
                            ui::badge(ui, label, tone);
                            for sc in scope_labels(a) {
                                ui::badge(ui, &sc, Tone::Info);
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            let connected = ui::fmt_dt(&a["connected_at"]);
                            if !connected.is_empty() {
                                ui.label(
                                    egui::RichText::new(format!("conectada em {connected}"))
                                        .weak()
                                        .small(),
                                );
                            }
                            let used = ui::fmt_dt(&a["last_used_at"]);
                            if !used.is_empty() {
                                ui.label(
                                    egui::RichText::new(format!("· último uso {used}"))
                                        .weak()
                                        .small(),
                                );
                            }
                            ui.label(egui::RichText::new(format!("· id {id}")).weak().small());
                        });
                        ui.horizontal(|ui| {
                            let reauthorizing =
                                self.reauth_task.as_ref().is_some_and(|(i, _)| i == &id);
                            if reauthorizing {
                                ui.spinner();
                            }
                            if ui
                                .add_enabled(!reauthorizing, egui::Button::new("Reautorizar"))
                                .clicked()
                            {
                                let id2 = id.clone();
                                self.reauth_task = Some((
                                    id.clone(),
                                    cx.svc.call(move |s| async move {
                                        j(api::google_reauthorize(State(s), Path(id2)).await)
                                    }),
                                ));
                            }
                            if ui
                                .add_enabled(
                                    self.disconnect_task.is_none(),
                                    egui::Button::new("Desconectar"),
                                )
                                .clicked()
                            {
                                self.confirm_disconnect = Some((id.clone(), email.clone()));
                            }
                        });
                    });
            }

            ui.add_space(8.0);
            let can_connect = client_id_saved && secret_present;
            if !can_connect {
                ui.label(
                    egui::RichText::new(
                        "Salve o client_id e o client_secret acima para poder conectar uma conta.",
                    )
                    .weak()
                    .small(),
                );
            }
            if !self.show_connect {
                if ui
                    .add_enabled(
                        can_connect,
                        egui::Button::new(
                            egui::RichText::new("Conectar conta").color(egui::Color32::WHITE),
                        )
                        .fill(Tone::Info.color(true)),
                    )
                    .clicked()
                {
                    self.show_connect = true;
                }
            } else {
                ui.label(
                    egui::RichText::new(
                        "Escopos a solicitar (podem ser ampliados depois com Reautorizar):",
                    )
                    .small(),
                );
                for (i, (_, label)) in SCOPE_OPTIONS.iter().enumerate() {
                    ui.checkbox(&mut self.connect_scopes[i], *label);
                }
                ui.horizontal(|ui| {
                    let any = self.connect_scopes.iter().any(|b| *b);
                    let connecting = self.connect_task.is_some();
                    if ui
                        .add_enabled(
                            can_connect && any && !connecting,
                            egui::Button::new("Abrir navegador para autorizar"),
                        )
                        .clicked()
                    {
                        let scopes: Vec<String> = SCOPE_OPTIONS
                            .iter()
                            .zip(self.connect_scopes.iter())
                            .filter(|(_, on)| **on)
                            .map(|((k, _), _)| k.to_string())
                            .collect();
                        self.connect_task = Some(cx.svc.call(move |s| async move {
                            j(api::google_connect(
                                State(s),
                                Some(Json(api::ConnectBody {
                                    scopes: Some(scopes),
                                    open_browser: Some(true),
                                })),
                            )
                            .await)
                        }));
                    }
                    if ui.button("Cancelar").clicked() {
                        self.show_connect = false;
                    }
                    if connecting {
                        ui.spinner();
                    }
                });
            }

            if let Some(url) = self.auth_url.clone() {
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Autorize no navegador; a conta aparece aqui ao concluir. Se o navegador não abriu, use a URL abaixo:").small());
                ui::mono(ui, &url);
                ui.horizontal(|ui| {
                    if ui.button("copiar URL").clicked() {
                        ui.ctx().copy_text(url.clone());
                        cx.toasts.info("URL copiada.");
                    }
                    if ui.button("abrir no navegador").clicked() {
                        ui.ctx().open_url(egui::OpenUrl::new_tab(&url));
                    }
                    if ui.button("fechar").clicked() {
                        self.auth_url = None;
                    }
                });
            }
        });

        if let Some((id, email)) = self.confirm_disconnect.clone() {
            let c = ui::Confirm {
                title: "Desconectar conta Google".into(),
                body: format!("Desconectar {email}? O refresh token é revogado e apagado do cofre; as agendas e ferramentas dessa conta deixam de funcionar até conectar de novo."),
                confirm_label: "Desconectar".into(),
                danger: true,
                tag: "google-disconnect".into(),
            };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_disconnect = None;
                if ok {
                    self.disconnect_task = Some(cx.svc.call(move |s| async move {
                        j(api::google_disconnect(State(s), Path(id)).await)
                    }));
                }
            }
        }
    }

    // ---------- 2. Agenda ----------

    fn calendar_section(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        ui::section(ui, "Agenda", |ui| {
            let c = &mut st.draft.calendar;
            ui::field(
                ui,
                "Habilitar",
                Some("Ferramentas calendar_* para os agentes."),
                |ui| {
                    ui::toggle(ui, &mut c.enabled);
                },
            );
            ui::field(ui, "Permitir criar/editar", Some("Exige o escopo Agenda (criar/editar). As ferramentas nascem com política confirmar."), |ui| {
                ui::toggle(ui, &mut c.allow_write);
            });
            ui::field(
                ui,
                "Permitir excluir",
                Some("calendar_delete_event; política confirmar forçada."),
                |ui| {
                    ui::toggle(ui, &mut c.allow_delete);
                },
            );
            ui::field(
                ui,
                "Enviar convites a participantes",
                Some("Ao criar/editar eventos com convidados."),
                |ui| {
                    ui::toggle(ui, &mut c.send_updates);
                },
            );
            ui::field(
                ui,
                "Fuso padrão",
                Some("Vazio = automático. Ex.: America/Sao_Paulo"),
                |ui| {
                    ui::optional_text(ui, &mut c.timezone, "automático");
                },
            );
            ui::field(ui, "Janela máxima por consulta (dias)", None, |ui| {
                ui::number(ui, &mut c.max_window_days, 1..=366);
            });
            st.save_bar(ui, cx.svc, "calendar");

            ui.add_space(10.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Agendas").strong());
                let loading = self.calendars.loading();
                if ui
                    .add_enabled(!loading, egui::Button::new("Atualizar agendas"))
                    .clicked()
                {
                    self.refresh_calendars(cx);
                }
                if loading || self.flags_task.is_some() {
                    ui.spinner();
                }
            });
            ui.label(egui::RichText::new("Marque as agendas que o agente pode consultar; a padrão recebe os eventos criados sem agenda explícita.").weak().small());
            if let Some(e) = &self.calendars.error {
                ui::error_text(ui, e);
            }
            let list = self
                .calendars
                .data
                .as_ref()
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if list.is_empty() && !self.calendars.loading() {
                ui.label(egui::RichText::new("Nenhuma agenda conhecida. Conecte uma conta com escopo de Agenda e clique em Atualizar agendas.").weak());
            }
            let mut groups: BTreeMap<String, (String, Vec<Value>)> = BTreeMap::new();
            for cal in list {
                let acc = ui::s(&cal, "account_id");
                let email = ui::s(&cal, "account_email");
                let entry = groups
                    .entry(acc.clone())
                    .or_insert_with(|| (email.clone(), Vec::new()));
                if entry.0.is_empty() {
                    entry.0 = email;
                }
                entry.1.push(cal);
            }
            let busy = self.flags_task.is_some();
            for (acc, (email, cals)) in groups {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(if email.is_empty() { acc.clone() } else { email })
                        .strong()
                        .small(),
                );
                egui::Grid::new(("calendars", &acc))
                    .num_columns(5)
                    .striped(true)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        for cal in &cals {
                            let cal_id = ui::s(cal, "calendar_id");
                            let mut enabled = ui::b(cal, "enabled");
                            let is_default = ui::b(cal, "default_for_create");
                            let r =
                                ui.add_enabled(!busy, egui::Checkbox::new(&mut enabled, "usar"));
                            if r.changed() {
                                self.set_flags(
                                    cx,
                                    acc.clone(),
                                    cal_id.clone(),
                                    enabled,
                                    is_default && enabled,
                                );
                            }
                            if ui
                                .add_enabled(
                                    !busy,
                                    egui::RadioButton::new(is_default, "padrão para criar"),
                                )
                                .clicked()
                                && !is_default
                            {
                                self.set_flags(cx, acc.clone(), cal_id.clone(), true, true);
                            }
                            color_swatch(ui, &ui::s(cal, "color"));
                            ui.horizontal(|ui| {
                                let summary = ui::s(cal, "summary");
                                ui.label(ui::ellipsize(&summary, 36))
                                    .on_hover_text(&summary);
                                let role = ui::s(cal, "access_role");
                                if !role.is_empty() {
                                    ui::badge(
                                        ui,
                                        &role_label(&role),
                                        if role == "owner" || role == "writer" {
                                            Tone::Success
                                        } else {
                                            Tone::Neutral
                                        },
                                    );
                                }
                            });
                            // id da agenda é longo (…@group.calendar.google.com):
                            // mostra truncado, completo no hover, e copia ao clicar.
                            let tz = ui::s(cal, "timezone");
                            let short = ui::ellipsize(&cal_id, 22);
                            let text = if tz.is_empty() {
                                short
                            } else {
                                format!("{tz} · {short}")
                            };
                            let r = ui
                                .add(
                                    egui::Label::new(egui::RichText::new(text).weak().small())
                                        .sense(egui::Sense::click()),
                                )
                                .on_hover_text(format!("{cal_id}\n(clique para copiar o id)"));
                            if r.clicked() {
                                ui.ctx().copy_text(cal_id.clone());
                                cx.toasts.success("Id da agenda copiado.");
                            }
                            ui.end_row();
                        }
                    });
            }
        });
    }

    // ---------- 3. Gmail ----------

    fn gmail_section(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        let accounts = self
            .accounts
            .data
            .as_ref()
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        ui::section(ui, "Gmail", |ui| {
            let g = &mut st.draft.gmail;
            ui::field(ui, "Habilitar", Some("Ferramentas gmail_* para os agentes. E-mails recebidos são dados, nunca gatilhos."), |ui| {
                ui::toggle(ui, &mut g.enabled);
            });
            ui::field(
                ui,
                "Contas usadas",
                Some("Nenhuma marcada = todas as contas conectadas com escopo de Gmail."),
                |ui| {
                    if accounts.is_empty() {
                        ui.label(
                            egui::RichText::new("Nenhuma conta conectada.")
                                .weak()
                                .small(),
                        );
                    }
                    for a in &accounts {
                        let id = ui::s(a, "id");
                        let email = ui::s(a, "email");
                        let mut on = g.accounts.iter().any(|x| x == &id || x == &email);
                        if ui.checkbox(&mut on, &email).changed() {
                            g.accounts.retain(|x| x != &id && x != &email);
                            if on {
                                g.accounts.push(id.clone());
                            }
                        }
                    }
                    let known: Vec<String> = accounts
                        .iter()
                        .flat_map(|a| [ui::s(a, "id"), ui::s(a, "email")])
                        .collect();
                    let unknown: Vec<String> = g
                        .accounts
                        .iter()
                        .filter(|x| !known.contains(x))
                        .cloned()
                        .collect();
                    for x in unknown {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{x} (conta não conectada)"))
                                    .weak()
                                    .small(),
                            );
                            if ui.button("remover").clicked() {
                                g.accounts.retain(|y| y != &x);
                            }
                        });
                    }
                },
            );
            ui::field(
                ui,
                "Permitir envio",
                Some("gmail_send nasce com política confirmar; exige o escopo Gmail (enviar)."),
                |ui| {
                    ui::toggle(ui, &mut g.allow_send);
                },
            );
            ui::field(ui, "Permitir modificar", Some("Arquivar, marcar lida, rotular (gmail_modify, política confirmar). Exige o escopo Gmail (arquivar/marcar/rotular)."), |ui| {
                ui::toggle(ui, &mut g.allow_modify);
            });
            ui::field(
                ui,
                "Só destinatários já vistos",
                Some("Envio apenas para endereços que já apareceram em e-mails lidos/enviados."),
                |ui| {
                    ui::toggle(ui, &mut g.only_known_recipients);
                },
            );
            ui::field(ui, "Rate limit de envio (por hora)", None, |ui| {
                ui::number(ui, &mut g.send_rate_limit_per_hour, 1..=1000);
            });
            ui::field(
                ui,
                "Tamanho máx. de corpo (KB)",
                Some("Corpos maiores são truncados na leitura."),
                |ui| {
                    ui::number(ui, &mut g.max_body_kb, 1..=4096);
                },
            );
            ui::field(
                ui,
                "Pasta de anexos",
                Some("Relativa à base de conhecimento."),
                |ui| {
                    ui::text_input(ui, &mut g.attachments_dir, "inbox/gmail");
                },
            );
            ui::field(ui, "Limite de anexo (MB)", None, |ui| {
                ui::number(ui, &mut g.max_attachment_mb, 1..=100);
            });
            st.save_bar(ui, cx.svc, "gmail");
        });
    }
}

// ---------- helpers de apresentação ----------

fn account_status(status: &str) -> (&'static str, Tone) {
    match status {
        "ok" => ("ok", Tone::Success),
        "reauthorize" => ("reautorizar", Tone::Warning),
        "" => ("desconhecido", Tone::Neutral),
        _ => ("erro", Tone::Danger),
    }
}

/// Escopos concedidos (URLs do Google ou chaves lógicas) → rótulos curtos; omite identidade.
fn scope_labels(account: &Value) -> Vec<String> {
    let Some(arr) = account.get("scopes").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for sc in arr.iter().filter_map(|s| s.as_str()) {
        let label = match sc {
            "openid" | "email" | "profile" => continue,
            s if s.ends_with("calendar.readonly") || s == "calendar" || s == "calendar_read" => {
                "Agenda (ler)"
            }
            s if s.ends_with("calendar.events")
                || s.ends_with("/calendar")
                || s == "calendar_write" =>
            {
                "Agenda (criar/editar)"
            }
            s if s.ends_with("gmail.readonly") || s == "gmail" || s == "gmail_read" => {
                "Gmail (ler)"
            }
            s if s.ends_with("gmail.send") || s == "gmail_send" => "Gmail (enviar)",
            s if s.ends_with("gmail.modify") || s == "gmail_modify" => {
                "Gmail (arquivar/marcar/rotular)"
            }
            s if s.ends_with("userinfo.email") || s.ends_with("userinfo.profile") => continue,
            s => s.rsplit('/').next().unwrap_or(s),
        };
        if !out.iter().any(|l| l == label) {
            out.push(label.to_string());
        }
    }
    out
}

fn role_label(role: &str) -> String {
    match role {
        "owner" => "dona".into(),
        "writer" => "escrita".into(),
        "reader" => "leitura".into(),
        "freeBusyReader" => "livre/ocupado".into(),
        other => other.to_string(),
    }
}

/// Quadradinho com a cor da agenda (`#rrggbb`); cinza se inválida.
fn color_swatch(ui: &mut egui::Ui, hex: &str) {
    let color = parse_hex_color(hex).unwrap_or(egui::Color32::GRAY);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 3.0, color);
}

fn parse_hex_color(hex: &str) -> Option<egui::Color32> {
    let h = hex.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(egui::Color32::from_rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colors() {
        assert_eq!(
            parse_hex_color("#ff0000"),
            Some(egui::Color32::from_rgb(255, 0, 0))
        );
        assert_eq!(
            parse_hex_color("00ff00"),
            Some(egui::Color32::from_rgb(0, 255, 0))
        );
        assert_eq!(parse_hex_color("#abc"), None);
        assert_eq!(parse_hex_color(""), None);
    }

    #[test]
    fn scope_labels_dedup_and_skip_identity() {
        let a = serde_json::json!({"scopes": ["openid", "email", "https://www.googleapis.com/auth/calendar.readonly", "https://www.googleapis.com/auth/calendar.events", "https://www.googleapis.com/auth/gmail.readonly", "gmail"]});
        assert_eq!(
            scope_labels(&a),
            vec!["Agenda (ler)", "Agenda (criar/editar)", "Gmail (ler)"]
        );
    }
}
