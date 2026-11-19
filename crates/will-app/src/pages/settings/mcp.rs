//! Seção Servidor MCP: status/URL do servidor local, configuração, políticas
//! por ferramenta (auto / auto para listados / confirmar / desligada),
//! destinatários confiáveis por canal e chamadas recentes.

use super::SettingsState;
use crate::help;
use crate::i18n::{tr, trf};
use crate::pages::Cx;
use crate::service::{Loader, Task};
use crate::ui::{self, Tone};
use serde_json::Value;
use std::collections::BTreeMap;
use will_core::{Event, Module, ToolPolicy};

#[derive(Default)]
pub struct McpPage {
    tools: Loader<Value>,
    calls: Loader<Value>,
    policy_task: Option<(String, Task<Value>)>,
    /// Token fixo para scripts (gerado e guardado no cofre pelo serviço).
    script_token: Loader<Value>,
    reveal_token: bool,
    rotate_task: Option<Task<Value>>,
    confirm_rotate: Option<ui::Confirm>,
    /// Destinatários confiáveis (`GET /api/mcp/trusted`) e a alteração em curso
    /// (a resposta já é a lista nova).
    trusted: Loader<Value>,
    trusted_task: Option<Task<Value>>,
    /// Campos de inclusão manual (um por canal).
    wa_input: String,
    email_input: String,
    /// Filtro digitado sobre cada lista (só aparece em lista grande).
    wa_filter: String,
    email_filter: String,
    /// Busca de contato/grupo do WhatsApp pelo nome (seletor).
    wa_query: String,
    wa_query_sent: String,
    wa_chats: Loader<Value>,
}

/// Chave de WhatsApp legível: grupo fica como está; telefone ganha máscara.
fn wa_key_label(key: &str) -> String {
    if will_core::trust::wa_key_is_group(key) {
        return key.to_string();
    }
    match key.strip_prefix("55") {
        // chave canônica brasileira: DDD + 8 dígitos (sem o nono)
        Some(rest) if rest.len() == 10 => {
            let (ddd, num) = rest.split_at(2);
            let (a, b) = num.split_at(4);
            let nine = if matches!(a.as_bytes()[0], b'6'..=b'9') {
                "9"
            } else {
                ""
            };
            format!("+55 ({ddd}) {nine}{a}-{b}")
        }
        _ => format!("+{key}"),
    }
}

fn policy_label(p: &str) -> &'static str {
    match p {
        "auto" => tr("automático"),
        "auto_listed" => tr("automático p/ listados"),
        "confirm" => tr("confirmar"),
        "off" => tr("desligada"),
        _ => tr("padrão"),
    }
}

fn family_label(f: &str) -> String {
    match f {
        "telegram" => tr("Telegram").into(),
        "whatsapp" => tr("WhatsApp").into(),
        "calendar" => tr("Agenda").into(),
        "gmail" => tr("Gmail").into(),
        "core" => tr("Sistema").into(),
        "recordings" => tr("Gravações").into(),
        other => other.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
}

impl McpPage {
    pub fn on_event(&mut self, ev: &Event) {
        match ev {
            Event::McpToolCalled { .. } => self.calls.loaded_at = None,
            Event::SettingsChanged { section } if section == "mcp" => {
                self.tools.loaded_at = None;
                self.trusted.loaded_at = None;
                self.script_token.loaded_at = None;
            }
            Event::ModuleStatus { module, .. } if *module == Module::Mcp => {
                self.script_token.loaded_at = None;
            }
            _ => {}
        }
    }

    fn reload_tools(&mut self, cx: &Cx) {
        self.tools
            .start(cx.svc.call(|c| async move { c.mcp_tools().await }));
    }

    fn reload_calls(&mut self, cx: &Cx) {
        self.calls.start(
            cx.svc
                .call(|c| async move { c.mcp_calls(&[("limit", "100".to_string())]).await }),
        );
    }

    fn reload_script_token(&mut self, cx: &Cx) {
        self.script_token
            .start(cx.svc.call(|c| async move { c.mcp_script_token().await }));
    }

    fn poll(&mut self, cx: &mut Cx, st: &mut SettingsState) {
        if self.tools.stale(std::time::Duration::from_secs(120)) {
            self.reload_tools(cx);
        }
        if self.script_token.stale(std::time::Duration::from_secs(300)) {
            self.reload_script_token(cx);
        }
        self.script_token.poll();
        if let Some(t) = &self.rotate_task {
            if let Some(r) = t.take() {
                self.rotate_task = None;
                match r {
                    Ok(v) => {
                        cx.toasts
                            .success(tr("Token regenerado. O anterior deixou de valer."));
                        self.script_token.data = Some(v);
                        self.reveal_token = false;
                    }
                    Err(e) => cx.toasts.error(trf("Regenerar token: {}", &[&e])),
                }
            }
        }
        if self.calls.stale(std::time::Duration::from_secs(30)) {
            self.reload_calls(cx);
        }
        self.tools.poll();
        self.calls.poll();
        if self.trusted.stale(std::time::Duration::from_secs(20)) {
            self.trusted
                .start(cx.svc.call(|c| async move { c.mcp_trusted().await }));
        }
        self.trusted.poll();
        if let Some(t) = &self.trusted_task {
            if let Some(r) = t.take() {
                self.trusted_task = None;
                match r {
                    Ok(v) => {
                        self.trusted.data = Some(v);
                        self.trusted.error = None;
                    }
                    Err(e) => cx.toasts.error(trf("Destinatários confiáveis: {}", &[&e])),
                }
            }
        }
        self.wa_chats.poll();
        if let Some((name, t)) = &self.policy_task {
            if let Some(r) = t.take() {
                let name = name.clone();
                self.policy_task = None;
                match r {
                    Ok(_) => {
                        cx.toasts
                            .success(trf("Política de '{}' atualizada.", &[&name]));
                        // O handler grava direto nas settings; realinha rascunho e salvo.
                        st.load(cx.svc);
                        self.reload_tools(cx);
                    }
                    Err(e) => cx.toasts.error(trf("Política de '{}': {}", &[&name, &e])),
                }
            }
        }
    }

    fn set_policy(&mut self, cx: &Cx, st: &mut SettingsState, name: &str, value: &str) {
        // Espelha no rascunho para a barra de salvar não reverter a mudança.
        match ToolPolicy::parse(value) {
            Some(p) => {
                st.draft.mcp.policies.insert(name.to_string(), p);
            }
            None => {
                st.draft.mcp.policies.remove(name);
            }
        }
        let n = name.to_string();
        let policy = value.to_string();
        let t = cx
            .svc
            .call(move |c| async move { c.mcp_set_policy(&n, &policy).await });
        self.policy_task = Some((name.to_string(), t));
    }

    fn trusted_add(&mut self, cx: &Cx, channel: &'static str, value: String, label: String) {
        self.trusted_task = Some(
            cx.svc
                .call(move |c| async move { c.mcp_trusted_add(channel, &value, &label).await }),
        );
    }

    fn trusted_remove(&mut self, cx: &Cx, channel: &'static str, key: String) {
        self.trusted_task = Some(
            cx.svc
                .call(move |c| async move { c.mcp_trusted_remove(channel, &key).await }),
        );
    }

    /// Entradas de um canal: `(chave, rótulo)`.
    fn trusted_entries(&self, channel: &str) -> Vec<(String, String)> {
        self.trusted
            .data
            .as_ref()
            .and_then(|v| v.get(channel))
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .map(|e| (ui::s(e, "key"), ui::s(e, "label")))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Lista de um canal numa tabela com rolagem: cabeçalho fixo, filtro
    /// (quando a lista é grande), ordem alfabética e "Remover" em cada linha.
    /// `headers` = títulos das duas primeiras colunas.
    fn trusted_rows(
        &mut self,
        ui: &mut egui::Ui,
        cx: &Cx,
        channel: &'static str,
        headers: [&str; 2],
        show: impl Fn(&str, &str) -> (String, String),
    ) {
        let entries = self.trusted_entries(channel);
        if entries.is_empty() {
            ui.label(
                egui::RichText::new(tr("Ninguém na lista: tudo neste canal pede confirmação."))
                    .weak()
                    .small(),
            );
            return;
        }
        // (texto principal, detalhe, chave), em ordem alfabética
        let mut rows: Vec<(String, String, String)> = entries
            .iter()
            .map(|(key, label)| {
                let (main, detail) = show(key, label);
                (main, detail, key.clone())
            })
            .collect();
        rows.sort_by_key(|(main, _, key)| (main.to_lowercase(), key.clone()));
        let total = rows.len();
        let filter = match channel {
            "whatsapp" => &mut self.wa_filter,
            _ => &mut self.email_filter,
        };
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(trf("{} na lista", &[&total]))
                    .small()
                    .weak(),
            );
            // filtro só quando a lista já não cabe numa olhada
            if total > 6 {
                ui.add(
                    egui::TextEdit::singleline(filter)
                        .hint_text(tr("filtrar…"))
                        .desired_width(200.0),
                );
            } else {
                filter.clear();
            }
        });
        let needle = filter.trim().to_lowercase();
        if !needle.is_empty() {
            rows.retain(|(main, detail, key)| {
                main.to_lowercase().contains(&needle)
                    || detail.to_lowercase().contains(&needle)
                    || key.contains(&needle)
            });
        }
        if rows.is_empty() {
            ui.label(egui::RichText::new(tr("nada encontrado")).weak().small());
            return;
        }
        let busy = self.trusted_task.is_some();
        let mut remove: Option<String> = None;
        egui::Frame::new()
            .stroke(egui::Stroke::new(
                1.0,
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            ))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                egui_extras::TableBuilder::new(ui)
                    .id_salt(("mcp-trusted", channel))
                    .striped(true)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(egui_extras::Column::remainder().at_least(140.0).clip(true))
                    .column(egui_extras::Column::exact(230.0).clip(true))
                    .column(egui_extras::Column::exact(90.0))
                    .min_scrolled_height(0.0)
                    .max_scroll_height(260.0)
                    .header(24.0, |mut h| {
                        for title in headers {
                            h.col(|ui| {
                                ui.label(egui::RichText::new(title).small().weak());
                            });
                        }
                        h.col(|_| {});
                    })
                    .body(|body| {
                        body.rows(30.0, rows.len(), |mut row| {
                            let (main, detail, key) = &rows[row.index()];
                            row.col(|ui| {
                                ui.add(egui::Label::new(main).truncate())
                                    .on_hover_text(main);
                            });
                            row.col(|ui| {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(detail).weak().small())
                                        .truncate(),
                                )
                                .on_hover_text(detail);
                            });
                            row.col(|ui| {
                                if ui
                                    .add_enabled(!busy, egui::Button::new(tr("Remover")).small())
                                    .clicked()
                                {
                                    remove = Some(key.clone());
                                }
                            });
                        });
                    });
            });
        if let Some(key) = remove {
            self.trusted_remove(cx, channel, key);
        }
    }

    /// Aba "Destinatários confiáveis": uma lista por canal.
    fn trusted_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        if let Some(e) = &self.trusted.error {
            ui::error_text(ui, e);
        }
        let busy = self.trusted_task.is_some();
        ui.label(
            egui::RichText::new(tr(
                "Ferramentas com a política \"automático p/ listados\" executam sem pedir confirmação só quando todos os destinatários da chamada estão aqui. Qualquer outro caso pede confirmação.",
            ))
            .weak()
            .small(),
        );
        ui.add_space(4.0);

        // ---- WhatsApp ----
        ui::section(ui, tr("WhatsApp"), |ui| {
            self.trusted_rows(
                ui,
                cx,
                "whatsapp",
                [tr("Nome"), tr("Número")],
                |key, label| {
                    let shown = wa_key_label(key);
                    let group = will_core::trust::wa_key_is_group(key);
                    match (label.trim().is_empty(), group) {
                        (true, true) => (tr("grupo").to_string(), shown),
                        (true, false) => (shown, String::new()),
                        (false, true) => (label.to_string(), trf("grupo · {}", &[&shown])),
                        (false, false) => (label.to_string(), shown),
                    }
                },
            );
            ui.add_space(8.0);
            ui::field(
                ui,
                tr("Adicionar pelo nome"),
                Some(tr("Busca nos contatos e grupos do WhatsApp pareado. Grupo é item próprio: listar uma pessoa não libera os grupos dela.")),
                |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.wa_query)
                            .hint_text(tr("nome do contato ou do grupo…"))
                            .desired_width(320.0),
                    );
                    let q = self.wa_query.trim().to_string();
                    if q.chars().count() < 2 {
                        return;
                    }
                    if q != self.wa_query_sent && !self.wa_chats.busy() {
                        self.wa_query_sent = q.clone();
                        self.wa_chats
                            .start(cx.svc.call(move |c| async move { c.wa_chats(&q).await }));
                    }
                    if self.wa_chats.loading() {
                        ui.spinner();
                    }
                    // (nome, é grupo, valor a enviar, chave canônica). A mesma
                    // pessoa pode vir mais de uma vez (conversa e contato, JID
                    // de telefone e LID): fica uma linha por chave.
                    let mut found: Vec<(String, bool, String, Option<String>)> = Vec::new();
                    for c in self
                        .wa_chats
                        .data
                        .as_ref()
                        .and_then(|v| v.get("chats"))
                        .and_then(|a| a.as_array())
                        .into_iter()
                        .flatten()
                    {
                        let name = ui::s(c, "name");
                        let group = ui::b(c, "is_group");
                        // grupo vale pelo JID; pessoa, pelo telefone
                        let phone = ui::s(c, "phone");
                        let value = if group || phone.is_empty() {
                            ui::s(c, "jid")
                        } else {
                            phone
                        };
                        let key = will_core::trust::wa_key(&value);
                        let repeated = found.iter().any(|(n, _, v, k)| match (&key, k) {
                            (Some(a), Some(b)) => a == b,
                            // sem telefone: some se já há alguém com o mesmo nome
                            (None, _) => n == &name || v == &value,
                            (Some(_), None) => false,
                        });
                        if repeated {
                            continue;
                        }
                        // chegou o telefone de quem estava sem: troca a linha
                        if key.is_some() {
                            found.retain(|(n, _, _, k)| k.is_some() || n != &name);
                        }
                        found.push((name, group, value, key));
                    }
                    if found.is_empty() && !self.wa_chats.busy() {
                        ui.label(egui::RichText::new(tr("nada encontrado")).weak().small());
                        return;
                    }
                    let listed: Vec<String> = self
                        .trusted_entries("whatsapp")
                        .into_iter()
                        .map(|(k, _)| k)
                        .collect();
                    let mut add: Option<(String, String)> = None;
                    egui::ScrollArea::vertical()
                        .id_salt("mcp-trusted-wa-found")
                        .max_height(180.0)
                        .show(ui, |ui| {
                            egui::Grid::new("mcp-trusted-wa-found-grid")
                                .num_columns(3)
                                .striped(true)
                                .spacing([14.0, 4.0])
                                .show(ui, |ui| {
                                    for (name, group, value, key) in found {
                                        ui.label(if name.is_empty() { value.clone() } else { name.clone() });
                                        ui.label(
                                            egui::RichText::new(match (&key, group) {
                                                (Some(_), true) => tr("grupo").to_string(),
                                                (Some(k), false) => wa_key_label(k),
                                                (None, _) => tr("sem telefone conhecido").to_string(),
                                            })
                                            .weak()
                                            .small(),
                                        );
                                        match key {
                                            Some(k) if listed.contains(&k) => {
                                                ui.label(egui::RichText::new(tr("já na lista")).weak().small());
                                            }
                                            Some(_) => {
                                                if ui
                                                    .add_enabled(!busy, egui::Button::new(tr("Adicionar")).small())
                                                    .clicked()
                                                {
                                                    add = Some((value, name));
                                                }
                                            }
                                            None => {
                                                ui.label("");
                                            }
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                    if let Some((value, label)) = add {
                        self.trusted_add(cx, "whatsapp", value, label);
                    }
                },
            );
            ui::field(
                ui,
                tr("Adicionar pelo número"),
                Some(tr(
                    "Telefone com DDI (ex.: 5511987654321) ou o JID de um grupo (…@g.us).",
                )),
                |ui| {
                    ui.horizontal(|ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.wa_input)
                                .hint_text("5511987654321")
                                .desired_width(240.0),
                        );
                        let ok = will_core::trust::wa_key(&self.wa_input).is_some();
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let click = ui
                            .add_enabled(ok && !busy, egui::Button::new(tr("Adicionar")))
                            .clicked();
                        if ok && !busy && (enter || click) {
                            let value = std::mem::take(&mut self.wa_input);
                            self.trusted_add(cx, "whatsapp", value, String::new());
                        }
                    });
                },
            );
        });

        // ---- e-mail ----
        ui::section(ui, tr("E-mail"), |ui| {
            self.trusted_rows(
                ui,
                cx,
                "email",
                [tr("Endereço ou domínio"), tr("Alcance")],
                |key, _| {
                    let detail = if will_core::trust::email_key_is_domain(key) {
                        tr("todos os endereços deste domínio").to_string()
                    } else {
                        tr("só este endereço").to_string()
                    };
                    (key.to_string(), detail)
                },
            );
            ui.add_space(8.0);
            ui::field(
                ui,
                tr("Adicionar"),
                Some(tr("Um endereço (fulano@empresa.com) ou um domínio inteiro (@empresa.com). Num e-mail com vários destinatários, todos precisam estar liberados.")),
                |ui| {
                    ui.horizontal(|ui| {
                        let r = ui.add(
                            egui::TextEdit::singleline(&mut self.email_input)
                                .hint_text("fulano@empresa.com")
                                .desired_width(280.0),
                        );
                        let ok = will_core::trust::email_key(&self.email_input).is_some();
                        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        let click = ui
                            .add_enabled(ok && !busy, egui::Button::new(tr("Adicionar")))
                            .clicked();
                        if ok && !busy && (enter || click) {
                            let value = std::mem::take(&mut self.email_input);
                            self.trusted_add(cx, "email", value, String::new());
                        }
                    });
                },
            );
        });
    }

    /// Token fixo para chamar o MCP de fora das threads (scripts, cron, outros
    /// programas). Fica no cofre de segredos e é reaplicado a cada subida.
    fn script_token_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        ui::field(
            ui,
            tr("Token para scripts"),
            Some(tr(
                "Fixo entre reinicializações, para scripts e chamadas HTTP de fora das threads.",
            )),
            |ui| {
                if let Some(e) = &self.script_token.error {
                    ui::error_text(ui, e);
                    return;
                }
                let Some(v) = &self.script_token.data else {
                    ui::busy(ui, tr("carregando…"));
                    return;
                };
                let token = ui::s(v, "token");
                let masked = ui::s(v, "masked");
                let url = ui::s(v, "url");
                ui.horizontal_wrapped(|ui| {
                    if self.reveal_token {
                        ui::mono(ui, &token);
                    } else {
                        ui::mono(ui, &format!("••••••••••••{masked}"));
                    }
                    if ui
                        .button(if self.reveal_token { tr("Ocultar") } else { tr("Mostrar") })
                        .clicked()
                    {
                        self.reveal_token = !self.reveal_token;
                    }
                    if ui.button(tr("Copiar")).clicked() {
                        ui.ctx().copy_text(token.clone());
                        cx.toasts.success(tr("Token copiado."));
                    }
                    if ui
                        .button(tr("Copiar como export"))
                        .on_hover_text(tr("WILL_IA_MCP_URL e WILL_IA_MCP_TOKEN prontos para colar num shell"))
                        .clicked()
                    {
                        ui.ctx().copy_text(format!(
                            "export WILL_IA_MCP_URL='{url}' WILL_IA_MCP_TOKEN='{token}'"
                        ));
                        cx.toasts.success(tr("Variáveis copiadas."));
                    }
                    let busy = self.rotate_task.is_some();
                    if ui
                        .add_enabled(!busy, egui::Button::new(tr("Regenerar")))
                        .clicked()
                    {
                        self.confirm_rotate = Some(ui::Confirm {
                            title: tr("Regenerar token para scripts?").into(),
                            body: tr("O token atual deixa de valer imediatamente. Scripts que o usam precisarão do novo valor.").into(),
                            confirm_label: tr("Regenerar").into(),
                            danger: true,
                            tag: "mcp-rotate".into(),
                        });
                    }
                    if busy {
                        ui.spinner();
                    }
                });
                if !ui::b(v, "active") {
                    ui.label(
                        egui::RichText::new(tr(
                            "O servidor MCP está desligado: o token só vale com ele no ar.",
                        ))
                        .small()
                        .weak(),
                    );
                }
            },
        );
        if let Some(c) = &self.confirm_rotate {
            match ui::confirm_modal(ui.ctx(), c) {
                Some(true) => {
                    self.confirm_rotate = None;
                    self.rotate_task = Some(
                        cx.svc
                            .call(|c| async move { c.mcp_rotate_script_token().await }),
                    );
                }
                Some(false) => self.confirm_rotate = None,
                None => {}
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        self.poll(cx, st);
        ui::heading(ui, tr("Servidor MCP"), Some(tr("Servidor local (Streamable HTTP em 127.0.0.1) que expõe as ferramentas do will-ia aos agentes. Bearer efêmero por processo; cada chamada sabe de qual thread veio.")));

        let tab = ui::tabs(
            ui,
            "mcp",
            &[
                ui::Tab::new("server", tr("Servidor")),
                ui::Tab::new("tools", tr("Ferramentas e políticas")),
                ui::Tab::new("trusted", tr("Destinatários confiáveis")),
                ui::Tab::new("calls", tr("Chamadas recentes")),
            ],
        );

        // ---- servidor ----
        if tab == "server" {
            ui::section_help(ui, tr("Servidor"), Some(&help::MCP_SERVER), |ui| {
                let status = cx
                    .snap
                    .modules
                    .get(&Module::Mcp)
                    .cloned()
                    .unwrap_or(will_core::ModuleStatus::Stopped);
                ui.horizontal(|ui| {
                    ui.label(tr("Módulo:"));
                    ui::badge(ui, &ui::module_label(&status), ui::tone_for_module(&status));
                });
                // URL do servidor MCP vem com o token de scripts (`GET /api/mcp/script-token`).
                let url = self
                    .script_token
                    .data
                    .as_ref()
                    .map(|v| ui::s(v, "url"))
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(tr("URL:"));
                    if url.is_empty() {
                        ui.label(egui::RichText::new(tr("(servidor não está no ar)")).weak());
                    } else {
                        ui::mono(ui, &url);
                    }
                });
                ui.add_space(6.0);
                self.script_token_ui(ui, cx);
                ui.add_space(6.0);
                let m = &mut st.draft.mcp;
                ui::field(
                    ui,
                    tr("Habilitar"),
                    Some(tr(
                        "Desligado, os agentes ficam sem as ferramentas will_ia.",
                    )),
                    |ui| {
                        ui::toggle(ui, &mut m.enabled);
                    },
                );
                ui::field(ui, tr("Porta"), Some(tr("0 = aleatória a cada execução (recomendado); o .mcp.json da KB é regravado com a URL atual.")), |ui| {
                ui::number(ui, &mut m.port, 0..=65535);
            });
                ui::field(
                    ui,
                    tr("Timeout de aprovação (s)"),
                    Some(tr(
                        "Pedido de confirmação sem resposta é negado após esse tempo, e a tela de aprovação some.",
                    )),
                    |ui| {
                        ui::number(ui, &mut m.approval_timeout_secs, 10..=3600);
                    },
                );
                ui::field(
                    ui,
                    tr("Bloqueio de envio duplicado (s)"),
                    Some(tr(
                        "Mensagem ou e-mail idêntico a outro enviado dentro desta janela não é enviado de novo: o agente recebe o resultado do primeiro. 0 desliga.",
                    )),
                    |ui| {
                        ui::number(ui, &mut m.duplicate_window_secs, 0..=86400);
                    },
                );
                st.save_bar(ui, cx.svc, "mcp");
            });
        }
        // ---- ferramentas ----
        if tab == "tools" {
            ui::section_help(
                ui,
                tr("Ferramentas e políticas"),
                Some(&help::MCP_TOOLS),
                |ui| {
                    ui.horizontal(|ui| {
                        if ui.button(tr("Atualizar")).clicked() {
                            self.tools.loaded_at = None;
                        }
                        if self.tools.loading() {
                            ui.spinner();
                        }
                        if let Some((n, _)) = &self.policy_task {
                            ui.label(
                                egui::RichText::new(trf("aplicando política de {}…", &[&n]))
                                    .weak()
                                    .small(),
                            );
                        }
                    });
                    if let Some(e) = &self.tools.error {
                        ui::error_text(ui, e);
                    }
                    let list = self
                        .tools
                        .data
                        .as_ref()
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    if list.is_empty() && !self.tools.loading() {
                        ui.label(egui::RichText::new(tr("Nenhuma ferramenta registrada.")).weak());
                    }
                    let mut by_family: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
                    for t in &list {
                        by_family.entry(ui::s(t, "family")).or_default().push(t);
                    }
                    let mut change: Option<(String, String)> = None;
                    for (family, tools) in &by_family {
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(family_label(family)).strong());
                        egui::Grid::new(format!("mcp-tools-{family}"))
                            .num_columns(4)
                            .striped(true)
                            .spacing([14.0, 6.0])
                            .show(ui, |ui| {
                                for t in tools {
                                    let name = ui::s(t, "name");
                                    let available = ui::b(t, "available");
                                    let default_policy = ui::s(t, "default_policy");
                                    let overridden = ui::b(t, "overridden");
                                    let policy = ui::s(t, "policy");
                                    let desc = ui::s(t, "description");
                                    let default_label =
                                        trf("padrão ({})", &[&policy_label(&default_policy)]);
                                    // "p/ listados" só em ferramenta com destinatário
                                    let has_recipient =
                                        t.get("trust_channel").is_some_and(|c| c.is_string());
                                    let mut opts: Vec<(&str, &str)> = vec![
                                        ("default", default_label.as_str()),
                                        ("auto", tr("automático")),
                                    ];
                                    if has_recipient {
                                        opts.push(("auto_listed", tr("automático p/ listados")));
                                    }
                                    opts.push(("confirm", tr("confirmar")));
                                    opts.push(("off", tr("desligada")));
                                    let mut v = if overridden {
                                        policy.clone()
                                    } else {
                                        "default".to_string()
                                    };
                                    let busy = self.policy_task.is_some();
                                    ui.monospace(&name);
                                    if available {
                                        ui::badge(ui, tr("disponível"), Tone::Success);
                                    } else {
                                        ui::badge(ui, tr("indisponível"), Tone::Neutral);
                                    }
                                    ui.add_enabled_ui(!busy, |ui| {
                                        if ui::select(ui, &format!("pol-{name}"), &mut v, &opts) {
                                            change = Some((name.clone(), v.clone()));
                                        }
                                    });
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(truncate(&desc, 160))
                                                .weak()
                                                .small(),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(&desc);
                                    ui.end_row();
                                }
                            });
                    }
                    if let Some((name, v)) = change {
                        self.set_policy(cx, st, &name, &v);
                    }
                },
            );
        }
        // ---- destinatários confiáveis ----
        if tab == "trusted" {
            self.trusted_ui(ui, cx);
        }
        // ---- chamadas ----
        if tab == "calls" {
            ui::section(ui, tr("Chamadas recentes"), |ui| {
                ui.horizontal(|ui| {
                    if ui.button(tr("Atualizar")).clicked() {
                        self.calls.loaded_at = None;
                    }
                    if self.calls.loading() {
                        ui.spinner();
                    }
                });
                if let Some(e) = &self.calls.error {
                    ui::error_text(ui, e);
                }
                let calls = self
                    .calls
                    .data
                    .as_ref()
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if calls.is_empty() && !self.calls.loading() {
                    ui.label(egui::RichText::new(tr("Nenhuma chamada registrada ainda.")).weak());
                    return;
                }
                egui::ScrollArea::vertical()
                    .id_salt("mcp-calls")
                    .max_height(360.0)
                    .show(ui, |ui| {
                        egui::Grid::new("mcp-calls-grid")
                            .num_columns(7)
                            .striped(true)
                            .spacing([14.0, 4.0])
                            .show(ui, |ui| {
                                for h in [
                                    tr("Hora"),
                                    tr("Ferramenta"),
                                    tr("Thread"),
                                    tr("Liberação"),
                                    tr("Resultado"),
                                    tr("Duração"),
                                    tr("Erro"),
                                ] {
                                    ui.label(egui::RichText::new(h).weak().small());
                                }
                                ui.end_row();
                                for c in &calls {
                                    ui.label(egui::RichText::new(ui::fmt_dt(&c["at"])).small());
                                    ui.monospace(ui::s(c, "tool"));
                                    let tid = ui::s(c, "thread_id");
                                    ui.monospace(if tid.is_empty() {
                                        "—".to_string()
                                    } else {
                                        tid.chars().take(8).collect()
                                    });
                                    // como a chamada foi liberada (registros antigos: vazio)
                                    match ui::s(c, "policy").as_str() {
                                        "auto_listed" => {
                                            ui::badge(ui, tr("automático (listado)"), Tone::Info);
                                        }
                                        "auto" => {
                                            ui.label(
                                                egui::RichText::new(tr("automático"))
                                                    .small()
                                                    .weak(),
                                            );
                                        }
                                        "confirm" => {
                                            ui.label(
                                                egui::RichText::new(tr("confirmação"))
                                                    .small()
                                                    .weak(),
                                            );
                                        }
                                        "off" => {
                                            ui.label(
                                                egui::RichText::new(tr("desligada")).small().weak(),
                                            );
                                        }
                                        "duplicate" => {
                                            ui::badge(ui, tr("duplicada"), Tone::Warning);
                                        }
                                        _ => {
                                            ui.label(egui::RichText::new("—").small().weak());
                                        }
                                    }
                                    if ui::b(c, "ok") {
                                        ui::badge(ui, tr("ok"), Tone::Success);
                                    } else {
                                        ui::badge(ui, tr("erro"), Tone::Danger);
                                    }
                                    let ms = c
                                        .get("duration_ms")
                                        .and_then(|d| d.as_i64())
                                        .unwrap_or(0)
                                        .max(0) as u64;
                                    ui.label(egui::RichText::new(ui::fmt_ms(ms)).small());
                                    let err = ui::s(c, "error");
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(truncate(&err, 60))
                                                .small()
                                                .color(Tone::Danger.color(ui.visuals().dark_mode)),
                                        )
                                        .wrap(),
                                    )
                                    .on_hover_text(&err);
                                    ui.end_row();
                                }
                            });
                    });
            });
        }
    }
}
