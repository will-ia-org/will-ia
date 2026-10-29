//! Seção Motor Claude: tokens (adicionar/testar/ativar/remover), binário,
//! modelo, permissões, limites.

use super::SettingsState;
use crate::help;
use crate::i18n::{tr, trf};
use crate::pages::Cx;
use crate::service::{Loader, Task};
use crate::ui::{self, Tone};
use serde_json::Value;
use will_core::settings::PermissionMode;
use will_core::Event;

#[derive(Default)]
pub struct EnginePage {
    tokens: Loader<Value>,
    check: Loader<Value>,
    new_label: String,
    new_token: String,
    show_add: bool,
    add_task: Option<Task<Value>>,
    test_task: Option<(String, Task<Value>)>,
    activate_task: Option<(String, Task<Value>)>,
    delete_task: Option<Task<Value>>,
    confirm_delete: Option<(String, String)>,
    confirm_bypass: bool,
    allowed_buf: Option<String>,
    last_test_output: Option<String>,
}

impl EnginePage {
    pub fn on_event(&mut self, ev: &Event) {
        if let Event::SettingsChanged { section } = ev {
            if section == "engine" {
                self.tokens.loaded_at = None;
            }
        }
    }

    fn reload_tokens(&mut self, cx: &Cx) {
        self.tokens
            .start(cx.svc.call(|c| async move { c.engine_tokens().await }));
    }

    fn poll(&mut self, cx: &mut Cx) {
        if self.tokens.stale(std::time::Duration::from_secs(60)) {
            self.reload_tokens(cx);
        }
        self.tokens.poll();
        self.check.poll();
        if let Some(t) = &self.add_task {
            if let Some(r) = t.take() {
                self.add_task = None;
                match r {
                    Ok(v) => {
                        if ui::b(&v, "activated") {
                            cx.toasts.success(tr("Token cadastrado e ativado."));
                        } else if let Some(e) = v.get("activation_error").and_then(|e| e.as_str()) {
                            cx.toasts
                                .error(trf("Token cadastrado, mas a ativação falhou: {}", &[&e]));
                        } else {
                            cx.toasts.success(tr("Token cadastrado."));
                        }
                        self.new_label.clear();
                        self.new_token.clear();
                        self.show_add = false;
                        self.reload_tokens(cx);
                    }
                    Err(e) => cx.toasts.error(e),
                }
            }
        }
        if let Some((_, t)) = &self.test_task {
            if let Some(r) = t.take() {
                self.test_task = None;
                match r {
                    Ok(v) => {
                        let out = ui::s(&v, "output");
                        self.last_test_output = if out.is_empty() { None } else { Some(out) };
                        if ui::b(&v, "ok") {
                            cx.toasts.success(tr("Token OK."));
                        } else {
                            cx.toasts
                                .error(trf("Token reprovado: {}", &[&ui::s(&v, "error")]));
                        }
                        self.reload_tokens(cx);
                    }
                    Err(e) => cx.toasts.error(e),
                }
            }
        }
        if let Some((_, t)) = &self.activate_task {
            if let Some(r) = t.take() {
                self.activate_task = None;
                match r {
                    Ok(v) => {
                        let n = ui::i(&v, "restarted_threads");
                        cx.toasts.success(if n > 0 {
                            trf("Token ativado; {} thread(s) voltam com --resume.", &[&n])
                        } else {
                            tr("Token ativado.").into()
                        });
                        self.reload_tokens(cx);
                    }
                    Err(e) => cx.toasts.error(trf("Ativação falhou: {}", &[&e])),
                }
            }
        }
        if let Some(t) = &self.delete_task {
            if let Some(r) = t.take() {
                self.delete_task = None;
                match r {
                    Ok(_) => {
                        cx.toasts.success(tr("Token removido."));
                        self.reload_tokens(cx);
                    }
                    Err(e) => cx.toasts.error(e),
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        self.poll(cx);
        ui::heading(ui, tr("Motor Claude"), Some(tr("Processos `claude -p` headless, um por thread, rodando sobre a base de conhecimento. Nenhuma chamada direta à API da Anthropic.")));

        // ---- tokens ----
        ui::section_help(
            ui,
            tr("Tokens de assinatura (claude setup-token)"),
            Some(&help::ENGINE_TOKENS),
            |ui| {
                if let Some(e) = &self.tokens.error {
                    ui::error_text(ui, e);
                }
                let list = self
                    .tokens
                    .data
                    .as_ref()
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                if list.is_empty() && !self.tokens.loading() {
                    ui.label(egui::RichText::new(tr("Nenhum token cadastrado. Gere um com `claude setup-token` no terminal e cole abaixo.")).weak());
                }
                egui::Grid::new("tokens-grid")
                    .num_columns(5)
                    .striped(true)
                    .spacing([14.0, 6.0])
                    .show(ui, |ui| {
                        for t in &list {
                            let id = ui::s(t, "id");
                            let active = ui::b(t, "active");
                            ui.horizontal(|ui| {
                                if active {
                                    ui::badge(ui, tr("ativo"), Tone::Success);
                                }
                                ui.label(egui::RichText::new(ui::s(t, "label")).strong());
                            });
                            ui.monospace(ui::s(t, "masked"));
                            let status = ui::s(t, "status");
                            match status.as_str() {
                                "ok" => ui::badge(ui, tr("ok"), Tone::Success),
                                "error" => ui::badge(ui, tr("erro"), Tone::Danger),
                                _ => ui::badge(ui, tr("não testado"), Tone::Neutral),
                            }
                            ui.label(
                                egui::RichText::new(
                                    t.get("error").and_then(|e| e.as_str()).unwrap_or(""),
                                )
                                .small()
                                .weak(),
                            );
                            ui.horizontal(|ui| {
                                let testing =
                                    self.test_task.as_ref().is_some_and(|(i, _)| i == &id);
                                let activating =
                                    self.activate_task.as_ref().is_some_and(|(i, _)| i == &id);
                                if testing || activating {
                                    ui.spinner();
                                }
                                if ui
                                    .add_enabled(
                                        !testing && !activating,
                                        egui::Button::new(tr("Testar")),
                                    )
                                    .clicked()
                                {
                                    let id2 = id.clone();
                                    self.test_task = Some((
                                        id.clone(),
                                        cx.svc.call(move |c| async move {
                                            c.test_engine_token(&id2).await
                                        }),
                                    ));
                                }
                                if !active
                                    && ui
                                        .add_enabled(
                                            !testing && !activating,
                                            egui::Button::new(tr("Ativar")),
                                        )
                                        .clicked()
                                {
                                    let id2 = id.clone();
                                    self.activate_task = Some((
                                        id.clone(),
                                        cx.svc.call(move |c| async move {
                                            c.activate_engine_token(&id2).await
                                        }),
                                    ));
                                }
                                if ui.button(tr("remover")).clicked() {
                                    self.confirm_delete = Some((id.clone(), ui::s(t, "label")));
                                }
                            });
                            ui.end_row();
                        }
                    });
                if let Some(out) = &self.last_test_output {
                    ui.collapsing(tr("Saída do último teste"), |ui| ui::mono(ui, out));
                }
                ui.add_space(6.0);
                if !self.show_add {
                    if ui::primary_button(ui, tr("Adicionar token")).clicked() {
                        self.show_add = true;
                    }
                } else {
                    ui::field(ui, tr("Rótulo"), Some(tr("Ex.: conta pessoal")), |ui| {
                        ui::text_input(ui, &mut self.new_label, tr("rótulo"));
                    });
                    ui::field(ui, tr("Token"), Some(tr("Saída de `claude setup-token` (sk-ant-oat01-…). Vai para o cofre de segredos, nunca para o banco.")), |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.new_token).password(true).desired_width(f32::INFINITY));
                });
                    ui.horizontal(|ui| {
                        let ok = self.new_token.trim().len() >= 20 && self.add_task.is_none();
                        if ui
                            .add_enabled(ok, egui::Button::new(tr("Cadastrar e ativar")))
                            .clicked()
                        {
                            let label = self.new_label.clone();
                            let token = self.new_token.clone();
                            self.add_task = Some(cx.svc.call(move |c| async move {
                                c.post(
                                    "engine/tokens",
                                    Some(serde_json::json!({
                                        "label": label,
                                        "token": token,
                                        "activate": true,
                                    })),
                                )
                                .await
                            }));
                        }
                        if ui
                            .add_enabled(ok, egui::Button::new(tr("Só cadastrar")))
                            .clicked()
                        {
                            let label = self.new_label.clone();
                            let token = self.new_token.clone();
                            self.add_task = Some(cx.svc.call(move |c| async move {
                                c.post(
                                    "engine/tokens",
                                    Some(serde_json::json!({
                                        "label": label,
                                        "token": token,
                                        "activate": false,
                                    })),
                                )
                                .await
                            }));
                        }
                        if ui.button(tr("Cancelar")).clicked() {
                            self.show_add = false;
                        }
                        if self.add_task.is_some() {
                            ui.spinner();
                        }
                    });
                }
            },
        );

        if let Some((id, label)) = self.confirm_delete.clone() {
            let c = ui::Confirm {
                title: tr("Remover token").into(),
                body: trf(
                    "Remover o token '{}'? Se estiver ativo, o motor fica sem token até ativar outro.",
                    &[&label],
                ),
                confirm_label: tr("Remover").into(),
                danger: true,
                tag: "del-token".into(),
            };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_delete = None;
                if ok {
                    self.delete_task = Some(
                        cx.svc
                            .call(move |c| async move { c.delete_engine_token(&id).await }),
                    );
                }
            }
        }

        // ---- execução ----
        ui::section_help(ui, tr("Execução"), Some(&help::ENGINE_EXECUTION), |ui| {
            let e = &mut st.draft.engine;
            ui::field(
                ui,
                tr("Binário claude"),
                Some(tr(
                    "Caminho ou nome no PATH. Instale com: npm i -g @anthropic-ai/claude-code",
                )),
                |ui| {
                    ui.horizontal(|ui| {
                        let w = (ui.available_width() - 110.0).max(160.0);
                        ui.add(egui::TextEdit::singleline(&mut e.claude_bin).desired_width(w));
                        if ui.button(tr("Verificar")).clicked() {
                            self.check
                                .start(cx.svc.call(|c| async move { c.engine_check().await }));
                        }
                    });
                    if self.check.loading() {
                        ui::busy(ui, tr("procurando…"));
                    }
                    if let Some(c) = &self.check.data {
                        if ui::b(c, "found") {
                            ui.colored_label(
                                Tone::Success.color(ui.visuals().dark_mode),
                                format!(
                                    "✓ {} · {}",
                                    ui::s(c, "path"),
                                    c.get("version")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or(tr("versão desconhecida"))
                                ),
                            );
                        } else {
                            ui::error_text(ui, &ui::s(c, "error"));
                        }
                    }
                },
            );
            ui::field(
                ui,
                tr("Modelo"),
                Some(tr("Vazio = padrão do CLI. Ex.: sonnet, opus.")),
                |ui| ui::optional_text(ui, &mut e.model, tr("padrão")),
            );
            ui::field(
                ui,
                tr("Modo de permissão"),
                Some(tr("acceptEdits com allowlist é o recomendado.")),
                |ui| {
                    let mut v = match e.permission_mode {
                        PermissionMode::AcceptEdits => "acceptEdits".to_string(),
                        PermissionMode::BypassPermissions => "bypassPermissions".to_string(),
                    };
                    if ui::select(
                        ui,
                        "perm",
                        &mut v,
                        &[
                            (
                                "acceptEdits",
                                tr("acceptEdits (recomendado, com allowlist)"),
                            ),
                            ("bypassPermissions", tr("bypassPermissions (perigoso)")),
                        ],
                    ) {
                        if v == "bypassPermissions" {
                            self.confirm_bypass = true;
                        } else {
                            e.permission_mode = PermissionMode::AcceptEdits;
                        }
                    }
                },
            );
            ui::field(
                ui,
                tr("Ferramentas permitidas"),
                Some(tr(
                    "Uma por linha (modo acceptEdits). Padrões como Bash(npm *) são aceitos.",
                )),
                |ui| {
                    let mut buf = self
                        .allowed_buf
                        .take()
                        .unwrap_or_else(|| e.allowed_tools.join("\n"));
                    if ui::lines_editor(ui, &mut buf, 8).changed() {
                        e.allowed_tools = ui::parse_lines(&buf);
                    }
                    self.allowed_buf = Some(buf);
                },
            );
            ui::field(
                ui,
                tr("Processos vivos (máx.)"),
                Some(tr(
                    "Acima disso o menos usado é aposentado (volta com --resume).",
                )),
                |ui| {
                    ui::number(ui, &mut e.max_live_processes, 1..=20);
                },
            );
            ui::field(
                ui,
                tr("Ociosidade (min)"),
                Some(tr("Processo sem mensagens é encerrado após esse tempo.")),
                |ui| {
                    ui::number(ui, &mut e.idle_timeout_minutes, 1..=600);
                },
            );
            ui::field(ui, tr("Timeout do turno (min)"), None, |ui| {
                ui::number(ui, &mut e.turn_timeout_minutes, 1..=600);
            });
            ui::field(
                ui,
                tr("Orçamento por turno (USD)"),
                Some(tr("Vazio = sem limite.")),
                |ui| ui::optional_number(ui, &mut e.max_budget_usd, tr("sem limite")),
            );
            ui::field(ui, tr("Effort"), None, |ui| {
                let mut v = e.effort.clone().unwrap_or_default();
                if ui::select(
                    ui,
                    "effort",
                    &mut v,
                    &[
                        ("", tr("padrão")),
                        ("low", "low"),
                        ("medium", "medium"),
                        ("high", "high"),
                        ("max", "max"),
                    ],
                ) {
                    e.effort = if v.is_empty() { None } else { Some(v) };
                }
            });
            ui::field(
                ui,
                tr("Mensagens parciais"),
                Some(tr("Streaming token a token na UI (mais eventos).")),
                |ui| {
                    ui::toggle(ui, &mut e.include_partial_messages);
                },
            );
            ui::field(
                ui,
                tr("Herdar MCPs globais do Claude"),
                Some(tr(
                    "Desligado = --strict-mcp-config (só o .mcp.json da KB).",
                )),
                |ui| {
                    ui::toggle(ui, &mut e.inherit_global_mcps);
                },
            );
            ui::field(
                ui,
                tr("Gravar stream bruto"),
                Some(tr("claude-<data>.log na pasta de logs (debug).")),
                |ui| {
                    ui::toggle(ui, &mut e.log_raw_stream);
                },
            );
            ui::field(ui, tr("System prompt adicional"), None, |ui| {
                ui::text_area(ui, &mut e.append_system_prompt, 5);
            });
        });
        // ---- resumo das threads ----
        ui::section_help(
            ui,
            tr("Resumo das threads"),
            Some(&help::ENGINE_THREAD_SUMMARY),
            |ui| {
                let ts = &mut st.draft.engine.thread_summary;
                ui.label(
                egui::RichText::new(tr(
                    "A cada N turnos concluídos, um modelo de chat da OpenAI resume o que está sendo conversado em uma ou duas frases, mostradas sob o título na lista de threads e no cabeçalho. Usa a chave da OpenAI do módulo de voz.",
                ))
                .small()
                .weak(),
            );
                ui.add_space(4.0);
                ui::field(ui, tr("Habilitar"), None, |ui| {
                    ui::toggle(ui, &mut ts.enabled);
                });
                ui::field(
                ui,
                tr("A cada quantos turnos"),
                Some(tr("Turnos concluídos entre um resumo e o próximo; os mesmos N turnos entram no resumo.")),
                |ui| {
                    ui::number(ui, &mut ts.every_turns, 1..=50);
                },
            );
                ui::field(ui, tr("Modelo"), None, |ui| {
                    ui::text_input(ui, &mut ts.model, "gpt-4o-mini");
                });
                ui::field(
                ui,
                tr("Prompt do resumo"),
                Some(tr("Instruções de sistema; os últimos turnos vão como mensagem do usuário no formato Usuário:/Assistente:.")),
                |ui| {
                    ui::text_area(ui, &mut ts.prompt, 7);
                    let is_default = ts.prompt.trim()
                        == will_core::settings::default_thread_summary_prompt().trim();
                    if ui
                        .add_enabled(!is_default, egui::Button::new(tr("Restaurar padrão")))
                        .clicked()
                    {
                        ts.prompt = will_core::settings::default_thread_summary_prompt().into();
                    }
                },
            );
                ui::field(
                    ui,
                    tr("Tamanho máximo (caracteres)"),
                    Some(tr("O resumo é cortado em palavra inteira com reticências.")),
                    |ui| {
                        ui::number(ui, &mut ts.max_chars, 40..=2000);
                    },
                );
            },
        );
        st.save_bar(ui, cx.svc, "engine");

        if self.confirm_bypass {
            let c = ui::Confirm {
                title: tr("Ativar bypassPermissions").into(),
                body: tr("Neste modo o Claude executa qualquer comando sem confirmação (--dangerously-skip-permissions). Use só numa KB isolada e com ferramentas MCP de envio em 'confirmar'.").into(),
                confirm_label: tr("Entendo, ativar").into(),
                danger: true,
                tag: "bypass".into(),
            };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_bypass = false;
                if ok {
                    st.draft.engine.permission_mode = PermissionMode::BypassPermissions;
                }
            }
        }
    }
}
