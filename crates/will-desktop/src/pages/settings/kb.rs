//! Seção Base de conhecimento: pasta (diálogo nativo), status, esqueleto, .mcp.json.

use super::SettingsState;
use crate::help;
use crate::i18n::{tr, trf};
use crate::pages::Cx;
use crate::service::{j, Loader, Task};
use crate::ui;
use axum::extract::State;
use serde_json::Value;
use will_server::api::kb as api;

#[derive(Default)]
pub struct KbPage {
    status: Loader<Value>,
    mcp_json: Loader<Value>,
    show_mcp_json: bool,
    scaffold: Option<Task<Value>>,
    path_buf: Option<String>,
}

impl KbPage {
    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        if self.status.stale(std::time::Duration::from_secs(20)) {
            self.status.start(
                cx.svc
                    .call(|s| async move { j(api::status(State(s)).await) }),
            );
        }
        self.status.poll();
        if self.mcp_json.poll() {
            self.show_mcp_json = true;
        }
        if let Some(t) = &self.scaffold {
            if let Some(r) = t.take() {
                self.scaffold = None;
                match r {
                    Ok(v) => {
                        let created: Vec<String> = v
                            .get("created")
                            .and_then(|c| c.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        cx.toasts.success(if created.is_empty() {
                            tr("Estrutura já existia; nada criado.").to_string()
                        } else {
                            trf("Criados: {}", &[&created.join(", ")])
                        });
                        self.status.loaded_at = None;
                    }
                    Err(e) => cx.toasts.error(e),
                }
            }
        }

        ui::heading_help(ui, tr("Base de conhecimento"), Some(tr("Pasta onde o Claude Code roda (CLAUDE.md, agentes, tools/, .mcp.json). O serviço só cria o esqueleto; não roda git nela.")), &help::KB);

        ui::section(ui, tr("Pasta"), |ui| {
            let kb = &mut st.draft.kb;
            let mut path = self
                .path_buf
                .take()
                .unwrap_or_else(|| kb.path.clone().unwrap_or_default());
            ui::field(
                ui,
                tr("Caminho"),
                Some(if cx.snap.portable_root.is_some() {
                    tr("Absoluto, ou relativo à raiz do pendrive (ex.: workspace). Mudar derruba processos vivos.")
                } else {
                    tr("Caminho absoluto. Mudar derruba todos os processos vivos.")
                }),
                |ui| {
                    ui.horizontal(|ui| {
                        let w = (ui.available_width() - 200.0).max(200.0);
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut path)
                                    .desired_width(w)
                                    .hint_text("/home/you/will-kb"),
                            )
                            .changed()
                        {
                            kb.path = if path.trim().is_empty() {
                                None
                            } else {
                                Some(path.trim().to_string())
                            };
                        }
                        if ui.button(tr("Escolher…")).clicked() {
                            let mut dlg = rfd::FileDialog::new()
                                .set_title(tr("Pasta da base de conhecimento"));
                            if let Some(p) = kb
                                .path
                                .as_ref()
                                .map(std::path::PathBuf::from)
                                .filter(|p| p.is_dir())
                            {
                                dlg = dlg.set_directory(p);
                            }
                            if let Some(p) = dlg.pick_folder() {
                                path = p.display().to_string();
                                kb.path = Some(path.clone());
                            }
                        }
                        if cx.snap.portable_root.is_some()
                            && ui.button(tr("usar workspace/ do pendrive")).clicked()
                        {
                            path = "workspace".into();
                            kb.path = Some(path.clone());
                            if let Some(root) = &cx.snap.portable_root {
                                let _ = std::fs::create_dir_all(root.join("workspace"));
                            }
                        }
                    });
                },
            );
            self.path_buf = Some(path);
            st.save_bar(ui, cx.svc, "kb");
        });

        ui::section(ui, tr("Status"), |ui| {
            if let Some(e) = &self.status.error {
                ui::error_text(ui, e);
            }
            if let Some(s) = &self.status.data {
                let row = |ui: &mut egui::Ui, label: &str, ok: bool, text: &str| {
                    ui.horizontal(|ui| {
                        ui::dot(
                            ui,
                            if ok {
                                ui::Tone::Success
                            } else {
                                ui::Tone::Warning
                            },
                        );
                        ui.label(label);
                        ui.label(egui::RichText::new(text).weak());
                    });
                };
                let path = ui::s(s, "path");
                row(
                    ui,
                    tr("Pasta"),
                    ui::b(s, "exists"),
                    if path.is_empty() {
                        tr("não configurada")
                    } else {
                        &path
                    },
                );
                row(ui, tr("Gravável"), ui::b(s, "writable"), "");
                row(ui, "CLAUDE.md", ui::b(s, "has_claude_md"), "");
                row(
                    ui,
                    ".mcp.json",
                    ui::b(s, "has_mcp_json"),
                    tr("gerado pelo serviço"),
                );
                let node = s.get("node").and_then(|v| v.as_str());
                let py = s.get("python").and_then(|v| v.as_str());
                row(
                    ui,
                    "Node.js",
                    node.is_some(),
                    node.unwrap_or(tr("não encontrado (opcional, para scripts em tools/)")),
                );
                row(
                    ui,
                    "Python",
                    py.is_some(),
                    py.unwrap_or(tr("não encontrado (opcional)")),
                );
                if let Some(w) = s.get("warnings").and_then(|w| w.as_array()) {
                    for w in w {
                        if let Some(t) = w.as_str() {
                            ui.colored_label(
                                ui::Tone::Warning.color(ui.visuals().dark_mode),
                                format!("! {t}"),
                            );
                        }
                    }
                }
            } else {
                ui::busy(ui, tr("verificando…"));
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let has = st.saved.kb.path.is_some();
                if ui
                    .add_enabled(
                        has && self.scaffold.is_none(),
                        egui::Button::new(tr("Criar estrutura inicial")),
                    )
                    .clicked()
                {
                    self.scaffold = Some(
                        cx.svc
                            .call(|s| async move { j(api::scaffold(State(s)).await) }),
                    );
                }
                if ui
                    .add_enabled(has, egui::Button::new(tr("Abrir pasta")))
                    .clicked()
                {
                    let svc = cx.svc.clone();
                    let t: Task<Value> = svc.call(|s| async move { j(api::open(State(s)).await) });
                    drop(t);
                }
                if ui
                    .add_enabled(has, egui::Button::new(tr("Ver .mcp.json")))
                    .clicked()
                {
                    self.mcp_json.start(
                        cx.svc
                            .call(|s| async move { j(api::mcp_json(State(s)).await) }),
                    );
                }
                if ui.button(tr("Atualizar")).clicked() {
                    self.status.loaded_at = None;
                }
            });
        });

        if self.show_mcp_json {
            if let Some(v) = &self.mcp_json.data {
                ui::section(ui, tr(".mcp.json gerado"), |ui| {
                    let path = ui::s(v, "path");
                    if !path.is_empty() {
                        ui::mono(ui, &path);
                    }
                    let text = v
                        .get("text")
                        .and_then(|t| t.as_str())
                        .map(String::from)
                        .unwrap_or_else(|| {
                            serde_json::to_string_pretty(v.get("json").unwrap_or(&Value::Null))
                                .unwrap_or_default()
                        });
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            ui::mono(ui, &text);
                        });
                    if ui.button(tr("fechar")).clicked() {
                        self.show_mcp_json = false;
                    }
                });
            }
            if let Some(e) = &self.mcp_json.error {
                ui::error_text(ui, e);
            }
        }
    }
}
