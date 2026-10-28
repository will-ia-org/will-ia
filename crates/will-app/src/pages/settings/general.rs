//! Seção Geral: autostart, idioma, log, notificações, tema, porta da API.

use super::SettingsState;
use crate::i18n::tr;
use crate::pages::Cx;
use crate::ui;

#[derive(Default)]
pub struct GeneralPage {
    autostart_error: Option<String>,
    last_autostart: Option<bool>,
}

impl GeneralPage {
    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        ui::heading(
            ui,
            tr("Geral"),
            Some(tr("Comportamento do aplicativo nesta máquina.")),
        );
        let portable = cx.snap.portable_root.is_some();
        ui::section(ui, tr("Aplicativo"), |ui| {
            let g = &mut st.draft.general;
            ui::field(
                ui,
                tr("Iniciar com o sistema"),
                Some(if portable {
                    tr("Indisponível em modo portátil (pendrive).")
                } else {
                    tr("Registra o will-ia no login do usuário (inicia na bandeja).")
                }),
                |ui| {
                    ui.add_enabled_ui(!portable, |ui| {
                        ui::toggle(ui, &mut g.autostart);
                    });
                },
            );
            ui::field(ui, tr("Idioma da UI"), None, |ui| {
                ui::select(
                    ui,
                    "lang",
                    &mut g.language,
                    &[("pt-BR", tr("Português (Brasil)")), ("en", tr("English"))],
                );
            });
            ui::field(ui, tr("Nível de log"), None, |ui| {
                ui::select(
                    ui,
                    "loglevel",
                    &mut g.log_level,
                    &[
                        ("error", "error"),
                        ("warn", "warn"),
                        ("info", "info"),
                        ("debug", "debug"),
                        ("trace", "trace"),
                    ],
                );
            });
            ui::field(ui, tr("Notificações nativas"), None, |ui| {
                ui::toggle(ui, &mut g.notifications);
            });
            ui::field(
                ui,
                tr("Tema"),
                Some(tr("Aplicado imediatamente ao salvar.")),
                |ui| {
                    ui::select(
                        ui,
                        "theme",
                        &mut g.theme,
                        &[
                            ("system", tr("Sistema")),
                            ("light", tr("Claro")),
                            ("dark", tr("Escuro")),
                        ],
                    );
                },
            );
            ui::field(
                ui,
                tr("Escala da interface"),
                Some(tr("1.0 = tamanho do sistema. Salve para aplicar.")),
                |ui| {
                    ui.add(
                        egui::Slider::new(&mut g.ui_scale, 0.6..=1.6)
                            .step_by(0.05)
                            .show_value(true),
                    );
                },
            );
            ui::field(
                ui,
                tr("Porta da API local"),
                Some(tr(
                    "Usada pelo modo headless e pela segunda instância. Exige reiniciar.",
                )),
                |ui| {
                    ui::number(ui, &mut g.ui_port, 0..=65535);
                },
            );
            ui::field(
                ui,
                tr("Onboarding concluído"),
                Some(tr("Desmarque para ver os primeiros passos no Início.")),
                |ui| {
                    ui::toggle(ui, &mut g.onboarding_done);
                },
            );
        });
        if let Some(e) = &self.autostart_error {
            ui::error_text(ui, e);
        }
        st.save_bar(ui, cx.svc, "general");

        // Aplica autostart quando o valor salvo muda (não em modo portátil).
        let saved = st.saved.general.autostart && !portable;
        if self.last_autostart != Some(saved) {
            self.last_autostart = Some(saved);
            self.autostart_error = apply_autostart(saved).err();
        }

        ui::section(ui, tr("Dados"), |ui| {
            ui::field(ui, tr("Modo"), None, |ui| {
                if let Some(root) = &cx.snap.portable_root {
                    ui::badge(ui, tr("portátil"), ui::Tone::Info);
                    ui::mono(ui, &root.display().to_string());
                } else {
                    ui::badge(ui, tr("instalado"), ui::Tone::Neutral);
                }
            });
            ui::field(ui, tr("Banco SQLite"), None, |ui| {
                ui::mono(ui, &cx.snap.db_path.display().to_string())
            });
            ui::field(
                ui,
                tr("Pasta de dados"),
                Some(tr("Segredos (arquivo), sessões do WhatsApp, logs, cache.")),
                |ui| {
                    ui.horizontal(|ui| {
                        ui::mono(ui, &cx.snap.data_dir.display().to_string());
                        if ui.button(tr("abrir")).clicked() {
                            if !cx.snap.server_is_local {
                                cx.toasts.info(tr(
                                    "A pasta fica no servidor; abra-a por lá (o app está conectado a um servidor remoto).",
                                ));
                            } else if let Err(e) = will_platform::open_path(&cx.snap.data_dir) {
                                cx.toasts.error(e);
                            }
                        }
                    });
                },
            );
            ui::field(ui, tr("API local"), None, |ui| {
                ui::mono(ui, &cx.snap.api_url)
            });
        });
    }
}

/// O autostart é do **servidor** (`will-ia`, residente na bandeja), não do app:
/// procura o binário ao lado deste executável e, depois, no PATH.
fn server_binary() -> Result<std::path::PathBuf, String> {
    let name = if cfg!(windows) {
        "will-ia.exe"
    } else {
        "will-ia"
    };
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            let p = dir.join(name);
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    will_platform::which("will-ia").ok_or_else(|| {
        tr("binário do servidor (will-ia) não encontrado ao lado do app nem no PATH").to_string()
    })
}

fn apply_autostart(enable: bool) -> Result<(), String> {
    let exe = server_binary()?;
    let exe_s = exe.to_string_lossy().to_string();
    let al = auto_launch::AutoLaunchBuilder::new()
        .set_app_name("will-ia")
        .set_app_path(&exe_s)
        .build()
        .map_err(|e| e.to_string())?;
    let enabled = al.is_enabled().unwrap_or(false);
    if enable && !enabled {
        al.enable().map_err(|e| format!("autostart: {e}"))?;
    } else if !enable && enabled {
        al.disable().map_err(|e| format!("autostart: {e}"))?;
    }
    Ok(())
}
