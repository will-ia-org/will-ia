//! Permissões do macOS pedidas **de uma vez**, na primeira abertura depois de
//! instalar (ADR-0023, `docs/27` "Permissões no macOS"): microfone (diálogo do
//! AVFoundation) e notificações (uma notificação de boas-vindas gera o pedido).
//! Arquivos e Pastas é pedido pelo servidor ao tocar na KB. Um marcador em
//! `<config do app>/permissions-pass.json` guarda a versão da passagem, para não
//! repetir. Fora do macOS tudo aqui é inerte.

// Fora do macOS quase tudo deste módulo fica sem uso (o CI Linux/Windows roda com
// `-D warnings`): silencia só aqui, em vez de espalhar `cfg` por campo e import.
#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]

use crate::i18n::tr;
use crate::pages::Cx;
use crate::ui::{self, Tone};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Suba quando a passagem ganhar um pedido novo (o app refaz só o que falta).
pub const PASS_VERSION: u32 = 1;

const MIC_UNKNOWN: u8 = 0;
const MIC_GRANTED: u8 = 1;
const MIC_DENIED: u8 = 2;

pub struct Permissions {
    /// Resposta do diálogo de microfone pedido por nós (0 = sem resposta ainda).
    mic_answer: Arc<AtomicU8>,
    mic_requested_at: Option<Instant>,
    welcome_sent: bool,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    last_check: Option<Instant>,
    #[cfg(target_os = "macos")]
    mic: will_platform::macos::Authorization,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            mic_answer: Arc::new(AtomicU8::new(MIC_UNKNOWN)),
            mic_requested_at: None,
            welcome_sent: false,
            last_check: None,
            #[cfg(target_os = "macos")]
            mic: will_platform::macos::Authorization::Unknown,
        }
    }
}

fn marker_path() -> PathBuf {
    crate::profiles::config_dir().join("permissions-pass.json")
}

fn pass_done_version() -> u32 {
    std::fs::read_to_string(marker_path())
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.get("version").and_then(|x| x.as_u64()))
        .unwrap_or(0) as u32
}

fn mark_pass_done() {
    let p = marker_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let body = serde_json::json!({
        "version": PASS_VERSION,
        "at": chrono::Local::now().to_rfc3339(),
    });
    let _ = std::fs::write(p, body.to_string());
}

impl Permissions {
    /// Primeira abertura após instalar/atualizar a passagem: pede tudo de uma vez.
    pub fn first_run_pass(&mut self) {
        if pass_done_version() >= PASS_VERSION {
            return;
        }
        #[cfg(target_os = "macos")]
        {
            self.refresh(true);
            if self.mic == will_platform::macos::Authorization::NotDetermined {
                self.request_mic();
            }
            self.send_welcome();
        }
        mark_pass_done();
    }

    #[cfg(target_os = "macos")]
    fn refresh(&mut self, force: bool) {
        let stale = self
            .last_check
            .map(|t| t.elapsed() > Duration::from_secs(5))
            .unwrap_or(true);
        if force || stale {
            self.mic = will_platform::macos::microphone_status();
            self.last_check = Some(Instant::now());
        }
    }

    #[cfg(target_os = "macos")]
    fn request_mic(&mut self) {
        let answer = self.mic_answer.clone();
        answer.store(MIC_UNKNOWN, Ordering::SeqCst);
        self.mic_requested_at = Some(Instant::now());
        will_platform::macos::request_microphone(move |granted| {
            answer.store(
                if granted { MIC_GRANTED } else { MIC_DENIED },
                Ordering::SeqCst,
            );
        });
    }

    fn send_welcome(&mut self) {
        self.welcome_sent = true;
        will_platform::notify(
            "will-ia instalado",
            "Notificações ativas. Aprovações e avisos chegam por aqui.",
        );
    }

    /// Cartão "Permissões do macOS" (só no macOS).
    #[cfg(not(target_os = "macos"))]
    pub fn ui(&mut self, _ui: &mut egui::Ui, _cx: &mut Cx) {}

    #[cfg(target_os = "macos")]
    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        use will_platform::macos::{Authorization, SettingsPane};
        self.refresh(false);
        // resposta do diálogo chegou: reconsulta na hora
        if self.mic_requested_at.is_some() && self.mic_answer.load(Ordering::SeqCst) != MIC_UNKNOWN
        {
            self.mic_requested_at = None;
            self.refresh(true);
        }
        let mic = self.mic;
        let mic_tone = match mic {
            Authorization::Authorized => Tone::Success,
            Authorization::NotDetermined | Authorization::Unknown => Tone::Warning,
            Authorization::Restricted | Authorization::Denied => Tone::Danger,
        };
        let toasts = &mut *cx.toasts;
        let open = |pane: SettingsPane, toasts: &mut ui::Toasts| {
            if let Err(e) = will_platform::macos::open_settings(pane) {
                toasts.error(format!("Ajustes: {e}"));
            }
        };
        let mut want_mic = false;
        let mut want_welcome = false;
        ui::section_help(
            ui,
            tr("Permissões do macOS"),
            Some(&crate::help::MACOS_PERMISSIONS),
            |ui| {
                ui.label(tr(
                    "Pedidas de uma vez na primeira abertura; se algo foi negado, libere em Ajustes. Com a assinatura estável do instalador, a concessão vale para as próximas versões.",
                ));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui::dot(ui, mic_tone);
                    ui.label(format!("{} — {}", tr("Microfone (voz)"), tr(mic.label())));
                    match mic {
                        Authorization::NotDetermined | Authorization::Unknown => {
                            if ui.button(tr("Pedir agora")).clicked() {
                                want_mic = true;
                            }
                        }
                        Authorization::Denied | Authorization::Restricted => {
                            if ui.button(tr("Abrir Ajustes")).clicked() {
                                open(SettingsPane::Microphone, toasts);
                            }
                        }
                        Authorization::Authorized => {}
                    }
                });
                ui.horizontal(|ui| {
                    ui::dot(
                        ui,
                        if self.welcome_sent {
                            Tone::Info
                        } else {
                            Tone::Neutral
                        },
                    );
                    ui.label(tr(
                        "Notificações — o sistema não informa o estado; teste e confira o banner",
                    ));
                    if ui.button(tr("Testar notificação")).clicked() {
                        want_welcome = true;
                    }
                    if ui.button(tr("Abrir Ajustes")).clicked() {
                        open(SettingsPane::Notifications, toasts);
                    }
                });
                ui.horizontal(|ui| {
                    ui::dot(ui, Tone::Neutral);
                    ui.label(tr("Arquivos e Pastas — o servidor pede ao tocar na base de conhecimento (Documentos, Mesa, iCloud…)"));
                    if ui.button(tr("Abrir Ajustes")).clicked() {
                        open(SettingsPane::FilesAndFolders, toasts);
                    }
                });
                ui.horizontal(|ui| {
                    ui::dot(ui, Tone::Neutral);
                    ui.label(tr(
                        "Itens de Início — o servidor aparece como item de segundo plano",
                    ));
                    if ui.button(tr("Abrir Ajustes")).clicked() {
                        open(SettingsPane::LoginItems, toasts);
                    }
                });
            },
        );
        if want_mic {
            self.request_mic();
        }
        if want_welcome {
            self.send_welcome();
        }
    }
}
