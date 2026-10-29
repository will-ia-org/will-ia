//! Seção Rede: onde o servidor escuta (`bind`), TLS embutido e o certificado
//! em uso, cujo PEM entra no perfil do app remoto (`docs/26`, etapa 5).
//! As mudanças de `NetworkSettings` valem no próximo início do servidor.

use super::SettingsState;
use crate::i18n::{tr, trf};
use crate::pages::Cx;
use crate::service::Loader;
use crate::ui::{self, Tone};
use serde_json::Value;

#[derive(Default)]
pub struct NetworkPage {
    /// `GET /api/network/cert`: caminho, PEM e impressão digital do certificado em uso.
    cert: Loader<Value>,
}

/// `127.0.0.1`, `localhost` ou `::1`: só esta máquina.
fn is_loopback(bind: &str) -> bool {
    let b = bind.trim().trim_matches(|c| c == '[' || c == ']');
    b.is_empty() || b == "127.0.0.1" || b == "localhost" || b == "::1" || b.starts_with("127.")
}

/// Campo de texto para `Option<String>` (vazio = `None`).
fn optional_path(ui: &mut egui::Ui, value: &mut Option<String>, hint: &str) {
    let mut text = value.clone().unwrap_or_default();
    if ui::text_input(ui, &mut text, hint).changed() {
        *value = Some(text.trim().to_string()).filter(|s| !s.is_empty());
    }
}

impl NetworkPage {
    fn load_cert(&mut self, cx: &mut Cx) {
        self.cert
            .start(cx.svc.call(|c| async move { c.get("network/cert").await }));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, st: &mut SettingsState) {
        if self.cert.stale(std::time::Duration::from_secs(300)) {
            self.load_cert(cx);
        }
        self.cert.poll();
        let dark = ui.visuals().dark_mode;

        ui::heading(
            ui,
            tr("Rede"),
            Some(tr(
                "Onde o servidor escuta e como apps em outras máquinas se conectam a ele.",
            )),
        );

        let tab = ui::tabs(
            ui,
            "network",
            &[
                ui::Tab::new("listen", tr("Escuta")),
                ui::Tab::new("cert", tr("Certificado em uso")),
            ],
        );

        if tab == "listen" {
            ui::section(ui, tr("Escuta"), |ui| {
                let n = &mut st.draft.network;
                ui::field(
                ui,
                tr("Endereço de escuta"),
                Some(tr(
                    "127.0.0.1 = só esta máquina; 0.0.0.0 = todas as interfaces (exige TLS ou a opção abaixo).",
                )),
                |ui| {
                    ui::text_input(ui, &mut n.bind, "127.0.0.1");
                },
            );
                ui::field(
                ui,
                tr("TLS (https)"),
                Some(tr(
                    "Criptografa a API e o WebSocket. Apps remotos fixam o certificado pelo PEM mostrado abaixo.",
                )),
                |ui| {
                    ui::toggle(ui, &mut n.tls);
                },
            );
                ui::field(
                    ui,
                    tr("Certificado (cert_path)"),
                    Some(tr("Vazio = autoassinado gerado em <data_dir>/tls/.")),
                    |ui| {
                        optional_path(ui, &mut n.cert_path, "/caminho/para/cert.pem");
                    },
                );
                ui::field(
                    ui,
                    tr("Chave privada (key_path)"),
                    Some(tr("Vazio = autoassinado gerado em <data_dir>/tls/.")),
                    |ui| {
                        optional_path(ui, &mut n.key_path, "/caminho/para/key.pem");
                    },
                );
                ui::field(
                    ui,
                    tr("Permitir acesso remoto sem TLS"),
                    Some(tr(
                        "Só atrás de VPN (Tailscale/WireGuard) ou de um proxy com https.",
                    )),
                    |ui| {
                        ui::toggle(ui, &mut n.allow_remote_without_tls);
                        if n.allow_remote_without_tls {
                            ui.colored_label(
                            Tone::Danger.color(dark),
                            tr("Perigoso: senhas, tokens e conversas trafegam em texto puro na rede."),
                        );
                        }
                    },
                );
                if !is_loopback(&n.bind) && !n.tls && !n.allow_remote_without_tls {
                    ui.colored_label(
                    Tone::Warning.color(dark),
                    tr("Escutar fora do loopback exige TLS ligado ou \"Permitir acesso remoto sem TLS\"; sem isso o servidor volta a escutar só em 127.0.0.1."),
                );
                }
                ui.label(
                    egui::RichText::new(tr(
                        "As mudanças desta seção valem no próximo início do servidor.",
                    ))
                    .small()
                    .weak(),
                );
            });
            st.save_bar(ui, cx.svc, "network");
        }
        if tab == "cert" {
            ui::section(ui, tr("Certificado em uso"), |ui| {
                ui.label(
                egui::RichText::new(tr(
                    "É este PEM que entra no perfil do app remoto (Conexão → TLS (https) → Certificado do servidor). Confira a impressão digital dos dois lados antes de confiar.",
                ))
                .weak()
                .small(),
            );
                ui.add_space(4.0);
                if self.cert.loading() {
                    ui::busy(ui, tr("carregando certificado…"));
                    return;
                }
                let (data, err) = (self.cert.data.clone(), self.cert.error.clone());
                match (&data, &err) {
                    (Some(c), _) => {
                        let pem = ui::s(c, "pem");
                        let fp = ui::s(c, "fingerprint_sha256");
                        let path = ui::s(c, "path");
                        ui::field(ui, tr("Impressão digital SHA-256"), None, |ui| {
                            if fp.is_empty() {
                                ui.label(egui::RichText::new("—").weak());
                            } else {
                                ui::mono(ui, &fp);
                            }
                        });
                        if !path.is_empty() {
                            ui::field(ui, tr("Arquivo"), None, |ui| ui::mono(ui, &path));
                        }
                        ui::field(ui, tr("Certificado (PEM)"), None, |ui| {
                            if pem.is_empty() {
                                ui.label(
                                egui::RichText::new(tr(
                                    "Nenhum certificado em uso (TLS desligado ou o servidor ainda não reiniciou).",
                                ))
                                .weak(),
                            );
                            } else {
                                let mut shown = pem.clone();
                                ui.add(
                                    egui::TextEdit::multiline(&mut shown)
                                        .desired_rows(8)
                                        .desired_width(f32::INFINITY)
                                        .font(egui::TextStyle::Monospace)
                                        .interactive(false),
                                );
                            }
                            ui.horizontal(|ui| {
                                if !pem.is_empty() && ui.button(tr("Copiar")).clicked() {
                                    ui.ctx().copy_text(pem.clone());
                                    cx.toasts.success(tr("Certificado copiado."));
                                }
                                if ui.button(tr("Recarregar")).clicked() {
                                    self.cert.loaded_at = None;
                                }
                            });
                        });
                    }
                    (None, Some(e)) => {
                        ui.label(
                        egui::RichText::new(tr(
                            "Nenhum certificado em uso (TLS desligado ou o servidor ainda não reiniciou).",
                        ))
                        .weak(),
                    );
                        ui.label(
                            egui::RichText::new(trf("servidor: {}", &[&e]))
                                .small()
                                .weak(),
                        );
                        if ui.button(tr("Recarregar")).clicked() {
                            self.cert.loaded_at = None;
                        }
                    }
                    (None, None) => {
                        if ui.button(tr("Recarregar")).clicked() {
                            self.cert.loaded_at = None;
                        }
                    }
                }
            });
        }
    }
}
