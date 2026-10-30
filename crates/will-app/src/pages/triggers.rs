//! Triggers (WhatsApp e e-mail): lista em cartões (fonte, escopo, thread,
//! último disparo, ações) e formulário com seletor de contatos do WhatsApp,
//! condições (campo + glob), thread existente ou nova, prompt e cooldown.
//! O agendamento por tempo é a outra aba (`schedules.rs`). Ver `docs/30`.

use super::{Cx, Nav, SettingsSection};
use crate::i18n::{tr, trf};
use crate::service::{Loader, Task};
use crate::ui::{self, Tone};
use egui::RichText;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use will_core::{Event, ThreadSummary, TriggerSource};

#[derive(Default)]
pub struct TriggersPage {
    list: Loader<Value>,
    threads: Loader<Value>,
    thread_list: Vec<ThreadSummary>,
    /// Contas e conversas do WhatsApp para o seletor (`GET /api/whatsapp/chats`).
    chats: Loader<Value>,
    chat_query: String,
    chat_query_sent: String,
    form: Option<Form>,
    actions: Vec<(String, Task<Value>)>,
    confirm_delete: Option<(String, String)>,
    expanded: HashSet<String>,
    /// Histórico por trigger (carregado ao expandir).
    runs: HashMap<String, Loader<Value>>,
    /// Popup do seletor de contatos acabou de abrir: foca a busca.
    picker_focus: bool,
}

#[derive(Clone)]
struct Cond {
    field: String,
    glob: String,
    negate: bool,
}

struct Form {
    editing: Option<String>,
    name: String,
    source: String,
    enabled: bool,
    account: String,
    chats: Vec<(String, String)>,
    include_groups: bool,
    include_from_me: bool,
    include_status: bool,
    gmail_query: String,
    conditions: Vec<Cond>,
    /// `"existing"` ou `"new"`.
    thread_mode: String,
    thread_id: String,
    thread_title: String,
    prompt: String,
    cooldown_secs: u32,
    debounce_secs: u32,
    save: Option<Task<Value>>,
}

impl Form {
    fn new(source: &str, default_thread: Option<String>, cooldown: u32, debounce: u32) -> Self {
        Self {
            editing: None,
            name: String::new(),
            source: source.into(),
            enabled: true,
            account: String::new(),
            chats: Vec::new(),
            include_groups: false,
            include_from_me: false,
            include_status: false,
            gmail_query: String::new(),
            conditions: Vec::new(),
            thread_mode: "new".into(),
            thread_id: default_thread.unwrap_or_default(),
            thread_title: String::new(),
            prompt: String::new(),
            cooldown_secs: cooldown,
            debounce_secs: debounce,
            save: None,
        }
    }

    fn from_view(v: &Value) -> Self {
        let f = &v["filter"];
        let mut out = Self::new(&ui::s(v, "source"), None, 0, 0);
        out.editing = Some(ui::s(v, "id"));
        out.name = ui::s(v, "name");
        out.enabled = ui::b(v, "enabled");
        out.account = ui::s(f, "account");
        out.chats = f
            .get("chats")
            .and_then(|c| c.as_array())
            .map(|a| {
                a.iter()
                    .map(|c| (ui::s(c, "jid"), ui::s(c, "name")))
                    .collect()
            })
            .unwrap_or_default();
        out.include_groups = ui::b(f, "include_groups");
        out.include_from_me = ui::b(f, "include_from_me");
        out.include_status = ui::b(f, "include_status");
        out.gmail_query = ui::s(f, "gmail_query");
        out.conditions = f
            .get("conditions")
            .and_then(|c| c.as_array())
            .map(|a| {
                a.iter()
                    .map(|c| Cond {
                        field: ui::s(c, "field"),
                        glob: ui::s(c, "glob"),
                        negate: ui::b(c, "negate"),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let tid = ui::s(v, "thread_id");
        out.thread_mode = if tid.is_empty() {
            "new".into()
        } else {
            "existing".into()
        };
        out.thread_id = tid;
        out.thread_title = ui::s(v, "thread_title");
        out.prompt = ui::s(v, "prompt");
        out.cooldown_secs = v.get("cooldown_secs").and_then(|c| c.as_u64()).unwrap_or(0) as u32;
        out.debounce_secs = v.get("debounce_secs").and_then(|c| c.as_u64()).unwrap_or(0) as u32;
        out
    }

    fn source(&self) -> TriggerSource {
        TriggerSource::parse(&self.source).unwrap_or(TriggerSource::WhatsApp)
    }

    fn saving(&self) -> bool {
        self.save.as_ref().is_some_and(|t| t.pending())
    }

    fn input(&self) -> Result<Value, String> {
        if self.name.trim().is_empty() {
            return Err(tr("Dê um nome à trigger.").into());
        }
        if self.prompt.trim().is_empty() {
            return Err(tr("Escreva o prompt que a thread vai receber.").into());
        }
        if self.thread_mode == "existing" && self.thread_id.trim().is_empty() {
            return Err(tr("Escolha a thread ou marque \"criar nova\".").into());
        }
        let conditions: Vec<Value> = self
            .conditions
            .iter()
            .filter(|c| !c.field.trim().is_empty() && !c.glob.trim().is_empty())
            .map(|c| json!({ "field": c.field.trim(), "glob": c.glob.trim(), "negate": c.negate }))
            .collect();
        let chats: Vec<Value> = self
            .chats
            .iter()
            .map(|(jid, name)| json!({ "jid": jid, "name": name }))
            .collect();
        Ok(json!({
            "name": self.name.trim(),
            "source": self.source,
            "enabled": self.enabled,
            "prompt": self.prompt.trim(),
            "thread_id": if self.thread_mode == "existing" { self.thread_id.trim() } else { "" },
            "thread_title": self.thread_title.trim(),
            "cooldown_secs": self.cooldown_secs,
            "debounce_secs": self.debounce_secs,
            "filter": {
                "account": self.account.trim(),
                "chats": chats,
                "include_groups": self.include_groups,
                "include_from_me": self.include_from_me,
                "include_status": self.include_status,
                "gmail_query": self.gmail_query.trim(),
                "conditions": conditions,
            },
        }))
    }
}

impl TriggersPage {
    pub fn on_event(&mut self, ev: &Event) {
        match ev {
            Event::TriggersChanged | Event::TriggerFired { .. } => {
                self.list.loaded_at = None;
                self.runs.clear();
            }
            Event::ThreadCreated { .. } | Event::ThreadDeleted { .. } => {
                self.threads.loaded_at = None;
                self.list.loaded_at = None;
            }
            _ => {}
        }
    }

    fn reload(&mut self, cx: &Cx) {
        self.list
            .start(cx.svc.call(|c| async move { c.triggers().await }));
    }

    fn push_action(&mut self, done_text: String, task: Task<Value>) {
        self.actions.push((done_text, task));
    }

    fn update_enabled(&mut self, cx: &Cx, id: String, enabled: bool) {
        let task = cx.svc.call(move |c| async move {
            c.update_trigger(&id, json!({ "enabled": enabled })).await
        });
        let text = if enabled {
            tr("Trigger retomada.")
        } else {
            tr("Trigger pausada.")
        };
        self.push_action(text.into(), task);
    }

    fn test(&mut self, cx: &Cx, id: String, name: &str) {
        let task = cx
            .svc
            .call(move |c| async move { c.test_trigger(&id).await });
        self.push_action(
            trf("\"{}\" disparada com um evento de teste.", &[&name]),
            task,
        );
    }

    fn delete(&mut self, cx: &Cx, id: String, name: &str) {
        let task = cx
            .svc
            .call(move |c| async move { c.delete_trigger(&id).await });
        self.push_action(trf("\"{}\" excluída.", &[&name]), task);
    }

    fn poll(&mut self, cx: &mut Cx) {
        if self.list.stale(Duration::from_secs(60)) {
            self.reload(cx);
        }
        self.list.poll();
        if self.threads.stale(Duration::from_secs(120)) {
            self.threads
                .start(cx.svc.call(|c| async move { c.threads().await }));
        }
        if self.threads.poll() {
            if let Some(v) = &self.threads.data {
                if let Ok(list) = serde_json::from_value::<Vec<ThreadSummary>>(v.clone()) {
                    self.thread_list = list;
                }
            }
        }
        // Seletor de contatos: consome a resposta ANTES de decidir recarregar e
        // só recarrega sem tarefa pendente/concluída (`busy`), senão `start`
        // descarta o resultado que acabou de chegar e vira um laço de requisições.
        self.chats.poll();
        if self.form.is_some()
            && (self.chats.data.is_none() && self.chats.error.is_none()
                || self.chat_query != self.chat_query_sent)
            && !self.chats.busy()
        {
            self.chat_query_sent = self.chat_query.clone();
            let q = self.chat_query.clone();
            self.chats
                .start(cx.svc.call(move |c| async move { c.wa_chats(&q).await }));
        }
        for l in self.runs.values_mut() {
            l.poll();
        }

        let mut results = Vec::new();
        self.actions.retain(|(text, t)| match t.take() {
            Some(r) => {
                results.push((text.clone(), r));
                false
            }
            None => true,
        });
        for (text, r) in results {
            match r {
                Ok(_) => cx.toasts.success(text),
                Err(e) => cx.toasts.error(e),
            }
            self.list.loaded_at = None;
        }
        if let Some(f) = &mut self.form {
            if let Some(t) = &f.save {
                if let Some(r) = t.take() {
                    f.save = None;
                    match r {
                        Ok(_) => {
                            cx.toasts.success(if f.editing.is_some() {
                                tr("Trigger salva.")
                            } else {
                                tr("Trigger criada.")
                            });
                            self.form = None;
                            self.list.loaded_at = None;
                        }
                        Err(e) => cx.toasts.error(trf("Não salvou: {}", &[&e])),
                    }
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        self.poll(cx);
        let enabled = self
            .list
            .data
            .as_ref()
            .map(|v| ui::b(v, "enabled"))
            .unwrap_or(cx.snap.settings.triggers.enabled);
        let gmail_available = self
            .list
            .data
            .as_ref()
            .map(|v| ui::b(v, "gmail_available"))
            .unwrap_or(false);
        if !enabled {
            let dark = ui.visuals().dark_mode;
            let warn = Tone::Warning.color(dark);
            egui::Frame::new()
                .fill(warn.linear_multiply(if dark { 0.18 } else { 0.12 }))
                .stroke(egui::Stroke::new(1.0, warn))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(tr(
                                "Triggers desligadas nas configurações: nada dispara.",
                            ))
                            .color(warn),
                        );
                        if ui.button(tr("Configurar")).clicked() {
                            *cx.go = Some(Nav::Settings(SettingsSection::Scheduler));
                        }
                    });
                });
            ui.add_space(6.0);
        }

        ui.horizontal(|ui| {
            let accent = ui::palette(ui.visuals().dark_mode).accent;
            if ui
                .add_enabled(
                    self.form.is_none(),
                    egui::Button::new(
                        RichText::new(tr("Nova trigger")).color(egui::Color32::WHITE),
                    )
                    .fill(accent),
                )
                .clicked()
            {
                let cooldown = cx.snap.settings.triggers.default_cooldown_secs;
                let debounce = cx.snap.settings.triggers.default_debounce_secs;
                let t = self.thread_list.first().map(|t| t.id.to_string());
                self.form = Some(Form::new("whatsapp", t, cooldown, debounce));
            }
            if ui.button(tr("Atualizar")).clicked() {
                self.list.loaded_at = None;
                self.threads.loaded_at = None;
                self.chats.loaded_at = None;
                self.chats.data = None;
            }
            if gmail_available
                && ui
                    .button(tr("Varrer Gmail agora"))
                    .on_hover_text(tr(
                        "Consulta a caixa de entrada fora do intervalo configurado.",
                    ))
                    .clicked()
            {
                let task = cx
                    .svc
                    .call(|c| async move { c.triggers_gmail_poll().await });
                self.push_action(tr("Varredura do Gmail feita.").into(), task);
            }
            if self.list.loading() {
                ui.spinner();
            }
        });

        if self.form.is_some() {
            self.form_ui(ui, cx, gmail_available);
        }

        let rows: Vec<Value> = self
            .list
            .data
            .as_ref()
            .and_then(|v| v.get("triggers"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let title = if rows.is_empty() {
            tr("Triggers").to_string()
        } else {
            trf("Triggers ({})", &[&rows.len()])
        };
        ui::section(ui, &title, |ui| {
            ui.set_width(ui.available_width());
            if let Some(e) = &self.list.error {
                ui::error_text(ui, e);
            }
            if rows.is_empty() {
                if self.list.loading() {
                    ui::busy(ui, tr("carregando triggers…"));
                } else {
                    ui.label(RichText::new(tr(
                        "Nenhuma trigger. Crie uma: mensagem de um contato do WhatsApp ou e-mail que casa com um filtro acorda uma thread com o seu prompt.",
                    )).weak());
                }
                return;
            }
            for v in &rows {
                self.card(ui, cx, v);
                ui.add_space(4.0);
            }
        });

        self.confirm_ui(ui, cx);
    }

    // ---------- formulário ----------

    fn form_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, gmail_available: bool) {
        // Tira o formulário de `self` durante o desenho para poder mexer nos
        // outros campos da página (busca de contatos, listas) sem brigar com o borrow.
        let Some(mut f) = self.form.take() else {
            return;
        };
        let mut submit = false;
        let mut cancel = false;
        let title = if f.editing.is_some() {
            tr("Editar trigger")
        } else {
            tr("Nova trigger")
        };
        let dark = ui.visuals().dark_mode;
        ui::section(ui, title, |ui| {
            ui.set_width(ui.available_width());
            ui::field(ui, tr("Nome"), None, |ui| {
                ui::text_input(ui, &mut f.name, tr("ex.: pedidos do Pedro"));
            });
            ui::field(ui, tr("Fonte"), None, |ui| {
                let opts: Vec<(&str, &str)> =
                    vec![("whatsapp", "WhatsApp"), ("gmail", tr("E-mail (Gmail)"))];
                ui::select(ui, "trigger-source", &mut f.source, &opts);
                if f.source == "gmail" && !gmail_available {
                    ui.colored_label(
                        Tone::Warning.color(dark),
                        tr("Fonte Gmail indisponível: conecte uma conta Google com Gmail em Configurações → Google."),
                    );
                }
            });
            ui::field(ui, tr("Habilitada"), None, |ui| {
                ui::toggle(ui, &mut f.enabled);
            });

            // ---- escopo ----
            ui.add_space(6.0);
            ui.label(RichText::new(tr("Escopo")).strong());
            match f.source() {
                TriggerSource::WhatsApp => self.whatsapp_scope(ui, &mut f),
                TriggerSource::Gmail => {
                    let accounts: Vec<String> = self
                        .list
                        .data
                        .as_ref()
                        .and_then(|v| v.get("gmail_accounts"))
                        .and_then(|a| a.as_array())
                        .map(|a| a.iter().map(|x| ui::s(x, "email")).collect())
                        .unwrap_or_default();
                    ui::field(
                        ui,
                        tr("Conta Google"),
                        Some(tr("Caixa de entrada que será varrida (conta conectada em Configurações → Google). Não é filtro de remetente: para isso use uma condição from, ex.: *@xpto.com.")),
                        |ui| {
                            if accounts.is_empty() {
                                ui.colored_label(Tone::Warning.color(dark), tr("Nenhuma conta Google com Gmail conectada."));
                            } else {
                                let any = tr("a única / qualquer");
                                let mut opts: Vec<(&str, &str)> = vec![("", any)];
                                opts.extend(accounts.iter().map(|a| (a.as_str(), a.as_str())));
                                ui::select(ui, "trigger-gmail-account", &mut f.account, &opts);
                            }
                        },
                    );
                    ui::field(
                        ui,
                        tr("Busca do Gmail"),
                        Some(tr("Sintaxe de busca do Gmail; vazio = in:inbox. Ex.: is:unread, label:clientes, from:@xpto.com.")),
                        |ui| {
                            ui::text_input(ui, &mut f.gmail_query, "in:inbox");
                        },
                    );
                }
            }

            // ---- condições ----
            ui.add_space(6.0);
            ui.label(RichText::new(tr("Condições")).strong());
            ui.label(
                RichText::new(tr(
                    "Todas precisam casar (E). Glob: * = qualquer trecho, ? = um caractere, ^ e $ ancoram; sem curinga = contém. Ex.: from = *@xpto.com · subject = *fatura* · text = ^urgente",
                ))
                .small()
                .weak(),
            );
            let fields = f.source().fields();
            let mut remove: Option<usize> = None;
            for (i, c) in f.conditions.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let opts: Vec<(&str, &str)> = fields.iter().map(|x| (*x, *x)).collect();
                    ui::select(ui, &format!("cond-field-{i}"), &mut c.field, &opts);
                    let w = (ui.available_width() - 150.0).max(120.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut c.glob)
                            .margin(crate::ui::input_margin())
                            .desired_width(w)
                            .hint_text("*@xpto.com"),
                    );
                    ui.checkbox(&mut c.negate, tr("não"))
                        .on_hover_text(tr("Inverte: dispara quando NÃO casa."));
                    if ui::icon_button(ui, crate::icons::CLOSE, tr("remover")).clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                f.conditions.remove(i);
            }
            if ui.button(tr("+ condição")).clicked() {
                f.conditions.push(Cond {
                    field: fields.first().map(|x| x.to_string()).unwrap_or_default(),
                    glob: String::new(),
                    negate: false,
                });
            }

            // ---- thread e prompt ----
            ui.add_space(6.0);
            ui.label(RichText::new(tr("Thread e prompt")).strong());
            ui::field(
                ui,
                tr("Thread"),
                Some(tr("Para não misturar contexto, uma thread por contato/assunto. \"Criar nova\" cria no primeiro disparo e fixa aqui.")),
                |ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut f.thread_mode, "new".into(), tr("criar nova"));
                        ui.radio_value(&mut f.thread_mode, "existing".into(), tr("existente"));
                    });
                    if f.thread_mode == "existing" {
                        let opts: Vec<(String, String)> = self
                            .thread_list
                            .iter()
                            .map(|t| {
                                (
                                    t.id.to_string(),
                                    t.title.clone().unwrap_or_else(|| t.id.to_string()),
                                )
                            })
                            .collect();
                        let refs: Vec<(&str, &str)> =
                            opts.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
                        if refs.is_empty() {
                            ui.label(RichText::new(tr("Nenhuma thread ainda.")).weak());
                        } else {
                            ui::select(ui, "trigger-thread", &mut f.thread_id, &refs);
                        }
                    } else {
                        ui::text_input(ui, &mut f.thread_title, tr("título da thread nova (opcional)"));
                    }
                },
            );
            ui::field(
                ui,
                tr("Prompt"),
                Some(tr("O que a thread deve fazer a cada evento. Ela recebe este texto + todo o contexto (remetente, chat/assunto, hora, corpo…).")),
                |ui| {
                    ui::text_area(ui, &mut f.prompt, 6);
                },
            );
            if f.source() == TriggerSource::WhatsApp {
                ui::field(
                    ui,
                    tr("Agrupar mensagens (segundos)"),
                    Some(tr(
                        "Espera a pessoa parar de escrever: só dispara quando passam estes segundos sem mensagem nova na conversa, com todas as mensagens juntas. 0 = cada mensagem dispara na hora.",
                    )),
                    |ui| {
                        ui::number(ui, &mut f.debounce_secs, 0..=600);
                    },
                );
            }
            ui::field(
                ui,
                tr("Cooldown (segundos)"),
                Some(tr(
                    "Intervalo mínimo entre dois disparos desta trigger; 0 = sem limite.",
                )),
                |ui| {
                    ui::number(ui, &mut f.cooldown_secs, 0..=86_400);
                },
            );

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if f.saving() {
                    ui::busy(ui, tr("salvando…"));
                } else {
                    if ui::primary_button(ui, tr("Salvar")).clicked() {
                        submit = true;
                    }
                    if ui.button(tr("Cancelar")).clicked() {
                        cancel = true;
                    }
                }
            });
        });

        if submit {
            match f.input() {
                Ok(body) => {
                    let editing = f.editing.clone();
                    f.save = Some(cx.svc.call(move |c| async move {
                        match editing {
                            Some(id) => c.update_trigger(&id, body).await,
                            None => c.create_trigger(body).await,
                        }
                    }));
                }
                Err(e) => cx.toasts.error(e),
            }
        }
        if !cancel {
            self.form = Some(f);
        }
    }

    /// Escopo do WhatsApp: conta, contatos escolhidos (chips), busca com lista
    /// de conversas/contatos do índice e permissões (grupos, minhas mensagens).
    fn whatsapp_scope(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let accounts: Vec<(String, String)> = self
            .chats
            .data
            .as_ref()
            .and_then(|v| v.get("accounts"))
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .map(|x| (ui::s(x, "id"), ui::s(x, "label")))
                    .collect()
            })
            .unwrap_or_default();
        if accounts.len() > 1 {
            ui::field(
                ui,
                tr("Conta"),
                Some(tr("Vazio = qualquer conta pareada.")),
                |ui| {
                    let any = tr("qualquer");
                    let mut opts: Vec<(&str, &str)> = vec![("", any)];
                    opts.extend(accounts.iter().map(|(id, l)| (id.as_str(), l.as_str())));
                    ui::select(ui, "trigger-wa-account", &mut f.account, &opts);
                },
            );
        }
        let chats: Vec<Value> = self
            .chats
            .data
            .as_ref()
            .and_then(|v| v.get("chats"))
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        let loading = self.chats.loading();
        let error = self.chats.error.clone();
        let query = &mut self.chat_query;
        let picker_focus = &mut self.picker_focus;
        ui::field(
            ui,
            tr("Contatos / conversas"),
            Some(tr("Só mensagens destes chats disparam. Vazio = qualquer conversa 1:1 (grupos só se permitido abaixo).")),
            |ui| {
                // chips dos escolhidos + botão que abre o seletor
                let mut remove: Option<usize> = None;
                ui.horizontal_wrapped(|ui| {
                    for (i, (jid, name)) in f.chats.iter().enumerate() {
                        let label = if name.is_empty() { jid.clone() } else { name.clone() };
                        if ui
                            .add(egui::Button::new(format!("{label}  ×")).small())
                            .on_hover_text(trf("{} · clique para remover", &[&jid]))
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    }
                    let btn = ui.add(egui::Button::new(tr("+ adicionar contato…")).small());
                    if btn.clicked() {
                        *picker_focus = true;
                    }
                    egui::containers::Popup::menu(&btn)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .width(380.0)
                        .show(|ui| {
                            ui.set_min_width(360.0);
                            let r = ui.add(
                                egui::TextEdit::singleline(query).margin(crate::ui::input_margin())
                                    .hint_text(tr("buscar por nome ou telefone…"))
                                    .desired_width(f32::INFINITY),
                            );
                            if *picker_focus {
                                r.request_focus();
                                *picker_focus = false;
                            }
                            if loading {
                                ui::busy(ui, tr("carregando conversas…"));
                            }
                            if let Some(e) = &error {
                                ui::error_text(ui, e);
                            }
                            let q = query.trim().to_lowercase();
                            let rows: Vec<&Value> = chats
                                .iter()
                                .filter(|c| {
                                    if !f.account.is_empty() && ui::s(c, "account_id") != f.account {
                                        return false;
                                    }
                                    if ui::s(c, "jid") == "status@broadcast" {
                                        return false;
                                    }
                                    if q.is_empty() {
                                        return true;
                                    }
                                    let hay = format!(
                                        "{} {} {}",
                                        ui::s(c, "name"),
                                        ui::s(c, "jid"),
                                        ui::s(c, "phone")
                                    )
                                    .to_lowercase();
                                    hay.contains(&q)
                                })
                                .collect();
                            if rows.is_empty() && !loading {
                                ui.label(
                                    RichText::new(if chats.is_empty() {
                                        tr("Nenhuma conversa no índice (WhatsApp pareado?).")
                                    } else {
                                        tr("Nada encontrado.")
                                    })
                                    .small()
                                    .weak(),
                                );
                            }
                            let dark = ui.visuals().dark_mode;
                            let weak = ui::palette(dark).text_weak;
                            egui::ScrollArea::vertical()
                                .id_salt("trigger-chats")
                                .max_height(280.0)
                                .show(ui, |ui| {
                                    for c in rows {
                                        let jid = ui::s(c, "jid");
                                        let name = ui::s(c, "name");
                                        let phone = ui::s(c, "phone");
                                        let is_group = ui::b(c, "is_group");
                                        let mut on = f.chats.iter().any(|(j, _)| *j == jid);
                                        ui.horizontal(|ui| {
                                            if ui.checkbox(&mut on, "").changed() {
                                                if on {
                                                    f.chats.push((jid.clone(), name.clone()));
                                                } else {
                                                    f.chats.retain(|(j, _)| *j != jid);
                                                }
                                            }
                                            ui.vertical(|ui| {
                                                ui.spacing_mut().item_spacing.y = 0.0;
                                                ui.label(RichText::new(if name.is_empty() {
                                                    jid.clone()
                                                } else {
                                                    name.clone()
                                                }));
                                                let sub = if is_group {
                                                    tr("grupo").to_string()
                                                } else if !phone.is_empty() {
                                                    phone.clone()
                                                } else {
                                                    jid.split('@').next().unwrap_or("").to_string()
                                                };
                                                ui.label(RichText::new(sub).small().color(weak));
                                            });
                                        });
                                    }
                                });
                        });
                });
                if let Some(i) = remove {
                    f.chats.remove(i);
                }
            },
        );
        ui::field(ui, tr("Aceitar grupos"), Some(tr("Mensagens de grupos escolhidos acima (ou de qualquer grupo, se nenhum chat foi escolhido).")), |ui| {
            ui::toggle(ui, &mut f.include_groups);
        });
        ui::field(
            ui,
            tr("Aceitar minhas mensagens"),
            Some(tr("Disparar também com o que eu mesmo envio nesses chats.")),
            |ui| {
                ui::toggle(ui, &mut f.include_from_me);
            },
        );
        ui::field(
            ui,
            tr("Ouvir status do WhatsApp"),
            Some(tr("Desligado (padrão): foto/texto que o contato publica no status não dispara; só mensagens de conversa. Ligado: status também dispara.")),
            |ui| {
                ui::toggle(ui, &mut f.include_status);
            },
        );
    }

    // ---------- cartões ----------

    fn card(&mut self, ui: &mut egui::Ui, cx: &Cx, v: &Value) {
        let dark = ui.visuals().dark_mode;
        let p = ui::palette(dark);
        let id = ui::s(v, "id");
        let name = ui::s(v, "name");
        let enabled = ui::b(v, "enabled");
        let busy = self.actions.iter().any(|(_, t)| t.pending());
        let source = ui::s(v, "source");
        let source_label = ui::s(v, "source_label");
        let last_result = ui::s(v, "last_result");
        let failed = !last_result.is_empty() && last_result != "ok";
        let expanded = self.expanded.contains(&id);
        let tone = if !enabled {
            Tone::Neutral
        } else if failed {
            Tone::Danger
        } else {
            Tone::Success
        };
        let filter = &v["filter"];
        let scope = scope_summary(&source, filter);
        let thread_name = ui::s(v, "thread_name");
        let thread_id = ui::s(v, "thread_id");
        let thread_exists = ui::b(v, "thread_exists");
        let fire_count = v.get("fire_count").and_then(|x| x.as_i64()).unwrap_or(0);
        let last_local = ui::s(v, "last_fired_local");

        egui::Frame::new()
            .fill(if expanded { p.card_alt } else { p.card })
            .stroke(egui::Stroke::new(
                1.0,
                if enabled { p.border } else { p.border.linear_multiply(0.6) },
            ))
            .corner_radius(8.0)
            .inner_margin(egui::Margin::symmetric(12, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let mut on = enabled;
                    if ui
                        .add_enabled_ui(!busy, |ui| ui::toggle(ui, &mut on))
                        .inner
                        .on_hover_text(if enabled { tr("Pausar") } else { tr("Retomar") })
                        .changed()
                    {
                        self.update_enabled(cx, id.clone(), on);
                    }
                    ui::dot(ui, tone);
                    let name_text = RichText::new(&name).font(ui::medium(14.0));
                    ui.label(if enabled { name_text } else { name_text.weak() });
                    ui::badge(
                        ui,
                        &source_label,
                        if source == "gmail" { Tone::Info } else { Tone::Success },
                    );
                    ui.add(egui::Label::new(RichText::new(&scope).small().weak()).truncate());
                    let deb = v.get("debounce_secs").and_then(|x| x.as_u64()).unwrap_or(0);
                    if source != "gmail" && deb > 0 {
                        ui.label(RichText::new(trf("agrupa {}s", &[&deb])).small().weak())
                            .on_hover_text(tr("Mensagens seguidas são juntadas e disparam uma vez quando a conversa silencia."));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        let chevron = if expanded {
                            crate::icons::EXPAND_MORE
                        } else {
                            crate::icons::CHEVRON_RIGHT
                        };
                        if ui::icon_button(ui, chevron, if expanded { tr("recolher") } else { tr("detalhes") }).clicked() {
                            if expanded {
                                self.expanded.remove(&id);
                            } else {
                                self.expanded.insert(id.clone());
                            }
                        }
                        if !enabled {
                            let reason = ui::s(v, "paused_reason");
                            ui.label(if reason.is_empty() {
                                RichText::new(tr("pausada")).small().color(Tone::Neutral.color(dark))
                            } else {
                                RichText::new(trf("pausada · {}", &[&reason])).small().color(Tone::Warning.color(dark))
                            });
                        } else if last_local.is_empty() {
                            ui.label(RichText::new(tr("nunca disparou")).small().weak());
                        } else {
                            ui.label(RichText::new(trf("{}× · último {}", &[&fire_count, &last_local])).small().weak());
                        }
                    });
                });
                // linha 2: thread + último resultado
                ui.horizontal(|ui| {
                    ui.add_space(30.0);
                    let t = if thread_id.is_empty() {
                        tr("thread: cria no primeiro disparo").to_string()
                    } else if thread_exists {
                        trf("thread: {}", &[&if thread_name.is_empty() { thread_id.clone() } else { thread_name.clone() }])
                    } else {
                        tr("thread removida: será recriada no próximo disparo").to_string()
                    };
                    ui.label(RichText::new(t).small().weak());
                    if failed {
                        ui.label(RichText::new(trf("falhou: {}", &[&last_result])).small().color(Tone::Danger.color(dark)));
                    }
                });
                if expanded {
                    ui.add_space(4.0);
                    ui.label(RichText::new(tr("Prompt")).small().strong());
                    ui.add(egui::Label::new(RichText::new(ui::s(v, "prompt")).small()).wrap());
                    let conds = filter.get("conditions").and_then(|c| c.as_array()).cloned().unwrap_or_default();
                    if !conds.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new(tr("Condições")).small().strong());
                        for c in &conds {
                            ui.label(RichText::new(format!(
                                "{} {} {}",
                                ui::s(c, "field"),
                                if ui::b(c, "negate") { "≠" } else { "~" },
                                ui::s(c, "glob")
                            )).small().monospace());
                        }
                    }
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.add_enabled(!busy, egui::Button::new(tr("Testar"))).on_hover_text(tr("Dispara agora com um evento de exemplo (ignora filtro e cooldown).")).clicked() {
                            self.test(cx, id.clone(), &name);
                        }
                        if ui.add_enabled(self.form.is_none(), egui::Button::new(tr("Editar"))).clicked() {
                            self.form = Some(Form::from_view(v));
                        }
                        if ui::danger_button(ui, tr("Excluir")).clicked() {
                            self.confirm_delete = Some((id.clone(), name.clone()));
                        }
                    });
                    // histórico
                    let runs = self.runs.entry(id.clone()).or_default();
                    if runs.data.is_none() && !runs.loading() {
                        let rid = id.clone();
                        runs.start(cx.svc.call(move |c| async move { c.trigger_runs(&rid).await }));
                    }
                    if let Some(list) = runs.data.as_ref().and_then(|d| d.get("runs")).and_then(|r| r.as_array()) {
                        if !list.is_empty() {
                            ui.add_space(4.0);
                            ui.label(RichText::new(tr("Últimos disparos")).small().strong());
                            for r in list.iter().take(10) {
                                let ok = ui::b(r, "ok");
                                let input = ui::s(r, "input");
                                let detail = ui::s(r, "detail");
                                ui.horizontal(|ui| {
                                    ui::dot(ui, if ok { Tone::Success } else { Tone::Danger });
                                    ui.label(RichText::new(ui::fmt_dt(&r["at"])).small().weak());
                                    ui.add(egui::Label::new(RichText::new(if ok { input } else { detail }).small()).truncate());
                                });
                            }
                        }
                    }
                }
            });
    }

    fn confirm_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let Some((id, name)) = self.confirm_delete.clone() else {
            return;
        };
        let c = ui::Confirm {
            title: tr("Excluir trigger").into(),
            body: trf(
                "Excluir a trigger \"{}\"? A thread dela continua existindo.",
                &[&name],
            ),
            confirm_label: tr("Excluir").into(),
            danger: true,
            tag: "del-trigger".into(),
        };
        if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
            self.confirm_delete = None;
            if ok {
                self.delete(cx, id, &name);
            }
        }
    }
}

/// Resumo do escopo para a linha do cartão.
fn scope_summary(source: &str, filter: &Value) -> String {
    if source == "gmail" {
        let q = ui::s(filter, "gmail_query");
        let acc = ui::s(filter, "account");
        let mut s = if q.is_empty() {
            "in:inbox".to_string()
        } else {
            q
        };
        if !acc.is_empty() {
            s = format!("{acc} · {s}");
        }
        return s;
    }
    let chats = filter
        .get("chats")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    if chats.is_empty() {
        let mut s = tr("qualquer conversa").to_string();
        if ui::b(filter, "include_groups") {
            s.push_str(tr(" + grupos"));
        }
        return s;
    }
    let names: Vec<String> = chats
        .iter()
        .map(|c| {
            let n = ui::s(c, "name");
            if n.is_empty() {
                ui::s(c, "jid")
            } else {
                n
            }
        })
        .collect();
    if names.len() <= 3 {
        names.join(", ")
    } else {
        trf(
            "{} e mais {}",
            &[&names[..2].join(", "), &(names.len() - 2)],
        )
    }
}
