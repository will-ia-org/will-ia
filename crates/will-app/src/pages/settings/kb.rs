//! Seção Base de conhecimento: pasta (diálogo nativo em loopback; navegação
//! pelo servidor via `GET /api/kb/browse` quando remoto), status, esqueleto,
//! .mcp.json.

use super::SettingsState;
use crate::help;
use crate::i18n::{tr, trf};
use crate::pages::Cx;
use crate::service::{Loader, Task};
use crate::ui;
use serde_json::Value;

#[derive(Default)]
pub struct KbPage {
    status: Loader<Value>,
    mcp_json: Loader<Value>,
    show_mcp_json: bool,
    scaffold: Option<Task<Value>>,
    path_buf: Option<String>,
    /// Modal "Pasta no servidor" (só quando o servidor não é esta máquina).
    browse_open: bool,
    /// Último `GET /api/kb/browse`: `{path, parent, home, writable, dangerous, dirs}`.
    browse: Loader<Value>,
    /// Caminho digitado no modal (campo "Ir").
    browse_input: String,
}

impl KbPage {
    /// Pede ao servidor a listagem de `path` (`None` = home dele).
    fn browse_to(&mut self, cx: &Cx, path: Option<String>) {
        self.browse.start(
            cx.svc
                .call(move |c| async move { c.kb_browse(path.as_deref()).await }),
        );
    }

    /// Modal "Pasta no servidor": navega com `kb_browse` e devolve o caminho
    /// escolhido (como o servidor o devolveu) em "Usar esta pasta". Fecha em
    /// Cancelar, Esc ou clique fora.
    fn browse_modal(&mut self, ctx: &egui::Context, cx: &Cx) -> Option<String> {
        let mut chosen: Option<String> = None;
        let mut close = false;
        // Navegação pedida neste quadro: `Some(None)` = home do servidor.
        let mut go: Option<Option<String>> = None;
        let dark = ctx.theme() == egui::Theme::Dark;
        let pal = ui::palette(dark);
        let warn = ui::Tone::Warning.color(dark);
        let data = self.browse.data.clone();
        let cur = data.as_ref().map(|d| ui::s(d, "path")).unwrap_or_default();
        let parent = data
            .as_ref()
            .and_then(|d| d.get("parent"))
            .and_then(|p| p.as_str())
            .map(String::from);
        let loading = self.browse.loading();
        let m = egui::Modal::new(egui::Id::unique("kb-browse")).show(ctx, |ui| {
            ui.set_width(540.0);
            ui.heading(tr("Pasta no servidor"));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(tr(
                    "O serviço roda em outra máquina: escolha a pasta da base de conhecimento no disco dela.",
                ))
                .small()
                .weak(),
            );
            ui.add_space(8.0);
            // caminho atual (editável) + Ir
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 56.0).max(200.0);
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.browse_input)
                        .desired_width(w)
                        .hint_text("/home/you/will-kb"),
                );
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.add_enabled(!loading, egui::Button::new(tr("Ir"))).clicked() || enter {
                    let typed = self.browse_input.trim().to_string();
                    go = Some(if typed.is_empty() { None } else { Some(typed) });
                }
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(!loading && parent.is_some(), egui::Button::new(tr("↑ pasta acima")))
                    .clicked()
                {
                    go = Some(parent.clone());
                }
                if ui.add_enabled(!loading, egui::Button::new(tr("Home"))).clicked() {
                    go = Some(None);
                }
                if loading {
                    ui.add(egui::Spinner::new().size(14.0));
                }
            });
            if let Some(e) = &self.browse.error {
                ui::error_text(ui, e);
            }
            if let Some(d) = &data {
                if ui::b(d, "dangerous") {
                    ui.colored_label(
                        warn,
                        format!("! {}", tr("Pasta do sistema ou raiz do disco: não use como base de conhecimento.")),
                    );
                }
                if !ui::b(d, "writable") {
                    ui.colored_label(
                        warn,
                        format!("! {}", tr("O serviço não consegue gravar nesta pasta.")),
                    );
                }
                ui.add_space(4.0);
                let dirs = d
                    .get("dirs")
                    .and_then(|x| x.as_array())
                    .cloned()
                    .unwrap_or_default();
                egui::Frame::new()
                    .fill(pal.card)
                    .stroke(egui::Stroke::new(1.0, pal.border))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(6, 4))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::vertical()
                            .id_salt("kb-browse-dirs")
                            .max_height(260.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 1.0;
                                if dirs.is_empty() {
                                    ui.label(egui::RichText::new(tr("nenhuma subpasta")).weak().small());
                                }
                                for dir in &dirs {
                                    let name = ui::s(dir, "name");
                                    let p = ui::s(dir, "path");
                                    let b = egui::Button::new(
                                        egui::RichText::new(&name).color(pal.text),
                                    )
                                    .frame_when_inactive(false)
                                    .corner_radius(6.0)
                                    .min_size(egui::vec2(ui.available_width(), 24.0));
                                    if ui
                                        .add_enabled(!loading, b)
                                        .on_hover_text(&p)
                                        .clicked()
                                    {
                                        go = Some(Some(p));
                                    }
                                }
                            });
                    });
            } else if loading {
                ui::busy(ui, tr("carregando…"));
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button(tr("Cancelar")).clicked() {
                    close = true;
                }
                let can = data.is_some() && !cur.is_empty() && !loading;
                let r = ui.add_enabled_ui(can, |ui| ui::primary_button(ui, tr("Usar esta pasta")));
                if r.inner.clicked() {
                    chosen = Some(cur.clone());
                }
                if !cur.is_empty() {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&cur).small().weak().monospace(),
                        )
                        .truncate(),
                    )
                    .on_hover_text(&cur);
                }
            });
        });
        if let Some(p) = go {
            self.browse_to(cx, p);
        }
        if m.should_close() {
            close = true;
        }
        if chosen.is_some() || close {
            self.browse_open = false;
        }
        chosen
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        if self.status.stale(std::time::Duration::from_secs(20)) {
            self.status
                .start(cx.svc.call(|c| async move { c.kb_status().await }));
        }
        self.status.poll();
        if self.mcp_json.poll() {
            self.show_mcp_json = true;
        }
        if self.browse.poll() {
            if let Some(d) = &self.browse.data {
                self.browse_input = ui::s(d, "path");
            }
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
                            if cx.snap.server_is_local {
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
                            } else {
                                // Remoto: o diálogo nativo mostraria o disco
                                // errado; navega pelas pastas do servidor.
                                let start = kb
                                    .path
                                    .clone()
                                    .map(|p| p.trim().to_string())
                                    .filter(|p| !p.is_empty());
                                self.browse_input = start.clone().unwrap_or_default();
                                self.browse_open = true;
                                self.browse_to(cx, start);
                            }
                        }
                        if cx.snap.portable_root.is_some()
                            && ui.button(tr("usar workspace/ do pendrive")).clicked()
                        {
                            path = "workspace".into();
                            kb.path = Some(path.clone());
                            if let Some(root) = cx
                                .snap
                                .portable_root
                                .as_ref()
                                .filter(|_| cx.snap.server_is_local)
                            {
                                let _ = std::fs::create_dir_all(root.join("workspace"));
                            }
                        }
                    });
                },
            );
            self.path_buf = Some(path);
            st.save_bar(ui, cx.svc, "kb");
        });

        if self.browse_open {
            if let Some(p) = self.browse_modal(ui.ctx(), cx) {
                st.draft.kb.path = Some(p.clone());
                self.path_buf = Some(p);
            }
        }

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
                    self.scaffold = Some(cx.svc.call(|c| async move { c.kb_scaffold().await }));
                }
                if ui
                    .add_enabled(has, egui::Button::new(tr("Abrir pasta")))
                    .clicked()
                {
                    let svc = cx.svc.clone();
                    let t: Task<Value> = svc.call(|c| async move { c.kb_open().await });
                    drop(t);
                }
                if ui
                    .add_enabled(has, egui::Button::new(tr("Ver .mcp.json")))
                    .clicked()
                {
                    self.mcp_json
                        .start(cx.svc.call(|c| async move { c.kb_mcp_json().await }));
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
