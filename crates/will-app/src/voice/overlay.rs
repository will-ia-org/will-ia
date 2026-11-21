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

/// Distância das caixinhas flutuantes até a borda da tela e entre si.
const EDGE: f32 = 16.0;
const STACK_GAP: f32 = 10.0;

/// Altura já ocupada pelas caixinhas abertas neste quadro (a próxima empilha
/// em cima). Zera quando o quadro muda.
fn stack_lift(ctx: &egui::Context, height: f32) -> f32 {
    let frame = ctx.cumulative_frame_nr();
    let id = egui::Id::unique("overlay-stack");
    ctx.data_mut(|d| {
        let (seen, used) = d.get_temp::<(u64, f32)>(id).unwrap_or((frame, 0.0));
        let used = if seen == frame { used } else { 0.0 };
        d.insert_temp(id, (frame, used + height + STACK_GAP));
        used
    })
}

/// Onde abrir uma caixinha flutuante de tamanho `size`: no canto inferior
/// direito da **tela em que a janela do will-ia está**, empilhada acima das
/// outras caixinhas abertas no mesmo quadro.
///
/// No macOS as telas vêm do sistema ([`crate::screens`]), então a caixinha
/// acompanha o monitor da janela e respeita a barra de menus e o Dock. Nos
/// outros sistemas (o egui só informa o tamanho do monitor, não a posição
/// dele), vale o canto do monitor principal.
pub fn corner_pos(ctx: &egui::Context, size: [f32; 2]) -> egui::Pos2 {
    let lift = stack_lift(ctx, size[1]);
    let (outer, minimized, monitor) = ctx.input(|i| {
        let v = i.viewport();
        (v.outer_rect, v.minimized.unwrap_or(false), v.monitor_size)
    });
    // As telas vêm em pontos do sistema; as janelas do egui, em pontos já
    // divididos pelo zoom da interface.
    let zoom = ctx.zoom_factor().max(0.1);
    let center = outer.filter(|_| !minimized).map(|r| r.center());
    let screens = crate::screens::all();
    let screen = match center {
        Some(c) => crate::screens::containing(&screens, egui::pos2(c.x * zoom, c.y * zoom)),
        None => screens.first().copied(),
    };
    match screen {
        Some(s) => egui::pos2(
            s.work.right() / zoom - size[0] - EDGE,
            (s.work.bottom() / zoom - size[1] - EDGE - lift).max(s.work.top() / zoom),
        ),
        None => {
            let m = monitor.unwrap_or(egui::vec2(1920.0, 1080.0));
            egui::pos2(
                (m.x - size[0] - EDGE).max(0.0),
                (m.y - size[1] - 56.0 - lift).max(0.0),
            )
        }
    }
}

/// Deve ser chamado a cada quadro do viewport raiz enquanto a voz estiver ativa.
pub fn show(ctx: &egui::Context, shared: &Shared) {
    let pos = corner_pos(ctx, SIZE);
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
    crate::ui::show_floating(
        ctx,
        "voice-overlay",
        builder,
        SIZE[0],
        egui::Align2::RIGHT_BOTTOM,
        egui::vec2(-16.0, -16.0),
        move |ui| {
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

/// Estado da caixinha do ditado ("Falar" de uma thread). A página de threads
/// escreve título/início/nível; a caixinha devolve os cliques em `send`/`cancel`.
#[derive(Default)]
pub struct DictationState {
    pub title: String,
    pub started: Option<web_time::Instant>,
    pub level: Option<Arc<std::sync::atomic::AtomicU32>>,
    pub transcribing: bool,
    pub send: bool,
    pub cancel: bool,
}

pub type DictationShared = Arc<Mutex<DictationState>>;

pub const DICTATION_SIZE: [f32; 2] = [340.0, 118.0];

/// Caixinha flutuante do ditado: sempre no topo, com o título da thread, o
/// tempo de gravação e os botões Enviar / Descartar. Chamar a cada quadro do
/// viewport raiz enquanto o microfone estiver aberto ou a fala em transcrição.
pub fn show_dictation(ctx: &egui::Context, shared: &DictationShared) {
    let pos = corner_pos(ctx, DICTATION_SIZE);
    let builder = egui::ViewportBuilder::default()
        .with_title(tr("will-ia · falar"))
        .with_inner_size(DICTATION_SIZE)
        .with_min_inner_size(DICTATION_SIZE)
        .with_position(pos)
        .with_decorations(false)
        .with_resizable(false)
        .with_transparent(cfg!(not(target_os = "linux")))
        .with_always_on_top()
        .with_taskbar(false);
    let shared = shared.clone();
    crate::ui::show_floating(
        ctx,
        "dictation-overlay",
        builder,
        DICTATION_SIZE[0],
        egui::Align2::RIGHT_BOTTOM,
        egui::vec2(-16.0, -112.0),
        move |ui| {
            ui.ctx().request_repaint_after(Duration::from_millis(60));
            let mut s = shared.lock();
            let t = ui.input(|i| i.time) as f32;
            let dark = ui.visuals().dark_mode;
            let color = if s.transcribing {
                egui::Color32::from_rgb(220, 160, 40)
            } else {
                egui::Color32::from_rgb(220, 70, 70)
            };
            let bg = if dark {
                egui::Color32::from_rgba_unmultiplied(20, 24, 32, 235)
            } else {
                egui::Color32::from_rgba_unmultiplied(250, 250, 252, 240)
            };
            let mut clicked = false;
            egui::Frame::new()
                .fill(bg)
                .corner_radius(0.0)
                .stroke(egui::Stroke::new(1.0, color.linear_multiply(0.6)))
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.set_min_size(egui::vec2(
                        DICTATION_SIZE[0] - 24.0,
                        DICTATION_SIZE[1] - 24.0,
                    ));
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
                        let c = rect.center();
                        let level = s
                            .level
                            .as_ref()
                            .map(|l| f32::from_bits(l.load(std::sync::atomic::Ordering::Relaxed)))
                            .unwrap_or(0.0);
                        let level = (level * 4.0).clamp(0.0, 1.0);
                        let pulse = 0.5 + 0.5 * (t * 4.0).sin();
                        let grow = if s.transcribing {
                            pulse * 0.6
                        } else {
                            level.max(pulse * 0.25)
                        };
                        ui.painter().circle_filled(
                            c,
                            14.0 + 8.0 * grow,
                            color.linear_multiply(0.25),
                        );
                        ui.painter().circle_filled(c, 10.0, color);
                        ui.add_space(6.0);
                        ui.vertical(|ui| {
                            let head = if s.transcribing {
                                tr("transcrevendo…").to_string()
                            } else {
                                let secs = s.started.map(|i| i.elapsed().as_secs()).unwrap_or(0);
                                trf(
                                    "ouvindo · {}",
                                    &[&format!("{}:{:02}", secs / 60, secs % 60)],
                                )
                            };
                            ui.label(egui::RichText::new(head).strong().size(15.0).color(color));
                            ui.add(
                                egui::Label::new(egui::RichText::new(s.title.clone()).small())
                                    .truncate(),
                            );
                        });
                    });
                    ui.add_space(4.0);
                    if !s.transcribing {
                        ui.horizontal(|ui| {
                            let send = egui::Button::new(
                                egui::RichText::new(tr("Enviar"))
                                    .strong()
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(45, 125, 230))
                            .min_size(egui::vec2(120.0, 28.0));
                            if ui
                                .add(send)
                                .on_hover_text(tr(
                                    "Fecha o microfone, transcreve e envia na thread.",
                                ))
                                .clicked()
                            {
                                s.send = true;
                                clicked = true;
                            }
                            let cancel =
                                egui::Button::new(tr("Descartar")).min_size(egui::vec2(90.0, 28.0));
                            if ui
                                .add(cancel)
                                .on_hover_text(tr("Fecha o microfone sem enviar nada."))
                                .clicked()
                            {
                                s.cancel = true;
                                clicked = true;
                            }
                        });
                    }
                });
            drop(s);
            if clicked {
                // quem consome o clique é a janela principal
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            }
        },
    );
}

/// Estado da caixinha da gravação de conversa. A página de gravações escreve
/// início/níveis/aviso; a caixinha devolve o clique em `stop`.
#[derive(Default)]
pub struct RecordingState {
    pub started: Option<web_time::Instant>,
    pub mic_level: f32,
    pub sys_level: f32,
    /// Por que a saída do sistema não está sendo gravada (só microfone).
    pub warning: Option<String>,
    /// Nenhum som chegou do computador desde o início (depois de alguns
    /// segundos): permissão negada ou nada tocando.
    pub system_silent: bool,
    pub stop: bool,
}

pub type RecordingShared = Arc<Mutex<RecordingState>>;

pub const RECORDING_SIZE: [f32; 2] = [360.0, 150.0];

/// Medidor de nível de uma faixa: rótulo (coluna de largura fixa) + barra.
fn level_bar(ui: &mut egui::Ui, label: &str, level: f32, color: egui::Color32) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (label_rect, _) = ui.allocate_exact_size(egui::vec2(78.0, 14.0), egui::Sense::hover());
        ui.painter().text(
            label_rect.left_center(),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(11.5),
            ui.visuals().weak_text_color(),
        );
        let (rect, _) = ui.allocate_exact_size(egui::vec2(160.0, 6.0), egui::Sense::hover());
        let track = ui.visuals().text_color().linear_multiply(0.15);
        ui.painter().rect_filled(rect, 3.0, track);
        let w = rect.width() * (level * 4.0).clamp(0.0, 1.0);
        if w > 0.5 {
            let fill = egui::Rect::from_min_size(rect.min, egui::vec2(w, rect.height()));
            ui.painter().rect_filled(fill, 3.0, color);
        }
    });
}

/// Caixinha flutuante da gravação de conversa: sempre no topo, com o tempo, o
/// nível das duas faixas (você / computador) e o botão Parar. Chamar a cada
/// quadro do viewport raiz enquanto a gravação estiver aberta.
pub fn show_recording(ctx: &egui::Context, shared: &RecordingShared) {
    let pos = corner_pos(ctx, RECORDING_SIZE);
    let builder = egui::ViewportBuilder::default()
        .with_title(tr("will-ia · gravando"))
        .with_inner_size(RECORDING_SIZE)
        .with_min_inner_size(RECORDING_SIZE)
        .with_position(pos)
        .with_decorations(false)
        .with_resizable(false)
        .with_transparent(cfg!(not(target_os = "linux")))
        .with_always_on_top()
        .with_taskbar(false);
    let shared = shared.clone();
    crate::ui::show_floating(
        ctx,
        "recording-overlay",
        builder,
        RECORDING_SIZE[0],
        egui::Align2::RIGHT_BOTTOM,
        egui::vec2(-16.0, -112.0),
        move |ui| {
            ui.ctx().request_repaint_after(Duration::from_millis(60));
            let mut s = shared.lock();
            let t = ui.input(|i| i.time) as f32;
            let dark = ui.visuals().dark_mode;
            let color = egui::Color32::from_rgb(220, 70, 70);
            let bg = if dark {
                egui::Color32::from_rgba_unmultiplied(20, 24, 32, 235)
            } else {
                egui::Color32::from_rgba_unmultiplied(250, 250, 252, 240)
            };
            let mut clicked = false;
            egui::Frame::new()
                .fill(bg)
                .corner_radius(0.0)
                .stroke(egui::Stroke::new(1.0, color.linear_multiply(0.6)))
                .inner_margin(12.0)
                .show(ui, |ui| {
                    ui.set_min_size(egui::vec2(
                        RECORDING_SIZE[0] - 24.0,
                        RECORDING_SIZE[1] - 24.0,
                    ));
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
                        let c = rect.center();
                        let pulse = 0.5 + 0.5 * (t * 3.0).sin();
                        ui.painter().circle_filled(
                            c,
                            14.0 + 6.0 * pulse,
                            color.linear_multiply(0.25),
                        );
                        ui.painter().circle_filled(c, 10.0, color);
                        ui.add_space(6.0);
                        ui.vertical(|ui| {
                            let secs = s.started.map(|i| i.elapsed().as_secs()).unwrap_or(0);
                            let clock = if secs >= 3600 {
                                format!("{}:{:02}:{:02}", secs / 3600, secs % 3600 / 60, secs % 60)
                            } else {
                                format!("{}:{:02}", secs / 60, secs % 60)
                            };
                            ui.label(
                                egui::RichText::new(trf("gravando · {}", &[&clock]))
                                    .strong()
                                    .size(15.0)
                                    .color(color),
                            );
                            level_bar(
                                ui,
                                tr("você"),
                                s.mic_level,
                                egui::Color32::from_rgb(60, 180, 120),
                            );
                            level_bar(
                                ui,
                                tr("computador"),
                                s.sys_level,
                                egui::Color32::from_rgb(70, 130, 230),
                            );
                        });
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let stop = egui::Button::new(
                            egui::RichText::new(tr("Parar"))
                                .strong()
                                .color(egui::Color32::WHITE),
                        )
                        .fill(color)
                        .min_size(egui::vec2(96.0, 28.0));
                        if ui
                            .add(stop)
                            .on_hover_text(tr("Encerra a gravação e pergunta com quem você falou."))
                            .clicked()
                        {
                            s.stop = true;
                            clicked = true;
                        }
                        let amber = egui::Color32::from_rgb(220, 160, 40);
                        if let Some(w) = &s.warning {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(tr("só microfone"))
                                        .small()
                                        .color(amber),
                                )
                                .truncate(),
                            )
                            .on_hover_text(w);
                        } else if s.system_silent {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(tr("sem som do computador"))
                                        .small()
                                        .color(amber),
                                )
                                .truncate(),
                            )
                            .on_hover_text(tr(
                                "Nenhum som chegou do computador até agora. Se algo está tocando, confira a permissão em Ajustes do Sistema → Privacidade e Segurança → Gravação de Tela e Áudio do Sistema.",
                            ));
                        }
                    });
                });
            drop(s);
            if clicked {
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            }
        },
    );
}
