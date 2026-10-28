//! Indicador flutuante de voz: janela pequena, sem decoração, sempre no topo,
//! mostrando o estado da sessão (ouvindo / pensando / falando) e o nível do
//! microfone. É um viewport "deferred" do egui, redesenhado por conta própria.

use crate::i18n::{tr, trf};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

#[derive(Default)]
pub struct OverlayState {
    pub state: String,
    pub detail: Option<String>,
    pub level: f32,
    pub ptt_mode: bool,
    pub ptt_down: bool,
    pub pending_brain: usize,
    pub last_transcript: Option<String>,
}

pub type Shared = Arc<Mutex<OverlayState>>;

pub const SIZE: [f32; 2] = [280.0, 84.0];

pub fn label_for(state: &str) -> (&'static str, egui::Color32) {
    match state {
        "connecting" => (tr("conectando…"), egui::Color32::from_rgb(150, 150, 160)),
        "listening" => (tr("ouvindo"), egui::Color32::from_rgb(60, 180, 120)),
        "user_speaking" => (
            tr("você está falando"),
            egui::Color32::from_rgb(60, 200, 140),
        ),
        "thinking" => (tr("pensando"), egui::Color32::from_rgb(220, 160, 40)),
        "brain_working" => (
            tr("consultando o cérebro"),
            egui::Color32::from_rgb(220, 120, 40),
        ),
        "speaking" => (tr("falando"), egui::Color32::from_rgb(70, 130, 230)),
        "error" => (tr("erro"), egui::Color32::from_rgb(220, 70, 70)),
        _ => (tr("voz"), egui::Color32::from_rgb(150, 150, 160)),
    }
}

/// Deve ser chamado a cada quadro do viewport raiz enquanto a voz estiver ativa.
pub fn show(ctx: &egui::Context, shared: &Shared) {
    let monitor = ctx
        .input(|i| i.viewport().monitor_size)
        .unwrap_or(egui::vec2(1920.0, 1080.0));
    let pos = egui::pos2(
        (monitor.x - SIZE[0] - 24.0).max(0.0),
        (monitor.y - SIZE[1] - 72.0).max(0.0),
    );
    let builder = egui::ViewportBuilder::default()
        .with_title(tr("will-ia · voz"))
        .with_inner_size(SIZE)
        .with_min_inner_size(SIZE)
        .with_position(pos)
        .with_decorations(false)
        .with_resizable(false)
        .with_transparent(cfg!(not(target_os = "linux")))
        .with_always_on_top()
        .with_taskbar(false);
    let shared = shared.clone();
    ctx.show_viewport_deferred(
        egui::ViewportId::from_hash_of("voice-overlay"),
        builder,
        move |ui, _class| {
            ui.ctx().request_repaint_after(Duration::from_millis(60));
            let s = shared.lock();
            let (label, color) = label_for(&s.state);
            let t = ui.input(|i| i.time) as f32;
            let dark = ui.visuals().dark_mode;
            let bg = if dark {
                egui::Color32::from_rgba_unmultiplied(20, 24, 32, 235)
            } else {
                egui::Color32::from_rgba_unmultiplied(250, 250, 252, 240)
            };
            egui::Frame::new()
                .fill(bg)
                .corner_radius(0.0)
                .stroke(egui::Stroke::new(1.0, color.linear_multiply(0.6)))
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.set_min_size(egui::vec2(SIZE[0] - 24.0, SIZE[1] - 24.0));
                    ui.horizontal(|ui| {
                        // círculo pulsante + nível
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
                        let c = rect.center();
                        let pulse = match s.state.as_str() {
                            "speaking" | "thinking" | "brain_working" | "connecting" => {
                                0.5 + 0.5 * (t * 4.0).sin()
                            }
                            _ => 0.0,
                        };
                        let level = (s.level * 6.0).clamp(0.0, 1.0);
                        ui.painter().circle_filled(
                            c,
                            14.0 + 8.0 * level.max(pulse * 0.6),
                            color.linear_multiply(0.25),
                        );
                        ui.painter().circle_filled(c, 10.0, color);
                        ui.add_space(6.0);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(label).strong().size(15.0).color(color));
                            let sub = if s.ptt_mode {
                                if s.ptt_down {
                                    tr("segurando para falar").to_string()
                                } else {
                                    tr("segure o atalho para falar").to_string()
                                }
                            } else if s.pending_brain > 0 {
                                trf("{} tarefa(s) no cérebro", &[&s.pending_brain])
                            } else {
                                s.last_transcript
                                    .clone()
                                    .or_else(|| s.detail.clone())
                                    .unwrap_or_else(|| "will-ia".into())
                            };
                            ui.add(
                                egui::Label::new(egui::RichText::new(sub).small().weak())
                                    .truncate(),
                            );
                        });
                    });
                });
        },
    );
}
