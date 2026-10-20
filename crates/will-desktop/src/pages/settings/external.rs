//! Seção MCPs externos: catálogo de servidores de terceiros que o serviço
//! escreve (junto com o `will_ia`) no `.mcp.json` da base de conhecimento.
//! Segredos de env/headers ficam no cofre e entram como `${WILL_MCP_<NOME>_<CHAVE>}`.

use super::SettingsState;
use crate::pages::{Cx, Nav, SettingsSection};
use crate::service::{j, Loader, Task};
use crate::ui::{self, Tone};
use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Map, Value};
use will_core::settings::mcp_secret_env_name;
use will_core::Event;
use will_server::api::mcp as api;

/// Linha chave/valor de `env` ou `headers` no editor.
#[derive(Default, Clone)]
struct KvRow {
    key: String,
    value: String,
    secret: bool,
    /// Referência do segredo já salvo (`mcp:<nome>:<chave>`), quando existir.
    existing_ref: Option<String>,
}

impl KvRow {
    fn from_entry(key: &str, v: &Value) -> Self {
        match v.get("$secret").and_then(|s| s.as_str()) {
            Some(r) => Self {
                key: key.to_string(),
                value: String::new(),
                secret: true,
                existing_ref: Some(r.to_string()),
            },
            None => Self {
                key: key.to_string(),
                value: v
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| v.to_string()),
                secret: false,
                existing_ref: None,
            },
        }
    }

    /// Valor no formato que `external_put` espera (ou `None` para descartar a linha).
    fn to_json(&self) -> Option<Value> {
        if self.key.trim().is_empty() {
            return None;
        }
        if !self.secret {
            return Some(Value::String(self.value.clone()));
        }
        if !self.value.is_empty() {
            return Some(json!({ "$secret_value": self.value }));
        }
        self.existing_ref.as_ref().map(|r| json!({ "$secret": r }))
    }
}

#[derive(Default)]
struct Editor {
    is_new: bool,
    name: String,
    kind: String,
    command: String,
    args_buf: String,
    cwd: String,
    url: String,
    env: Vec<KvRow>,
    headers: Vec<KvRow>,
    auto_allow_tools: bool,
    allowed_buf: String,
    enabled: bool,
}

impl Editor {
    fn new_server() -> Self {
        Self {
            is_new: true,
            kind: "stdio".into(),
            auto_allow_tools: true,
            enabled: true,
            ..Default::default()
        }
    }

    fn from_value(v: &Value) -> Self {
        let rows = |section: &str| -> Vec<KvRow> {
            v.get(section)
                .and_then(|m| m.as_object())
                .map(|m| m.iter().map(|(k, val)| KvRow::from_entry(k, val)).collect())
                .unwrap_or_default()
        };
        let lines = |key: &str| -> String {
            v.get(key)
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        };
        Self {
            is_new: false,
            name: ui::s(v, "name"),
            kind: {
                let k = ui::s(v, "kind");
                if k.is_empty() {
                    "stdio".into()
                } else {
                    k
                }
            },
            command: ui::s(v, "command"),
            args_buf: lines("args"),
            cwd: ui::s(v, "cwd"),
            url: ui::s(v, "url"),
            env: rows("env"),
            headers: rows("headers"),
            auto_allow_tools: v
                .get("auto_allow_tools")
                .and_then(|b| b.as_bool())
                .unwrap_or(true),
            allowed_buf: lines("allowed_tools"),
            enabled: v.get("enabled").and_then(|b| b.as_bool()).unwrap_or(true),
        }
    }

    fn name_valid(&self) -> bool {
        let n = self.name.trim();
        !n.is_empty()
            && n.len() <= 64
            && n.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            && n != "will_ia"
    }

    fn can_save(&self) -> bool {
        if !self.name_valid() {
            return false;
        }
        match self.kind.as_str() {
            "stdio" => !self.command.trim().is_empty(),
            _ => !self.url.trim().is_empty(),
        }
    }

    fn to_json(&self) -> Value {
        let kv = |rows: &[KvRow]| -> Value {
            Value::Object(
                rows.iter()
                    .filter_map(|r| r.to_json().map(|v| (r.key.trim().to_string(), v)))
                    .collect::<Map<_, _>>(),
            )
        };
        let opt = |s: &str| -> Value {
            if s.trim().is_empty() {
                Value::Null
            } else {
                Value::String(s.trim().to_string())
            }
        };
        let allowed = ui::parse_lines(&self.allowed_buf);
        let stdio = self.kind == "stdio";
        json!({
            "name": self.name.trim(),
            "kind": self.kind,
            "command": if stdio { opt(&self.command) } else { Value::Null },
            "args": if stdio { ui::parse_lines(&self.args_buf) } else { vec![] },
            "cwd": if stdio { opt(&self.cwd) } else { Value::Null },
            "env": if stdio { kv(&self.env) } else { json!({}) },
            "url": if stdio { Value::Null } else { opt(&self.url) },
            "headers": if stdio { json!({}) } else { kv(&self.headers) },
            "enabled": self.enabled,
            "auto_allow_tools": self.auto_allow_tools,
            "allowed_tools": if allowed.is_empty() { Value::Null } else { json!(allowed) },
        })
    }
}

#[derive(Default)]
pub struct ExternalPage {
    list: Loader<Value>,
    editor: Option<Editor>,
    test_task: Option<(String, Task<Value>)>,
    save_task: Option<(String, Task<Value>)>,
    toggle_task: Option<(String, Task<Value>)>,
    delete_task: Option<(String, Task<Value>)>,
    import_task: Option<Task<Value>>,
    confirm_delete: Option<String>,
    show_import_json: bool,
    import_json_buf: String,
}

impl ExternalPage {
    pub fn on_event(&mut self, ev: &Event) {
        if let Event::SettingsChanged { section } = ev {
            if section == "external_mcps" {
                self.list.loaded_at = None;
            }
        }
    }

    fn reload(&mut self, cx: &Cx) {
        self.list.start(
            cx.svc
                .call(|s| async move { j(api::external_list(State(s)).await) }),
        );
    }

    fn poll(&mut self, cx: &mut Cx) {
        if self.list.stale(std::time::Duration::from_secs(120)) {
            self.reload(cx);
        }
        self.list.poll();

        if let Some((name, t)) = &self.test_task {
            if let Some(r) = t.take() {
                let name = name.clone();
                self.test_task = None;
                match r {
                    Ok(v) => {
                        if ui::b(&v, "ok") {
                            let n = v
                                .get("tools")
                                .and_then(|t| t.as_array())
                                .map(|a| a.len())
                                .unwrap_or(0);
                            cx.toasts
                                .success(format!("'{name}' respondeu: {n} ferramenta(s)."));
                        } else {
                            cx.toasts
                                .error(format!("'{name}' falhou: {}", ui::s(&v, "error")));
                        }
                        self.reload(cx);
                    }
                    Err(e) => cx.toasts.error(format!("Teste de '{name}': {e}")),
                }
            }
        }
        if let Some((name, t)) = &self.save_task {
            if let Some(r) = t.take() {
                let name = name.clone();
                self.save_task = None;
                match r {
                    Ok(_) => {
                        cx.toasts
                            .success(format!("Servidor '{name}' salvo; .mcp.json regravado."));
                        self.editor = None;
                        self.reload(cx);
                    }
                    Err(e) => cx.toasts.error(format!("Não salvou '{name}': {e}")),
                }
            }
        }
        if let Some((name, t)) = &self.toggle_task {
            if let Some(r) = t.take() {
                let name = name.clone();
                self.toggle_task = None;
                match r {
                    Ok(_) => self.reload(cx),
                    Err(e) => cx.toasts.error(format!("Não alterou '{name}': {e}")),
                }
            }
        }
        if let Some((name, t)) = &self.delete_task {
            if let Some(r) = t.take() {
                let name = name.clone();
                self.delete_task = None;
                match r {
                    Ok(_) => {
                        cx.toasts.success(format!("Servidor '{name}' removido."));
                        if self.editor.as_ref().is_some_and(|e| e.name == name) {
                            self.editor = None;
                        }
                        self.reload(cx);
                    }
                    Err(e) => cx.toasts.error(format!("Não removeu '{name}': {e}")),
                }
            }
        }
        if let Some(t) = &self.import_task {
            if let Some(r) = t.take() {
                self.import_task = None;
                match r {
                    Ok(v) => {
                        let count = ui::i(&v, "count");
                        let found = v.get("found").and_then(|f| f.as_i64());
                        let names: Vec<String> = v
                            .get("imported")
                            .and_then(|a| a.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let msg = match found {
                            Some(f) => format!("Importados {count} de {f} servidor(es)."),
                            None => format!("Importados {count} servidor(es)."),
                        };
                        if names.is_empty() {
                            cx.toasts.info(msg);
                        } else {
                            cx.toasts.success(format!("{msg} {}", names.join(", ")));
                        }
                        self.show_import_json = false;
                        self.import_json_buf.clear();
                        self.reload(cx);
                    }
                    Err(e) => cx.toasts.error(format!("Importação: {e}")),
                }
            }
        }
    }

    fn put(&mut self, cx: &Cx, name: String, body: Value, toggle: bool) {
        let n = name.clone();
        let t = cx.svc.call(move |s| async move {
            j(api::external_put(State(s), Path(n), Json(body)).await)
        });
        if toggle {
            self.toggle_task = Some((name, t));
        } else {
            self.save_task = Some((name, t));
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, _st: &mut SettingsState) {
        self.poll(cx);
        ui::heading(ui, "MCPs externos", Some("Os servidores daqui + o will_ia são escritos em <kb>/.mcp.json; segredos entram como ${WILL_MCP_<NOME>_<CHAVE>} injetados no processo, nunca em texto puro."));

        // ---- barra de ações ----
        ui.horizontal_wrapped(|ui| {
            if ui::primary_button(ui, "Adicionar").clicked() {
                self.editor = Some(Editor::new_server());
            }
            let importing = self.import_task.is_some();
            if ui.add_enabled(!importing, egui::Button::new("Importar do Claude (~/.claude.json)")).on_hover_text("Lê os servidores do escopo de usuário do Claude Code e traz para o catálogo.").clicked() {
                self.import_task = Some(cx.svc.call(|s| async move { j(api::external_import_claude(State(s)).await) }));
            }
            if ui.add_enabled(!importing, egui::Button::new(if self.show_import_json { "Fechar JSON" } else { "Importar JSON" })).clicked() {
                self.show_import_json = !self.show_import_json;
            }
            if ui.button("Ver .mcp.json").on_hover_text("Abre a seção Base de conhecimento, onde o arquivo gerado pode ser inspecionado.").clicked() {
                *cx.go = Some(Nav::Settings(SettingsSection::Kb));
            }
            if ui.button("Atualizar").clicked() {
                self.list.loaded_at = None;
            }
            if importing || self.list.loading() {
                ui.spinner();
            }
        });

        if self.show_import_json {
            ui::section(ui, "Importar JSON", |ui| {
                ui.label(egui::RichText::new("Cole um bloco {\"mcpServers\": {...}} (o formato que os READMEs publicam). Valores literais em env/headers são importados como texto; troque por segredos no editor depois.").weak().small());
                ui::lines_editor(ui, &mut self.import_json_buf, 8);
                ui.horizontal(|ui| {
                    let ok = !self.import_json_buf.trim().is_empty() && self.import_task.is_none();
                    if ui.add_enabled(ok, egui::Button::new("Importar")).clicked() {
                        let text = self.import_json_buf.clone();
                        self.import_task = Some(cx.svc.call(move |s| async move {
                            j(api::external_import_json(
                                State(s),
                                Json(api::ImportJson {
                                    json: Value::String(text),
                                }),
                            )
                            .await)
                        }));
                    }
                    if ui.button("Cancelar").clicked() {
                        self.show_import_json = false;
                    }
                });
            });
        }

        // ---- editor ----
        if self.editor.is_some() {
            self.editor_ui(ui, cx);
        }

        // ---- lista ----
        if let Some(e) = &self.list.error {
            ui::error_text(ui, e);
        }
        let servers = self
            .list
            .data
            .as_ref()
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if servers.is_empty() && !self.list.loading() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Nenhum MCP externo cadastrado. Adicione um, importe do Claude Code ou cole o JSON de um README.").weak());
        }
        for srv in &servers {
            self.server_card(ui, cx, srv);
        }

        // ---- confirmação de remoção ----
        if let Some(name) = self.confirm_delete.clone() {
            let c = ui::Confirm { title: "Remover servidor".into(), body: format!("Remover '{name}' do catálogo? Ele sai do .mcp.json da base de conhecimento; os segredos associados ficam no cofre."), confirm_label: "Remover".into(), danger: true, tag: "del-ext-mcp".into() };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_delete = None;
                if ok {
                    let n = name.clone();
                    self.delete_task = Some((
                        name,
                        cx.svc.call(move |s| async move {
                            j(api::external_delete(State(s), Path(n)).await)
                        }),
                    ));
                }
            }
        }
    }

    fn server_card(&mut self, ui: &mut egui::Ui, cx: &Cx, srv: &Value) {
        let name = ui::s(srv, "name");
        let kind = ui::s(srv, "kind");
        let enabled = ui::b(srv, "enabled");
        ui::section(ui, &name, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui::badge(
                    ui,
                    if kind.is_empty() { "stdio" } else { &kind },
                    Tone::Info,
                );
                if enabled {
                    ui::badge(ui, "habilitado", Tone::Success);
                } else {
                    ui::badge(ui, "desabilitado", Tone::Neutral);
                }
                if !ui::b(srv, "auto_allow_tools") {
                    ui::badge(ui, "ferramentas sob aprovação do Claude", Tone::Warning);
                }
                if let Some(allowed) = srv.get("allowed_tools").and_then(|a| a.as_array()) {
                    ui::badge(
                        ui,
                        &format!("{} ferramenta(s) permitida(s)", allowed.len()),
                        Tone::Neutral,
                    );
                }
            });
            // comando/url
            if kind == "stdio" || kind.is_empty() {
                let mut cmd = ui::s(srv, "command");
                if let Some(args) = srv.get("args").and_then(|a| a.as_array()) {
                    for a in args.iter().filter_map(|x| x.as_str()) {
                        cmd.push(' ');
                        cmd.push_str(a);
                    }
                }
                ui::mono(ui, &cmd);
                let cwd = ui::s(srv, "cwd");
                if !cwd.is_empty() {
                    ui.label(egui::RichText::new(format!("cwd: {cwd}")).weak().small());
                }
            } else {
                ui::mono(ui, &ui::s(srv, "url"));
            }
            // segredos referenciados
            let secret_keys: Vec<String> = ["env", "headers"]
                .iter()
                .flat_map(|sec| {
                    srv.get(*sec)
                        .and_then(|m| m.as_object())
                        .into_iter()
                        .flat_map(|m| {
                            m.iter()
                                .filter(|(_, v)| v.get("$secret").is_some())
                                .map(|(k, _)| k.clone())
                                .collect::<Vec<_>>()
                        })
                })
                .collect();
            if !secret_keys.is_empty() {
                let vars: Vec<String> = secret_keys
                    .iter()
                    .map(|k| format!("${{{}}}", mcp_secret_env_name(&name, k)))
                    .collect();
                ui.label(
                    egui::RichText::new(format!("segredos: {}", vars.join(", ")))
                        .weak()
                        .small(),
                );
            }
            // último teste
            ui.horizontal_wrapped(|ui| match srv.get("last_test").filter(|t| !t.is_null()) {
                Some(t) => {
                    let at = ui::fmt_dt(&t["at"]);
                    if ui::b(t, "ok") {
                        let n = t
                            .get("tools")
                            .and_then(|a| a.as_array())
                            .map(|a| a.len())
                            .unwrap_or(0);
                        ui::badge(ui, &format!("ok · {n} ferramenta(s)"), Tone::Success);
                        let sn = ui::s(t, "server_name");
                        if !sn.is_empty() {
                            ui.label(egui::RichText::new(sn).small());
                        }
                    } else {
                        ui::badge(ui, "erro", Tone::Danger);
                        let err = ui::s(t, "error");
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&err)
                                    .small()
                                    .color(Tone::Danger.color(ui.visuals().dark_mode)),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&err);
                    }
                    if !at.is_empty() {
                        ui.label(
                            egui::RichText::new(format!("testado em {at}"))
                                .weak()
                                .small(),
                        );
                    }
                    if let Some(tools) = t
                        .get("tools")
                        .and_then(|a| a.as_array())
                        .filter(|a| !a.is_empty())
                    {
                        let names: Vec<&str> = tools.iter().filter_map(|x| x.as_str()).collect();
                        ui.label(egui::RichText::new("ferramentas").weak().small())
                            .on_hover_text(names.join("\n"));
                    }
                }
                None => ui::badge(ui, "não testado", Tone::Neutral),
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let testing = self.test_task.as_ref().is_some_and(|(n, _)| n == &name);
                let toggling = self.toggle_task.as_ref().is_some_and(|(n, _)| n == &name);
                let deleting = self.delete_task.as_ref().is_some_and(|(n, _)| n == &name);
                let busy = testing || toggling || deleting;
                if ui.add_enabled(!busy, egui::Button::new("Testar")).on_hover_text("Sobe o servidor (ou conecta na URL), lista as ferramentas e encerra. Até 45 s.").clicked() {
                    let n = name.clone();
                    self.test_task = Some((name.clone(), cx.svc.call(move |s| async move { j(api::external_test(State(s), Path(n)).await) })));
                }
                if testing {
                    ui.spinner();
                    ui.label(egui::RichText::new("testando…").weak().small());
                }
                if ui.add_enabled(!busy, egui::Button::new("Editar")).clicked() {
                    self.editor = Some(Editor::from_value(srv));
                }
                if ui.add_enabled(!busy, egui::Button::new("Remover")).clicked() {
                    self.confirm_delete = Some(name.clone());
                }
                ui.add_space(12.0);
                ui.label("Habilitado");
                let mut on = enabled;
                ui.add_enabled_ui(!busy, |ui| {
                    if ui::toggle(ui, &mut on).changed() {
                        let mut body = srv.clone();
                        if let Some(o) = body.as_object_mut() {
                            o.insert("enabled".into(), Value::Bool(on));
                        }
                        self.put(cx, name.clone(), body, true);
                    }
                });
                if toggling || deleting {
                    ui.spinner();
                }
            });
        });
    }

    fn editor_ui(&mut self, ui: &mut egui::Ui, cx: &Cx) {
        let Some(ed) = self.editor.as_mut() else {
            return;
        };
        let title = if ed.is_new {
            "Novo servidor".to_string()
        } else {
            format!("Editar {}", ed.name)
        };
        let saving = self.save_task.is_some();
        let mut action: Option<bool> = None; // Some(true) = salvar, Some(false) = cancelar
        ui::section(ui, &title, |ui| {
            ui::field(ui, "Nome", Some("Letras, números, _ e -. Vira a chave em mcpServers e o prefixo das variáveis de segredo."), |ui| {
                ui.add_enabled_ui(ed.is_new, |ui| {
                    ui::text_input(ui, &mut ed.name, "ex.: github");
                });
                if ed.is_new && !ed.name.is_empty() && !ed.name_valid() {
                    ui::error_text(ui, "nome inválido (ou reservado)");
                }
            });
            ui::field(
                ui,
                "Tipo",
                Some("stdio = processo local; http = Streamable HTTP; sse = legado."),
                |ui| {
                    ui::select(
                        ui,
                        "ext-kind",
                        &mut ed.kind,
                        &[
                            ("stdio", "stdio (processo local)"),
                            ("http", "http (Streamable HTTP)"),
                            ("sse", "sse (legado)"),
                        ],
                    );
                },
            );
            if ed.kind == "stdio" {
                ui::field(
                    ui,
                    "Comando",
                    Some("Executável no PATH ou caminho absoluto. Ex.: npx, uvx, node."),
                    |ui| {
                        ui::text_input(ui, &mut ed.command, "npx");
                    },
                );
                ui::field(
                    ui,
                    "Argumentos",
                    Some("Um por linha. Ex.: -y / @modelcontextprotocol/server-github"),
                    |ui| {
                        ui::lines_editor(ui, &mut ed.args_buf, 3);
                    },
                );
                ui::field(ui, "Pasta de trabalho (cwd)", Some("Opcional."), |ui| {
                    ui::text_input(ui, &mut ed.cwd, "opcional");
                });
                ui.label(egui::RichText::new("Variáveis de ambiente").strong());
                ui.label(egui::RichText::new("Marque 'segredo' para tokens: o valor vai para o cofre e o processo recebe ${WILL_MCP_<NOME>_<CHAVE>}.").weak().small());
                kv_editor(ui, "ext-env", &ed.name, &mut ed.env);
            } else {
                ui::field(ui, "URL", Some("Endpoint do servidor remoto."), |ui| {
                    ui::text_input(ui, &mut ed.url, "https://mcp.exemplo.com/mcp");
                });
                ui.label(egui::RichText::new("Headers HTTP").strong());
                ui.label(egui::RichText::new("Ex.: Authorization = Bearer <token>. Marque 'segredo' para o valor ficar no cofre.").weak().small());
                kv_editor(ui, "ext-headers", &ed.name, &mut ed.headers);
            }
            ui.add_space(6.0);
            ui::field(ui, "Liberar ferramentas automaticamente", Some("Entra na allowlist do Claude Code (mcp__<nome>__*); desligado, o Claude pede aprovação a cada uso."), |ui| {
                ui::toggle(ui, &mut ed.auto_allow_tools);
            });
            ui::field(
                ui,
                "Ferramentas permitidas",
                Some("Uma por linha; vazio = todas as ferramentas do servidor."),
                |ui| {
                    ui::lines_editor(ui, &mut ed.allowed_buf, 3);
                },
            );
            ui::field(
                ui,
                "Habilitado",
                Some("Desabilitado não entra no .mcp.json."),
                |ui| {
                    ui::toggle(ui, &mut ed.enabled);
                },
            );
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        ed.can_save() && !saving,
                        egui::Button::new(
                            egui::RichText::new("Salvar").color(egui::Color32::WHITE),
                        )
                        .fill(Tone::Info.color(true)),
                    )
                    .clicked()
                {
                    action = Some(true);
                }
                if ui
                    .add_enabled(!saving, egui::Button::new("Cancelar"))
                    .clicked()
                {
                    action = Some(false);
                }
                if saving {
                    ui.spinner();
                }
            });
        });
        match action {
            Some(true) => {
                let name = ed.name.trim().to_string();
                let body = ed.to_json();
                self.put(cx, name, body, false);
            }
            Some(false) => self.editor = None,
            None => {}
        }
    }
}

/// Editor de linhas chave/valor/segredo.
fn kv_editor(ui: &mut egui::Ui, id: &str, server: &str, rows: &mut Vec<KvRow>) {
    let mut remove: Option<usize> = None;
    egui::Grid::new(id)
        .num_columns(5)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            if !rows.is_empty() {
                ui.label(egui::RichText::new("Chave").weak().small());
                ui.label(egui::RichText::new("Valor").weak().small());
                ui.label(egui::RichText::new("Segredo").weak().small());
                ui.label("");
                ui.label("");
                ui.end_row();
            }
            for (i, r) in rows.iter_mut().enumerate() {
                ui.add(
                    egui::TextEdit::singleline(&mut r.key)
                        .hint_text("CHAVE")
                        .desired_width(180.0),
                );
                let hint = if r.secret && r.existing_ref.is_some() {
                    "salvo no cofre · digite para substituir"
                } else if r.secret {
                    "valor (vai para o cofre)"
                } else {
                    "valor"
                };
                ui.add(
                    egui::TextEdit::singleline(&mut r.value)
                        .password(r.secret)
                        .hint_text(hint)
                        .desired_width(260.0),
                );
                ui.checkbox(&mut r.secret, "");
                if r.secret && !r.key.trim().is_empty() && !server.trim().is_empty() {
                    ui.label(
                        egui::RichText::new(format!(
                            "${{{}}}",
                            mcp_secret_env_name(server, r.key.trim())
                        ))
                        .monospace()
                        .small()
                        .weak(),
                    );
                } else {
                    ui.label("");
                }
                if ui.button("remover").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
    if let Some(i) = remove {
        rows.remove(i);
    }
    ui.horizontal(|ui| {
        if ui.button("+ variável").clicked() {
            rows.push(KvRow::default());
        }
        if ui.button("+ segredo").clicked() {
            rows.push(KvRow {
                secret: true,
                ..Default::default()
            });
        }
    });
    ui.add_space(4.0);
}
