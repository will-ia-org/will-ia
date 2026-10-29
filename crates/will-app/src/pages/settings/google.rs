//! Seção Google: OAuth Client do próprio usuário + contas conectadas, Agenda
//! (configuração e seleção de agendas) e Gmail (configuração).

use super::SettingsState;
use crate::help;
use crate::i18n::{tr, trf};
use crate::oauth_cb;
use crate::pages::Cx;
use crate::service::{Loader, Task};
use crate::ui::{self, SecretAction, SecretField, Tone};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use will_core::{Event, Module};

/// Chave padrão do segredo no cofre (o `client_secret_ref` pode apontar para outra).
const DEFAULT_SECRET_KEY: &str = "google_client_secret";

/// Escopos lógicos oferecidos ao conectar: (chave aceita pelo serviço, rótulo pt-BR;
/// traduzir com `tr` ao exibir). `N_SCOPES` dimensiona os vetores de marcação —
/// um índice fora do tamanho derrubava o app ao abrir "Conectar conta".
const N_SCOPES: usize = 7;
const SCOPE_OPTIONS: [(&str, &str); N_SCOPES] = [
    ("calendar", "Agenda (ler)"),
    ("calendar_write", "Agenda (criar/editar)"),
    ("gmail", "Gmail (ler)"),
    ("gmail_send", "Gmail (enviar)"),
    ("gmail_modify", "Gmail (arquivar/marcar/rotular)"),
    ("drive", "Drive (ler)"),
    ("drive_write", "Drive (ler e escrever)"),
];

/// Fluxo OAuth em andamento: o app escuta o callback no loopback local e
/// repassa o `code` ao servidor. `reauth_id` distingue reautorizar de conectar.
struct Flow {
    task: Task<Value>,
    url: Arc<Mutex<Option<String>>>,
    reauth_id: Option<String>,
}

pub struct GooglePage {
    accounts: Loader<Value>,
    calendars: Loader<Value>,
    client_secret: SecretField,
    show_connect: bool,
    connect_scopes: [bool; N_SCOPES],
    /// Reautorização com escolha de escopos: (id da conta, marcação por opção).
    reauth_pick: Option<(String, [bool; N_SCOPES])>,
    flow: Option<Flow>,
    /// URL de autorização a abrir no navegador do cliente (uma vez).
    open_url_once: Option<String>,
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
            connect_scopes: [true, false, true, false, false, false, false],
            reauth_pick: None,
            flow: None,
            open_url_once: None,
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
        self.accounts
            .start(cx.svc.call(|c| async move { c.google_accounts().await }));
    }

    fn reload_calendars(&mut self, cx: &Cx) {
        self.calendars.start(
            cx.svc
                .call(|c| async move { c.google_calendars(&[]).await }),
        );
    }

    fn refresh_calendars(&mut self, cx: &Cx) {
        self.calendars.start(
            cx.svc
                .call(|c| async move { c.google_calendars_refresh().await }),
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
        self.flags_task = Some(cx.svc.call(move |c| async move {
            c.google_calendar_flags(
                &account,
                &calendar_id,
                json!({
                    "enabled": enabled,
                    "default_for_create": default_for_create,
                }),
            )
            .await
        }));
    }

    /// Inicia um fluxo OAuth: abre o listener local, pede ao servidor a URL de
    /// autorização, abre o navegador e repassa o `code` ao concluir.
    fn start_oauth(
        &mut self,
        cx: &Cx,
        reauth_id: Option<String>,
        scopes: Option<Vec<String>>,
        fixed_port: Option<u16>,
    ) {
        let url = Arc::new(Mutex::new(None));
        let url2 = url.clone();
        let reauth = reauth_id.clone();
        let task = cx.svc.call(move |c| async move {
            let (listener, port) = oauth_cb::bind(fixed_port).await?;
            let body = match &reauth {
                Some(id) => {
                    c.google_reauthorize(
                        id,
                        Some(json!({
                            "callback_port": port,
                            "open_browser": false,
                            "scopes": scopes,
                        })),
                    )
                    .await?
                }
                None => {
                    c.google_connect(Some(json!({
                        "scopes": scopes,
                        "callback_port": port,
                        "open_browser": false,
                    })))
                    .await?
                }
            };
            let auth_url = body
                .get("auth_url")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            let state = body
                .get("state")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            if auth_url.is_empty() || state.is_empty() {
                return Err("servidor não devolveu auth_url/state do fluxo".into());
            }
            if let Ok(mut g) = url2.lock() {
                *g = Some(auth_url.clone());
            }
            let code =
                oauth_cb::wait_code(listener, &state, std::time::Duration::from_secs(300)).await?;
            c.google_callback(&state, &code).await
        });
        self.flow = Some(Flow {
            task,
            url,
            reauth_id,
        });
        self.auth_url = None;
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
                cx.toasts.error(trf("Agendas: {}", &[&e]));
            }
        }
        // Fluxo OAuth (conectar/reautorizar): o app publica a URL para abrir no
        // navegador e, ao receber o code no loopback, o servidor conclui.
        let mut new_url = None;
        let mut finished = None;
        if let Some(flow) = self.flow.as_ref() {
            if self.auth_url.is_none() {
                if let Ok(g) = flow.url.lock() {
                    if let Some(u) = g.clone() {
                        new_url = Some(u);
                    }
                }
            }
            if let Some(r) = flow.task.take() {
                finished = Some(r);
            }
        }
        if let Some(u) = new_url {
            self.auth_url = Some(u.clone());
            self.open_url_once = Some(u);
        }
        if let Some(r) = finished {
            self.flow = None;
            match r {
                Ok(v) => {
                    let email = v
                        .get("email")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if email.is_empty() {
                        cx.toasts.success(tr("Conta Google conectada."));
                    } else {
                        cx.toasts
                            .success(trf("Conta Google {} conectada.", &[&email]));
                    }
                    self.auth_url = None;
                    self.show_connect = false;
                    self.reload_accounts(cx);
                    self.reload_calendars(cx);
                }
                Err(e) => cx.toasts.error(trf("Autorização: {}", &[&e])),
            }
        }
        if let Some(t) = &self.disconnect_task {
            if let Some(r) = t.take() {
                self.disconnect_task = None;
                match r {
                    Ok(_) => {
                        cx.toasts.success(tr("Conta desconectada."));
                        self.reload_accounts(cx);
                        self.reload_calendars(cx);
                    }
                    Err(e) => cx.toasts.error(trf("Desconectar: {}", &[&e])),
                }
            }
        }
        if let Some(t) = &self.flags_task {
            if let Some(r) = t.take() {
                self.flags_task = None;
                match r {
                    Ok(_) => self.reload_calendars(cx),
                    Err(e) => {
                        cx.toasts.error(trf("Agenda: {}", &[&e]));
                        self.reload_calendars(cx);
                    }
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        self.poll(cx);
        if let Some(u) = self.open_url_once.take() {
            ui.ctx().open_url(egui::OpenUrl::new_tab(&u));
        }
        ui::heading(ui, tr("Google"), Some(tr("Agenda e Gmail com o OAuth Client do próprio usuário. Google não cria threads: e-mails e eventos são dados para as ferramentas calendar_* e gmail_*.")));

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

        let tone = |m: Module| {
            cx.snap
                .modules
                .get(&m)
                .map(ui::tone_for_module)
                .or(Some(Tone::Neutral))
        };
        let tab = ui::tabs(
            ui,
            "google",
            &[
                ui::Tab::new("account", tr("Conta")).tone(tone(Module::Google)),
                ui::Tab::new("calendar", tr("Agenda")).tone(tone(Module::Calendar)),
                ui::Tab::new("gmail", tr("Gmail")).tone(tone(Module::Gmail)),
                ui::Tab::new("drive", tr("Drive")).tone(tone(Module::Drive)),
            ],
        );
        match tab {
            "calendar" => self.calendar_section(ui, cx, st),
            "gmail" => self.gmail_section(ui, cx, st),
            "drive" => self.drive_section(ui, cx, st),
            _ => self.account_section(ui, cx, st),
        }
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
        let redirect_port = st.saved.google.redirect_port;
        let client_id_saved = st
            .saved
            .google
            .client_id
            .as_deref()
            .is_some_and(|c| !c.trim().is_empty());

        ui::section_help(
            ui,
            tr("Conta Google (OAuth do próprio usuário)"),
            Some(&help::GOOGLE_OAUTH),
            |ui| {
                if !client_id_saved {
                    ui.label(egui::RichText::new(tr("Ainda sem credenciais? O ícone ? no título tem o passo a passo no Google Cloud.")).weak().small());
                    ui.add_space(4.0);
                }
                ui::field(
                    ui,
                    tr("client_id"),
                    Some(tr("Termina em .apps.googleusercontent.com")),
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
                    tr("client_secret"),
                    Some(tr(
                        "Vai para o cofre de segredos; nunca para o banco ou logs.",
                    )),
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
                tr("Porta do callback"),
                Some(tr("Vazio = aleatória (cliente App para computador). Fixe só se usar um cliente Web e cadastre a URI ao lado no Google.")),
                |ui| {
                    ui.horizontal(|ui| {
                        let g = &mut st.draft.google;
                        let mut text = g.redirect_port.map(|p| p.to_string()).unwrap_or_default();
                        if ui.add(egui::TextEdit::singleline(&mut text).hint_text(tr("aleatória")).desired_width(100.0)).changed() {
                            g.redirect_port = text.trim().parse().ok();
                        }
                        if let Some(p) = g.redirect_port {
                            let uri = format!("http://127.0.0.1:{p}/oauth/google/callback");
                            ui::mono(ui, &uri);
                            if ui.button(tr("Copiar URI")).clicked() {
                                ui.ctx().copy_text(uri);
                                cx.toasts.success(tr("URI de redirecionamento copiada."));
                            }
                        }
                    });
                },
            );
                st.save_bar(ui, cx.svc, "google");

                ui.add_space(10.0);
                ui.separator();
                ui.label(egui::RichText::new(tr("Contas conectadas")).strong());
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
                        ui::busy(ui, tr("carregando contas…"));
                    } else {
                        ui.label(egui::RichText::new(tr("Nenhuma conta conectada.")).weak());
                    }
                }
                for a in &list {
                    let id = ui::s(a, "id");
                    let email = ui::s(a, "email");
                    let status = ui::s(a, "status");
                    let pal = ui::palette(ui.visuals().dark_mode);
                    egui::Frame::new()
                        .fill(pal.card_alt)
                        .stroke(egui::Stroke::new(1.0, pal.border))
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::symmetric(14, 10))
                        .outer_margin(egui::Margin { left: 0, right: 0, top: 0, bottom: 6 })
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            let (label, tone) = account_status(&status);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 8.0;
                                ui::dot(ui, tone);
                                ui.label(egui::RichText::new(&email).font(ui::semibold(14.0)));
                                ui::badge(ui, label, tone);
                            });
                            // escopos agrupados por produto (um chip por produto; o que
                            // falta para os módulos ligados aparece em amarelo)
                            let missing_now = missing_scopes(a, &st.saved);
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                                ui.add_space(18.0);
                                for (head, detail) in scope_groups(a) {
                                    ui::chip2(ui, head, &detail, Tone::Info);
                                }
                                for k in &missing_now {
                                    let (head, detail) = scope_split(k);
                                    ui::chip2(ui, head, &trf("falta: {}", &[&detail]), Tone::Warning);
                                }
                            });
                            ui.add_space(2.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.add_space(18.0);
                                let connected = ui::fmt_dt(&a["connected_at"]);
                                if !connected.is_empty() {
                                    ui.label(
                                        egui::RichText::new(trf("conectada em {}", &[&connected]))
                                            .weak()
                                            .small(),
                                    );
                                }
                                let used = ui::fmt_dt(&a["last_used_at"]);
                                if !used.is_empty() {
                                    ui.label(
                                        egui::RichText::new(trf("· último uso {}", &[&used]))
                                            .weak()
                                            .small(),
                                    );
                                }
                                ui.label(
                                    egui::RichText::new(format!("· id {}", id.get(..8).unwrap_or(&id)))
                                        .weak()
                                        .small(),
                                )
                                .on_hover_text(&id);
                            });
                            let missing = missing_now;
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.add_space(18.0);
                                let reauthorizing = self
                                    .flow
                                    .as_ref()
                                    .is_some_and(|f| f.reauth_id.as_deref() == Some(id.as_str()));
                                let busy_flow = self.flow.is_some();
                                if reauthorizing {
                                    ui.spinner();
                                }
                                let picking = self
                                    .reauth_pick
                                    .as_ref()
                                    .is_some_and(|(pid, _)| pid == &id);
                                if ui
                                    .add_enabled(
                                        !busy_flow && !picking,
                                        egui::Button::new(tr("Reautorizar / ampliar escopos")),
                                    )
                                    .on_hover_text(tr("Renova o consentimento e permite acrescentar escopos (ex.: Drive) sem desconectar a conta."))
                                    .clicked()
                                {
                                    let granted = granted_logical(a);
                                    let mut pick = [false; N_SCOPES];
                                    for (i, (k, _)) in SCOPE_OPTIONS.iter().enumerate() {
                                        pick[i] = granted.contains(k) || missing.contains(k);
                                    }
                                    self.reauth_pick = Some((id.clone(), pick));
                                }
                                if ui
                                    .add_enabled(
                                        self.disconnect_task.is_none(),
                                        egui::Button::new(tr("Desconectar")),
                                    )
                                    .clicked()
                                {
                                    self.confirm_disconnect = Some((id.clone(), email.clone()));
                                }
                            });
                            if let Some((_, pick)) = self
                                .reauth_pick
                                .as_mut()
                                .filter(|(pid, _)| pid == &id)
                            {
                                let granted = granted_logical(a);
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(tr(
                                        "Escopos desta conta (os já concedidos continuam; marque os novos):",
                                    ))
                                    .small(),
                                );
                                for (i, (k, label)) in SCOPE_OPTIONS.iter().enumerate() {
                                    let has = granted.contains(k);
                                    if has {
                                        pick[i] = true;
                                    }
                                    ui.add_enabled(!has, egui::Checkbox::new(&mut pick[i], tr(label)));
                                }
                                let mut go = None;
                                ui.horizontal(|ui| {
                                    if ui::primary_button(ui, tr("Abrir navegador para autorizar"))
                                        .clicked()
                                    {
                                        go = Some(
                                            SCOPE_OPTIONS
                                                .iter()
                                                .zip(pick.iter())
                                                .filter(|(_, on)| **on)
                                                .map(|((k, _), _)| k.to_string())
                                                .collect::<Vec<_>>(),
                                        );
                                    }
                                    if ui.button(tr("Cancelar")).clicked() {
                                        go = Some(Vec::new());
                                    }
                                });
                                if let Some(scopes) = go {
                                    self.reauth_pick = None;
                                    if !scopes.is_empty() {
                                        self.start_oauth(cx, Some(id.clone()), Some(scopes), redirect_port);
                                    }
                                }
                            }
                        });
                }

                ui.add_space(8.0);
                let can_connect = client_id_saved && secret_present;
                if !can_connect {
                    ui.label(
                    egui::RichText::new(tr(
                        "Salve o client_id e o client_secret acima para poder conectar uma conta.",
                    ))
                    .weak()
                    .small(),
                );
                }
                if !self.show_connect {
                    if ui
                        .add_enabled(
                            can_connect,
                            egui::Button::new(
                                egui::RichText::new(tr("Conectar conta"))
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(Tone::Info.color(true)),
                        )
                        .clicked()
                    {
                        self.show_connect = true;
                    }
                } else {
                    ui.label(
                        egui::RichText::new(tr(
                            "Escopos a solicitar (podem ser ampliados depois com Reautorizar):",
                        ))
                        .small(),
                    );
                    for (i, (_, label)) in SCOPE_OPTIONS.iter().enumerate() {
                        ui.checkbox(&mut self.connect_scopes[i], tr(label));
                    }
                    ui.horizontal(|ui| {
                        let any = self.connect_scopes.iter().any(|b| *b);
                        let connecting = self.flow.as_ref().is_some_and(|f| f.reauth_id.is_none());
                        if ui
                            .add_enabled(
                                can_connect && any && !connecting,
                                egui::Button::new(tr("Abrir navegador para autorizar")),
                            )
                            .clicked()
                        {
                            let scopes: Vec<String> = SCOPE_OPTIONS
                                .iter()
                                .zip(self.connect_scopes.iter())
                                .filter(|(_, on)| **on)
                                .map(|((k, _), _)| k.to_string())
                                .collect();
                            self.start_oauth(cx, None, Some(scopes), redirect_port);
                        }
                        if ui.button(tr("Cancelar")).clicked() {
                            self.show_connect = false;
                        }
                        if connecting {
                            ui.spinner();
                        }
                    });
                }

                if let Some(url) = self.auth_url.clone() {
                    ui.add_space(6.0);
                    ui.label(egui::RichText::new(tr("Autorize no navegador; a conta aparece aqui ao concluir. Se o navegador não abriu, use a URL abaixo:")).small());
                    ui::mono(ui, &url);
                    ui.horizontal(|ui| {
                        if ui.button(tr("copiar URL")).clicked() {
                            ui.ctx().copy_text(url.clone());
                            cx.toasts.info(tr("URL copiada."));
                        }
                        if ui.button(tr("abrir no navegador")).clicked() {
                            ui.ctx().open_url(egui::OpenUrl::new_tab(&url));
                        }
                        if ui.button(tr("fechar")).clicked() {
                            self.auth_url = None;
                        }
                    });
                }
            },
        );

        if let Some((id, email)) = self.confirm_disconnect.clone() {
            let c = ui::Confirm {
                title: tr("Desconectar conta Google").into(),
                body: trf("Desconectar {}? O refresh token é revogado e apagado do cofre; as agendas e ferramentas dessa conta deixam de funcionar até conectar de novo.", &[&email]),
                confirm_label: tr("Desconectar").into(),
                danger: true,
                tag: "google-disconnect".into(),
            };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_disconnect = None;
                if ok {
                    self.disconnect_task = Some(
                        cx.svc
                            .call(move |c| async move { c.google_disconnect(&id).await }),
                    );
                }
            }
        }
    }

    // ---------- 2. Agenda ----------

    fn calendar_section(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        ui::section_help(ui, tr("Agenda"), Some(&help::GOOGLE_CALENDAR), |ui| {
            let c = &mut st.draft.calendar;
            ui::field(
                ui,
                tr("Habilitar"),
                Some(tr("Ferramentas calendar_* para os agentes.")),
                |ui| {
                    ui::toggle(ui, &mut c.enabled);
                },
            );
            ui::field(ui, tr("Permitir criar/editar"), Some(tr("Exige o escopo Agenda (criar/editar). As ferramentas nascem com política confirmar.")), |ui| {
                ui::toggle(ui, &mut c.allow_write);
            });
            ui::field(
                ui,
                tr("Permitir excluir"),
                Some(tr("calendar_delete_event; política confirmar forçada.")),
                |ui| {
                    ui::toggle(ui, &mut c.allow_delete);
                },
            );
            ui::field(
                ui,
                tr("Enviar convites a participantes"),
                Some(tr("Ao criar/editar eventos com convidados.")),
                |ui| {
                    ui::toggle(ui, &mut c.send_updates);
                },
            );
            ui::field(
                ui,
                tr("Fuso padrão"),
                Some(tr("Vazio = automático. Ex.: America/Sao_Paulo")),
                |ui| {
                    ui::optional_text(ui, &mut c.timezone, tr("automático"));
                },
            );
            ui::field(ui, tr("Janela máxima por consulta (dias)"), None, |ui| {
                ui::number(ui, &mut c.max_window_days, 1..=366);
            });
            st.save_bar(ui, cx.svc, "calendar");

            ui.add_space(10.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tr("Agendas")).strong());
                let loading = self.calendars.loading();
                if ui
                    .add_enabled(!loading, egui::Button::new(tr("Atualizar agendas")))
                    .clicked()
                {
                    self.refresh_calendars(cx);
                }
                if loading || self.flags_task.is_some() {
                    ui.spinner();
                }
            });
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
                ui.label(egui::RichText::new(tr("Nenhuma agenda conhecida. Conecte uma conta com escopo de Agenda e clique em Atualizar agendas.")).weak());
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
                            let r = ui
                                .add_enabled(!busy, egui::Checkbox::new(&mut enabled, tr("usar")));
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
                                    egui::RadioButton::new(is_default, tr("padrão para criar")),
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
                                .on_hover_text(trf("{}\n(clique para copiar o id)", &[&cal_id]));
                            if r.clicked() {
                                ui.ctx().copy_text(cal_id.clone());
                                cx.toasts.success(tr("Id da agenda copiado."));
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
        ui::section_help(ui, tr("Gmail"), Some(&help::GOOGLE_GMAIL), |ui| {
            let g = &mut st.draft.gmail;
            ui::field(ui, tr("Habilitar"), Some(tr("Ferramentas gmail_* para os agentes. E-mails recebidos são dados, nunca gatilhos.")), |ui| {
                ui::toggle(ui, &mut g.enabled);
            });
            ui::field(
                ui,
                tr("Contas usadas"),
                Some(tr(
                    "Nenhuma marcada = todas as contas conectadas com escopo de Gmail.",
                )),
                |ui| account_multicheck(ui, &accounts, &mut g.accounts),
            );
            ui::field(
                ui,
                tr("Permitir envio"),
                Some(tr(
                    "gmail_send nasce com política confirmar; exige o escopo Gmail (enviar).",
                )),
                |ui| {
                    ui::toggle(ui, &mut g.allow_send);
                },
            );
            ui::field(ui, tr("Permitir modificar"), Some(tr("Arquivar, marcar lida, rotular (gmail_modify, política confirmar). Exige o escopo Gmail (arquivar/marcar/rotular).")), |ui| {
                ui::toggle(ui, &mut g.allow_modify);
            });
            ui::field(
                ui,
                tr("Só destinatários já vistos"),
                Some(tr(
                    "Envio apenas para endereços que já apareceram em e-mails lidos/enviados.",
                )),
                |ui| {
                    ui::toggle(ui, &mut g.only_known_recipients);
                },
            );
            ui::field(ui, tr("Rate limit de envio (por hora)"), None, |ui| {
                ui::number(ui, &mut g.send_rate_limit_per_hour, 1..=1000);
            });
            ui::field(
                ui,
                tr("Tamanho máx. de corpo (KB)"),
                Some(tr("Corpos maiores são truncados na leitura.")),
                |ui| {
                    ui::number(ui, &mut g.max_body_kb, 1..=4096);
                },
            );
            ui::field(
                ui,
                tr("Pasta de anexos"),
                Some(tr("Relativa à base de conhecimento.")),
                |ui| {
                    ui::text_input(ui, &mut g.attachments_dir, "inbox/gmail");
                },
            );
            ui::field(ui, tr("Limite de anexo (MB)"), None, |ui| {
                ui::number(ui, &mut g.max_attachment_mb, 1..=100);
            });
            st.save_bar(ui, cx.svc, "gmail");
        });
    }

    fn drive_section(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        let accounts = self
            .accounts
            .data
            .as_ref()
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        ui::section_help(ui, tr("Drive"), Some(&help::GOOGLE_DRIVE), |ui| {
            let d = &mut st.draft.drive;
            ui::field(ui, tr("Habilitar"), Some(tr("Ferramentas drive_* para os agentes. Exige conta com escopo Drive (ler) ou Drive (ler e escrever). Conteúdo de arquivos é dado, nunca instrução.")), |ui| {
                ui::toggle(ui, &mut d.enabled);
            });
            ui::field(
                ui,
                tr("Contas usadas"),
                Some(tr(
                    "Nenhuma marcada = todas as contas conectadas com escopo de Drive.",
                )),
                |ui| account_multicheck(ui, &accounts, &mut d.accounts),
            );
            ui::field(ui, tr("Permitir escrever"), Some(tr("Enviar, criar, renomear/mover, substituir conteúdo e lixeira (cada ação pede confirmação). Exige o escopo Drive (ler e escrever).")), |ui| {
                ui::toggle(ui, &mut d.allow_write);
            });
            ui::field(ui, tr("Permitir compartilhar"), Some(tr("drive_share/drive_unshare: dá acesso a terceiros (pessoa, domínio ou qualquer um com o link). Sempre com confirmação.")), |ui| {
                ui::toggle(ui, &mut d.allow_share);
            });
            ui::field(
                ui,
                tr("Permitir excluir definitivamente"),
                Some(tr(
                    "drive_trash com permanent: apaga sem passar pela lixeira.",
                )),
                |ui| {
                    ui::toggle(ui, &mut d.allow_delete);
                },
            );
            ui::field(
                ui,
                tr("Baixar arquivos para"),
                Some(tr("Relativa à base de conhecimento.")),
                |ui| {
                    ui::text_input(ui, &mut d.download_dir, "inbox/drive");
                },
            );
            ui::field(ui, tr("Limite de download (MB)"), None, |ui| {
                ui::number(ui, &mut d.max_download_mb, 1..=2048);
            });
            ui::field(
                ui,
                tr("Limite de upload (MB)"),
                Some(tr("Acima de 5 MB o envio é feito em blocos (resumable).")),
                |ui| {
                    ui::number(ui, &mut d.max_upload_mb, 1..=2048);
                },
            );
            ui::field(ui, tr("Exportar Docs como"), None, |ui| {
                ui::select(ui, "drive-export-docs", &mut d.export_docs, &DOCS_EXPORTS);
            });
            ui::field(
                ui,
                tr("Exportar Planilhas como"),
                Some(tr("CSV exporta só a primeira aba; XLSX, todas.")),
                |ui| {
                    ui::select(
                        ui,
                        "drive-export-sheets",
                        &mut d.export_sheets,
                        &SHEETS_EXPORTS,
                    );
                },
            );
            ui::field(ui, tr("Exportar Apresentações como"), None, |ui| {
                ui::select(
                    ui,
                    "drive-export-slides",
                    &mut d.export_slides,
                    &SLIDES_EXPORTS,
                );
            });
            st.save_bar(ui, cx.svc, "drive");
        });
    }
}

/// Formatos de exportação dos arquivos nativos do Google (MIME → rótulo).
const DOCS_EXPORTS: [(&str, &str); 5] = [
    ("text/markdown", "Markdown (.md)"),
    ("application/pdf", "PDF"),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "Word (.docx)",
    ),
    ("text/plain", "Texto (.txt)"),
    ("text/html", "HTML"),
];
const SHEETS_EXPORTS: [(&str, &str); 3] = [
    ("text/csv", "CSV (1ª aba)"),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "Excel (.xlsx)",
    ),
    ("application/pdf", "PDF"),
];
const SLIDES_EXPORTS: [(&str, &str); 2] = [
    ("application/pdf", "PDF"),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "PowerPoint (.pptx)",
    ),
];

/// Multi-check das contas conectadas (por id/e-mail) com remoção das que não
/// existem mais. Compartilhado por Gmail e Drive.
fn account_multicheck(ui: &mut egui::Ui, accounts: &[Value], selected: &mut Vec<String>) {
    if accounts.is_empty() {
        ui.label(
            egui::RichText::new(tr("Nenhuma conta conectada."))
                .weak()
                .small(),
        );
    }
    for a in accounts {
        let id = ui::s(a, "id");
        let email = ui::s(a, "email");
        let mut on = selected.iter().any(|x| x == &id || x == &email);
        if ui.checkbox(&mut on, &email).changed() {
            selected.retain(|x| x != &id && x != &email);
            if on {
                selected.push(id.clone());
            }
        }
    }
    let known: Vec<String> = accounts
        .iter()
        .flat_map(|a| [ui::s(a, "id"), ui::s(a, "email")])
        .collect();
    let unknown: Vec<String> = selected
        .iter()
        .filter(|x| !known.contains(x))
        .cloned()
        .collect();
    for x in unknown {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(trf("{} (conta não conectada)", &[&x]))
                    .weak()
                    .small(),
            );
            if ui.button(tr("remover")).clicked() {
                selected.retain(|y| y != &x);
            }
        });
    }
}

// ---------- helpers de apresentação ----------

fn account_status(status: &str) -> (&'static str, Tone) {
    match status {
        "ok" => (tr("ok"), Tone::Success),
        "reauthorize" => (tr("reautorizar"), Tone::Warning),
        "" => (tr("desconhecido"), Tone::Neutral),
        _ => (tr("erro"), Tone::Danger),
    }
}

/// Chaves lógicas (`SCOPE_OPTIONS`) que a conta já tem, a partir das URLs
/// concedidas. `drive` completo implica `drive` (ler); `calendar.events`
/// implica `calendar` (ler) etc.
fn granted_logical(account: &Value) -> Vec<&'static str> {
    let Some(arr) = account.get("scopes").and_then(|s| s.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<&'static str> = Vec::new();
    let mut add = |k: &'static str| {
        if !out.contains(&k) {
            out.push(k);
        }
    };
    for sc in arr.iter().filter_map(|s| s.as_str()) {
        match sc {
            s if s.ends_with("calendar.readonly") || s == "calendar" || s == "calendar_read" => {
                add("calendar")
            }
            s if s.ends_with("calendar.events")
                || s.ends_with("/auth/calendar")
                || s == "calendar_write" =>
            {
                add("calendar");
                add("calendar_write");
            }
            s if s.ends_with("gmail.readonly") || s == "gmail" || s == "gmail_read" => add("gmail"),
            s if s.ends_with("gmail.send") || s == "gmail_send" => {
                add("gmail");
                add("gmail_send");
            }
            s if s.ends_with("gmail.modify") || s == "gmail_modify" => {
                add("gmail");
                add("gmail_modify");
            }
            s if s.ends_with("drive.readonly") || s == "drive" || s == "drive_read" => add("drive"),
            s if s.ends_with("/auth/drive") || s == "drive_write" => {
                add("drive");
                add("drive_write");
            }
            _ => {}
        }
    }
    out
}

/// (produto, capacidade) de uma chave lógica: `gmail_send` → ("Gmail", "enviar").
fn scope_split(key: &str) -> (&'static str, &'static str) {
    match key {
        "calendar" => (tr("Agenda"), tr("ler")),
        "calendar_write" => (tr("Agenda"), tr("criar/editar")),
        "gmail" => (tr("Gmail"), tr("ler")),
        "gmail_send" => (tr("Gmail"), tr("enviar")),
        "gmail_modify" => (tr("Gmail"), tr("arquivar/marcar/rotular")),
        "drive" => (tr("Drive"), tr("ler")),
        "drive_write" => (tr("Drive"), tr("ler e escrever")),
        _ => ("?", ""),
    }
}

/// Escopos concedidos agrupados por produto, na ordem Agenda · Gmail · Drive:
/// ("Gmail", "ler · enviar"). `drive_write` já implica ler, então vira só
/// "ler e escrever".
fn scope_groups(account: &Value) -> Vec<(&'static str, String)> {
    let g = granted_logical(account);
    let mut out: Vec<(&'static str, String)> = Vec::new();
    for keys in [
        &["calendar", "calendar_write"][..],
        &["gmail", "gmail_send", "gmail_modify"][..],
        &["drive", "drive_write"][..],
    ] {
        let mut caps: Vec<&'static str> = Vec::new();
        let mut head = "";
        for k in keys {
            if g.contains(k) {
                let (h, c) = scope_split(k);
                head = h;
                caps.push(c);
            }
        }
        if *keys == ["drive", "drive_write"] && caps.len() == 2 {
            caps.remove(0);
        }
        if !caps.is_empty() {
            out.push((head, caps.join(" · ")));
        }
    }
    out
}

/// Escopos que os módulos **ligados** nas configurações exigem e a conta ainda
/// não concedeu — é o que o "Reautorizar / ampliar escopos" sugere.
fn missing_scopes(account: &Value, settings: &will_core::settings::Settings) -> Vec<&'static str> {
    let granted = granted_logical(account);
    let mut wanted: Vec<&'static str> = Vec::new();
    if settings.calendar.enabled {
        wanted.push(if settings.calendar.allow_write {
            "calendar_write"
        } else {
            "calendar"
        });
    }
    if settings.gmail.enabled {
        wanted.push("gmail");
        if settings.gmail.allow_send {
            wanted.push("gmail_send");
        }
        if settings.gmail.allow_modify {
            wanted.push("gmail_modify");
        }
    }
    if settings.drive.enabled {
        wanted.push(if settings.drive.allow_write {
            "drive_write"
        } else {
            "drive"
        });
    }
    wanted.retain(|w| !granted.contains(w));
    wanted
}

fn role_label(role: &str) -> String {
    match role {
        "owner" => tr("dona").into(),
        "writer" => tr("escrita").into(),
        "reader" => tr("leitura").into(),
        "freeBusyReader" => tr("livre/ocupado").into(),
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
    fn scope_groups_by_product() {
        let a = serde_json::json!({"scopes": ["https://www.googleapis.com/auth/gmail.send", "https://www.googleapis.com/auth/calendar.readonly", "https://www.googleapis.com/auth/drive"]});
        let g = scope_groups(&a);
        assert_eq!(g[0].0, "Agenda");
        assert_eq!(g[0].1, "ler");
        assert_eq!(g[1].0, "Gmail");
        assert_eq!(g[1].1, "ler · enviar");
        assert_eq!(g[2].0, "Drive");
        assert_eq!(g[2].1, "ler e escrever");
        assert_eq!(scope_split("gmail_modify").1, "arquivar/marcar/rotular");
    }

    #[test]
    fn granted_and_missing_scopes() {
        let a = serde_json::json!({"scopes": ["openid", "https://www.googleapis.com/auth/gmail.modify", "https://www.googleapis.com/auth/calendar.readonly"]});
        let g = granted_logical(&a);
        assert_eq!(g, vec!["gmail", "gmail_modify", "calendar"]);
        let mut st = will_core::settings::Settings::fresh();
        st.calendar.enabled = true;
        st.calendar.allow_write = true;
        st.gmail.enabled = true;
        st.gmail.allow_send = true;
        st.gmail.allow_modify = true;
        st.drive.enabled = true;
        st.drive.allow_write = false;
        assert_eq!(
            missing_scopes(&a, &st),
            vec!["calendar_write", "gmail_send", "drive"]
        );
        st.drive.enabled = false;
        assert_eq!(
            missing_scopes(&a, &st),
            vec!["calendar_write", "gmail_send"]
        );
    }
}
