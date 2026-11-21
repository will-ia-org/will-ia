//! Aba Backup / Restore (Configurações → Geral): baixa um `.zip` com a pasta de
//! dados da instância conectada (banco, cofre cifrado sem a chave, sessão do
//! WhatsApp, perfil do navegador e base de conhecimento opcionais) e restaura um `.zip` em outra instância — o serviço
//! valida, agenda e reinicia (`docs/10`, RF-CFG-11).

use crate::i18n::{tr, trf};
use crate::pages::Cx;
use crate::service::{Loader, Task};
use crate::ui::{self, Tone};
use serde_json::Value;
use std::path::PathBuf;

/// Download do backup direto para o arquivo: bytes gravados.
type BackupTask = Task<u64>;

pub struct BackupPage {
    info: Loader<Value>,
    /// Tamanho da base de conhecimento (varredura lenta, à parte de `info`).
    kb_size: Loader<Value>,
    include_browser: bool,
    include_whatsapp: bool,
    include_kb: bool,
    /// Com a base: deixa de fora o que o git ignora (caches, `node_modules`…).
    kb_skip_ignored: bool,
    /// Chave do cofre desta instância (só depois de "Mostrar chave do cofre").
    key_task: Option<Task<Value>>,
    vault_key: Option<String>,
    /// Chave do cofre da máquina de origem, digitada para restaurar.
    restore_key: String,
    /// (destino escolhido, download em voo)
    backup_task: Option<(PathBuf, BackupTask)>,
    restore_task: Option<Task<Value>>,
    confirm_restore: Option<PathBuf>,
    last_backup: Option<PathBuf>,
    restore_sent: bool,
    error: Option<String>,
}

impl Default for BackupPage {
    fn default() -> Self {
        Self {
            info: Loader::default(),
            kb_size: Loader::default(),
            include_browser: false,
            include_whatsapp: true,
            include_kb: false,
            kb_skip_ignored: true,
            key_task: None,
            vault_key: None,
            restore_key: String::new(),
            backup_task: None,
            restore_task: None,
            confirm_restore: None,
            last_backup: None,
            restore_sent: false,
            error: None,
        }
    }
}

fn fmt_bytes(b: u64) -> String {
    if b >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", b as f64 / 1_073_741_824.0)
    } else if b >= 1024 * 1024 {
        format!("{:.1} MB", b as f64 / 1_048_576.0)
    } else {
        format!("{} KB", b / 1024)
    }
}

impl BackupPage {
    fn poll(&mut self, cx: &mut Cx) {
        if self.info.stale(std::time::Duration::from_secs(60)) {
            self.info
                .start(cx.svc.call(|c| async move { c.backup_info().await }));
        }
        self.info.poll();
        if self.kb_size.stale(std::time::Duration::from_secs(600)) {
            self.kb_size
                .start(cx.svc.call(|c| async move { c.backup_kb_size().await }));
        }
        self.kb_size.poll();
        let mut done: Option<Result<u64, String>> = None;
        if let Some((_, t)) = &self.backup_task {
            if let Some(r) = t.take() {
                done = Some(r);
            }
        }
        if let Some(r) = done {
            let (dest, _) = self.backup_task.take().expect("tarefa de backup");
            match r {
                Ok(bytes) => {
                    cx.toasts
                        .success(trf("Backup salvo ({})", &[&fmt_bytes(bytes)]));
                    self.last_backup = Some(dest);
                    self.error = None;
                }
                Err(e) => self.error = Some(e),
            }
        }
        if let Some(t) = &self.key_task {
            if let Some(r) = t.take() {
                self.key_task = None;
                match r {
                    Ok(v) => {
                        self.vault_key = Some(ui::s(&v, "key"));
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
        if let Some(t) = &self.restore_task {
            if let Some(r) = t.take() {
                self.restore_task = None;
                match r {
                    Ok(_) => {
                        self.restore_key.clear();
                        self.restore_sent = true;
                        self.error = None;
                        self.info.loaded_at = None;
                        cx.toasts.success(tr(
                            "Restauração enviada. O serviço está reiniciando com os dados do backup.",
                        ));
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        self.poll(cx);
        let dark = ui.visuals().dark_mode;
        let info = self.info.data.clone().unwrap_or(Value::Null);
        let db_bytes = info.get("db_bytes").and_then(|v| v.as_u64()).unwrap_or(0);
        let browser_bytes = info
            .get("browser_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let whatsapp_bytes = info
            .get("whatsapp_bytes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let kb = self.kb_size.data.clone().unwrap_or(Value::Null);
        let kb_path = ui::s(&kb, "path");
        let kb_u64 = |k: &str| kb.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        let (kb_bytes, kb_bytes_all) = (kb_u64("bytes"), kb_u64("bytes_all"));
        let kb_git = ui::b(&kb, "git");
        let kb_measured = self.kb_size.data.is_some();
        let suggested = ui::s(&info, "suggested_name");
        let pending = ui::b(&info, "restore_pending");

        // ---- backup ----
        ui::section(ui, tr("Backup"), |ui| {
            ui.label(tr(
                "Gera um .zip com a pasta de dados da instância conectada, para levar tudo a outro computador sem configurar de novo:",
            ));
            ui.add_space(4.0);
            for line in [
                trf("banco de dados (threads, histórico, aprovações, tarefas, agendamentos, configurações) · {}", &[&fmt_bytes(db_bytes)]),
                tr("cofre de segredos cifrado (tokens do Claude, bot do Telegram, chaves de API, contas Google); a chave do cofre NÃO vai no arquivo").to_string(),
                tr("telemetria embutida e identidade da instância").to_string(),
            ] {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui::dot(ui, Tone::Success);
                    ui.label(egui::RichText::new(line).small());
                });
            }
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui::dot(ui, Tone::Neutral);
                ui.label(
                    egui::RichText::new(tr(
                        "NÃO entra: logs, caches e arquivos da máquina (porta, lock, manifesto da instalação). A base de conhecimento só entra se você ligar a opção abaixo.",
                    ))
                    .small()
                    .weak(),
                );
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui::toggle(ui, &mut self.include_whatsapp);
                ui.label(trf(
                    "incluir a sessão do WhatsApp (pareamento) · {}",
                    &[&fmt_bytes(whatsapp_bytes)],
                ));
            });
            if self.include_whatsapp {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(tr(
                            "Se a máquina de origem continuar ligada, as duas vão disputar a mesma sessão do WhatsApp.",
                        ))
                        .small()
                        .weak(),
                    );
                });
            }
            ui.horizontal(|ui| {
                ui::toggle(ui, &mut self.include_browser);
                ui.label(trf(
                    "incluir o perfil do navegador (logins do agent-browser) · {}",
                    &[&fmt_bytes(browser_bytes)],
                ));
            });
            let kb_ready = !kb_path.is_empty();
            if !kb_ready {
                self.include_kb = false;
            }
            ui.horizontal(|ui| {
                ui.add_enabled_ui(kb_ready, |ui| ui::toggle(ui, &mut self.include_kb));
                let size = if self.kb_skip_ignored && kb_git {
                    kb_bytes
                } else {
                    kb_bytes_all
                };
                ui.label(if kb_ready {
                    trf(
                        "incluir a base de conhecimento (a pasta inteira) · {}",
                        &[&fmt_bytes(size)],
                    )
                } else if kb_measured {
                    tr("incluir a base de conhecimento · nenhuma pasta configurada nesta instância")
                        .to_string()
                } else {
                    tr("incluir a base de conhecimento · medindo a pasta…").to_string()
                });
            });
            if self.include_kb {
                let note = |ui: &mut egui::Ui, text: String| {
                    ui.horizontal_wrapped(|ui| {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(text).small().weak());
                    });
                };
                note(ui, trf("Pasta: {}", &[&kb_path]));
                if kb_git {
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui::toggle(ui, &mut self.kb_skip_ignored);
                        ui.label(trf(
                            "deixar de fora o que o .gitignore ignora (caches, node_modules) · {} em vez de {}",
                            &[&fmt_bytes(kb_bytes), &fmt_bytes(kb_bytes_all)],
                        ));
                    });
                }
                note(
                    ui,
                    tr("Os arquivos da base vão em texto claro dentro do zip (a chave do cofre não os protege). Para restaurar, o destino precisa ter a pasta da Base de conhecimento já configurada e existente: é nela que os arquivos entram, e o que estiver lá é renomeado, não apagado. Sem pasta válida, a restauração é barrada.").to_string(),
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let busy = self.backup_task.is_some();
                if ui
                    .add_enabled(!busy, egui::Button::new(tr("Gerar backup…")))
                    .clicked()
                {
                    let name = if suggested.is_empty() {
                        "will-ia-backup.zip".to_string()
                    } else {
                        suggested.clone()
                    };
                    if let Some(dest) = crate::platform::save_zip(tr("Salvar backup"), &name) {
                        let opts = will_client::BackupOptions {
                            browser: self.include_browser,
                            whatsapp: self.include_whatsapp,
                            kb: self.include_kb,
                            kb_all: !(self.kb_skip_ignored && kb_git),
                        };
                        let to = dest.clone();
                        let t = cx
                            .svc
                            .call(move |c| async move { c.backup_to_file(to, opts).await });
                        self.backup_task = Some((dest, t));
                        self.error = None;
                    }
                }
                if busy {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(tr("gerando e baixando…"))
                            .weak()
                            .small(),
                    );
                }
            });
            if let Some(p) = self.last_backup.clone() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(trf("Salvo em {}", &[&p.display()]))
                            .small()
                            .weak(),
                    );
                    if ui.small_button(tr("Mostrar na pasta")).clicked() {
                        if let Some(dir) = p.parent() {
                            let _ = crate::platform::open_path(dir);
                        }
                    }
                });
            }
        });

        // ---- chave do cofre ----
        ui::section(ui, tr("Chave do cofre"), |ui| {
            ui.label(tr(
                "O backup leva os segredos cifrados, mas não a chave que os abre. Para restaurar em outra máquina, copie a chave daqui e informe lá. Quem tiver o arquivo e a chave lê todos os segredos: guarde os dois separados.",
            ));
            ui.add_space(6.0);
            match self.vault_key.clone() {
                None => {
                    ui.horizontal(|ui| {
                        let busy = self.key_task.is_some();
                        if ui
                            .add_enabled(!busy, egui::Button::new(tr("Mostrar chave do cofre")))
                            .clicked()
                        {
                            self.key_task =
                                Some(cx.svc.call(|c| async move { c.backup_key().await }));
                        }
                        if busy {
                            ui.spinner();
                        }
                    });
                }
                Some(k) => {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&k).monospace());
                        if ui.small_button(tr("Copiar")).clicked() {
                            ui.ctx().copy_text(k.clone());
                            cx.toasts.success(tr("Chave do cofre copiada"));
                        }
                        if ui.small_button(tr("Ocultar")).clicked() {
                            self.vault_key = None;
                        }
                    });
                }
            }
        });

        // ---- restore ----
        ui::section(ui, tr("Restaurar"), |ui| {
            ui.label(tr(
                "Escolha um .zip gerado pelo backup. Os dados atuais desta instância são guardados em old/restore-<data>/ e substituídos pelos do arquivo; o serviço reinicia sozinho e o app reconecta.",
            ));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(tr(
                    "Use isto na máquina nova logo após instalar o will-ia. Se o backup traz a base de conhecimento, configure antes a pasta em Base de conhecimento nesta instância: sem pasta válida a restauração é barrada. Se não traz, a base precisa estar no caminho configurado (ou ajuste depois).",
                ))
                .small()
                .weak(),
            );
            ui.add_space(8.0);
            ui.label(tr("Chave do cofre da máquina de origem"));
            ui.add(
                egui::TextEdit::singleline(&mut self.restore_key)
                    .password(true)
                    .desired_width(380.0)
                    .hint_text(tr("cole a chave mostrada em Backup › Chave do cofre")),
            );
            ui.label(
                egui::RichText::new(tr(
                    "Pode ficar vazia ao restaurar um backup feito nesta mesma máquina ou um backup antigo (que ainda levava a chave).",
                ))
                .small()
                .weak(),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let busy = self.restore_task.is_some();
                if ui
                    .add_enabled(!busy, egui::Button::new(tr("Restaurar de um arquivo…")))
                    .clicked()
                {
                    if let Some(p) = crate::platform::pick_zip(tr("Escolher backup")) {
                        self.confirm_restore = Some(p);
                    }
                }
                if busy {
                    ui.spinner();
                    ui.label(egui::RichText::new(tr("enviando…")).weak().small());
                }
                if pending {
                    ui::badge(ui, tr("restauração pendente"), Tone::Warning);
                }
            });
            if self.restore_sent {
                ui.colored_label(
                    Tone::Success.color(dark),
                    tr("Restauração agendada. Aguarde o serviço voltar; se ele não for residente, inicie-o de novo à mão."),
                );
            }
        });
        if let Some(e) = &self.error {
            ui::error_text(ui, e);
        }

        // ---- confirmação ----
        if let Some(p) = self.confirm_restore.clone() {
            let c = ui::Confirm {
                title: tr("Restaurar este backup?").into(),
                body: trf(
                    "Os dados atuais desta instância serão substituídos por {} e o serviço vai reiniciar. Threads, aprovações, tarefas, segredos e sessão do WhatsApp passam a ser os do arquivo. Se o backup trouxe a base de conhecimento, a pasta atual da base é renomeada e a do arquivo entra no lugar.",
                    &[&p.display()],
                ),
                confirm_label: tr("Restaurar e reiniciar").into(),
                danger: true,
                tag: "restore-backup".into(),
            };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_restore = None;
                if ok {
                    let key = self.restore_key.clone();
                    self.restore_task = Some(
                        cx.svc
                            .call(move |c| async move { c.restore_file(p, key).await }),
                    );
                    self.error = None;
                }
            }
        }
    }
}
