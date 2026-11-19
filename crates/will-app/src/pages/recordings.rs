//! Gravações de conversa (docs/32): grava microfone + saída do sistema em dois
//! canais, pergunta com quem foi a conversa ao parar, sobe para o servidor e
//! mostra a transcrição ("Você" × quem estava do outro lado).
//!
//! A gravação segue viva com qualquer página aberta: o app chama [`tick`] a
//! cada quadro (atalho global, caixinha flutuante, pergunta ao parar).
//!
//! [`tick`]: RecordingsPage::tick

use super::{Cx, Nav};
use crate::i18n::{tr, trf};
use crate::service::{Loader, Task, UiCommand};
use crate::ui::{self, Tone};
use crate::voice::call_recorder::{CallRecorder, Finished};
use crate::voice::clip_player::{Clip, ClipPlayer};
use crate::voice::overlay::{self, RecordingShared};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use will_core::ThreadId;

/// Gravação parada, à espera de "com quem você falou" e do envio.
struct Finish {
    path: PathBuf,
    started_at: chrono::DateTime<chrono::Utc>,
    duration_secs: f64,
    has_system: bool,
    with: String,
    subject: String,
    /// Falha do último envio (o arquivo local continua guardado).
    error: Option<String>,
    confirm_discard: bool,
    focus: bool,
}

#[derive(Default)]
pub struct RecordingsPage {
    /// Atalho global ou botão pediram começar/parar.
    toggle: bool,
    recorder: Option<CallRecorder>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    overlay: RecordingShared,
    finish: Option<Finish>,
    upload: Option<Task<Value>>,
    list: Loader<Value>,
    selected: Option<String>,
    detail: Loader<Value>,
    /// Gravação cujo detalhe está carregado em `detail`, com o estado dela na
    /// hora da carga (recarrega quando o estado muda na lista).
    detail_of: Option<(String, String)>,
    action: Option<(&'static str, Task<Value>)>,
    confirm_delete: Option<String>,
    /// Edição de "com quem / assunto": id, com quem, assunto.
    edit: Option<(String, String, String)>,
    /// Thread aberta pelo botão "Perguntar": o app navega até ela.
    opened_thread: Option<ThreadId>,
    /// Player da gravação selecionada (id + player) e o áudio em carga.
    player: Option<(String, ClipPlayer)>,
    audio_task: Option<(String, Task<Vec<u8>>)>,
    /// Ponto para onde saltar assim que o áudio carregar (clique numa fala).
    seek_on_load: Option<f64>,
    /// Já avisou, nesta gravação, que o som do computador não está sendo captado.
    system_warned: bool,
}
/// Depois de quanto tempo sem nenhum som do computador a caixinha avisa.
const SILENT_AFTER: Duration = Duration::from_secs(6);
/// Largura da coluna da lista.
const LIST_WIDTH: f32 = 300.0;

/// Pasta local (do app) das gravações ainda não enviadas ao servidor.
fn pending_dir() -> PathBuf {
    crate::profiles::config_dir().join("gravacoes")
}

/// `m:ss` ou `h:mm:ss`.
fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn text_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("")
}

/// Nome mostrado para a gravação: com quem foi, ou um aviso de que falta.
fn title_of(v: &Value) -> String {
    let w = text_of(v, "with_whom").trim();
    if w.is_empty() {
        tr("(sem nome)").to_string()
    } else {
        w.to_string()
    }
}

fn status_badge(ui: &mut egui::Ui, v: &Value) {
    match text_of(v, "status") {
        "transcribing" => ui::badge(ui, tr("transcrevendo…"), Tone::Warning),
        "done" => ui::badge(ui, tr("transcrita"), Tone::Success),
        "error" => ui::badge(ui, tr("erro na transcrição"), Tone::Danger)
            .on_hover_text(text_of(v, "error")),
        _ => ui::badge(ui, tr("sem transcrição"), Tone::Neutral),
    };
}

impl RecordingsPage {
    /// Atalho global da gravação: começa ou para.
    pub fn request_toggle(&mut self) {
        self.toggle = true;
    }

    /// Thread aberta por "Perguntar", para o app navegar até ela.
    pub fn take_opened_thread(&mut self) -> Option<ThreadId> {
        self.opened_thread.take()
    }

    fn start(&mut self, cx: &mut Cx) {
        if self.finish.is_some() || self.upload.is_some() {
            cx.toasts.info(tr(
                "Termine de salvar a gravação anterior antes de começar outra.",
            ));
            return;
        }
        let now = chrono::Local::now();
        let path = pending_dir().join(format!("{}.wav", now.format("%Y%m%d-%H%M%S")));
        let input = cx.snap.settings.realtime.input_device.clone();
        match CallRecorder::start(&path, input.as_deref()) {
            Ok(r) => {
                {
                    let mut s = self.overlay.lock();
                    s.started = Some(r.started);
                    s.warning = r.system_error();
                    s.system_silent = false;
                    s.stop = false;
                }
                self.system_warned = false;
                self.started_at = Some(now.with_timezone(&chrono::Utc));
                self.recorder = Some(r);
            }
            Err(e) => cx.toasts.error(trf("Não foi possível gravar: {}", &[&e])),
        }
    }

    fn stop(&mut self, cx: &mut Cx) {
        let Some(r) = self.recorder.take() else {
            return;
        };
        let started_at = self.started_at.take().unwrap_or_else(chrono::Utc::now);
        match r.stop() {
            Ok(Finished {
                path,
                duration_secs,
                has_system,
            }) => {
                if duration_secs < 1.0 {
                    let _ = std::fs::remove_file(&path);
                    cx.toasts.info(tr("Gravação curta demais; descartada."));
                    return;
                }
                self.finish = Some(Finish {
                    path,
                    started_at,
                    duration_secs,
                    has_system,
                    with: String::new(),
                    subject: String::new(),
                    error: None,
                    confirm_discard: false,
                    focus: true,
                });
                // A pergunta aparece na janela principal: traz ela para a frente.
                cx.svc.send_cmd(UiCommand::Show);
            }
            Err(e) => cx
                .toasts
                .error(trf("A gravação não pôde ser salva: {}", &[&e])),
        }
    }

    /// Chamado a cada quadro pelo app, com qualquer página aberta.
    pub fn tick(&mut self, ctx: &egui::Context, cx: &mut Cx) {
        if std::mem::take(&mut self.toggle) {
            if self.recorder.is_some() {
                self.stop(cx);
            } else {
                self.start(cx);
            }
        }
        if let Some(r) = &mut self.recorder {
            // As barras de volume da caixinha vêm daqui: sem pedir novo quadro, a
            // janela principal parada deixaria as barras congeladas.
            ctx.request_repaint_after(Duration::from_millis(60));
            let system_error = r.system_error();
            if let Some(e) = system_error.as_ref().filter(|_| !self.system_warned) {
                self.system_warned = true;
                cx.toasts.error(trf(
                    "Gravando só o microfone: o áudio do computador não pôde ser capturado ({}).",
                    &[e],
                ));
            }
            let broken = r.runtime_error();
            let stop_clicked = {
                let mut s = self.overlay.lock();
                let (mic, sys) = r.levels();
                s.mic_level = mic;
                s.sys_level = sys;
                s.warning = system_error;
                s.system_silent = !r.system_heard() && r.started.elapsed() >= SILENT_AFTER;
                std::mem::take(&mut s.stop)
            };
            if let Some(e) = broken {
                cx.toasts.error(trf("Gravação interrompida: {}", &[&e]));
                self.stop(cx);
            } else if stop_clicked {
                self.stop(cx);
            }
        }
        if self.recorder.is_some() {
            overlay::show_recording(ctx, &self.overlay);
        }
        self.finish_modal(ctx, cx);
        if let Some(t) = &self.upload {
            if let Some(r) = t.take() {
                self.upload = None;
                match r {
                    Ok(v) => {
                        if let Some(f) = self.finish.take() {
                            let _ = std::fs::remove_file(&f.path);
                        }
                        self.selected = v.get("id").and_then(|i| i.as_str()).map(String::from);
                        self.list.loaded_at = None;
                        cx.toasts.success(tr("Gravação salva. Transcrevendo…"));
                        *cx.go = Some(Nav::Recordings);
                    }
                    Err(e) => {
                        if let Some(f) = &mut self.finish {
                            f.error = Some(e);
                        }
                    }
                }
            }
        }
    }

    /// Pergunta "com quem você falou?" depois de parar.
    fn finish_modal(&mut self, ctx: &egui::Context, cx: &mut Cx) {
        let uploading = self.upload.is_some();
        let Some(f) = &mut self.finish else {
            return;
        };
        let mut save = false;
        let mut discard = false;
        egui::Modal::new(egui::Id::unique("recording-finish")).show(ctx, |ui| {
            let pal = ui::palette(ui.visuals().dark_mode);
            ui.set_width(460.0);
            ui.heading(tr("Com quem você falou?"));
            ui.add_space(4.0);
            let mut info = trf("Gravação de {}.", &[&clock(f.duration_secs)]);
            if !f.has_system {
                info.push(' ');
                info.push_str(tr("Só o microfone foi gravado."));
            }
            ui.label(egui::RichText::new(info).size(12.0).color(pal.text_weak));
            ui.add_space(8.0);
            ui.label(tr("Pessoa, turma ou grupo"));
            let r = ui::text_input(ui, &mut f.with, tr("ex.: André, Turma de sexta, Diretoria"));
            if std::mem::take(&mut f.focus) {
                r.request_focus();
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.label(
                egui::RichText::new(tr(
                    "Esse nome identifica as falas que vieram do computador na transcrição.",
                ))
                .size(11.0)
                .color(pal.text_weak),
            );
            ui.add_space(6.0);
            ui.label(tr("Assunto (opcional)"));
            ui::text_input(ui, &mut f.subject, "");
            if let Some(e) = &f.error {
                ui.add_space(6.0);
                ui::error_text(ui, &trf("Não foi possível salvar: {}", &[e]));
                ui.label(
                    egui::RichText::new(tr("O áudio continua guardado neste computador."))
                        .size(11.0)
                        .color(pal.text_weak),
                );
            }
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if uploading {
                    ui.spinner();
                    ui.label(tr("enviando…"));
                    return;
                }
                if ui::primary_button(ui, tr("Salvar e transcrever")).clicked() || enter {
                    save = true;
                }
                if f.confirm_discard {
                    if ui::danger_button(ui, tr("Confirmar: apagar o áudio")).clicked() {
                        discard = true;
                    }
                    if ui.button(tr("Cancelar")).clicked() {
                        f.confirm_discard = false;
                    }
                } else if ui.button(tr("Descartar")).clicked() {
                    f.confirm_discard = true;
                }
            });
        });
        if discard {
            if let Some(f) = self.finish.take() {
                let _ = std::fs::remove_file(&f.path);
                cx.toasts.info(tr("Gravação descartada."));
            }
        } else if save {
            f.error = None;
            let query = vec![
                ("started_at", f.started_at.to_rfc3339()),
                ("with", f.with.trim().to_string()),
                ("subject", f.subject.trim().to_string()),
                ("has_system", f.has_system.to_string()),
            ];
            let path = f.path.clone();
            self.upload = Some(
                cx.svc
                    .call(move |c| async move { c.upload_recording(path, query).await }),
            );
        }
    }

    fn poll(&mut self, cx: &mut Cx) {
        let transcribing = self
            .list
            .data
            .as_ref()
            .and_then(|v| v.as_array())
            .is_some_and(|a| a.iter().any(|r| text_of(r, "status") == "transcribing"));
        let max_age = if transcribing {
            Duration::from_secs(3)
        } else {
            Duration::from_secs(20)
        };
        if self.list.stale(max_age) {
            self.list
                .start(cx.svc.call(|c| async move { c.recordings().await }));
        }
        self.list.poll();
        // Seleção: mantém a escolhida; sem escolha, a mais recente.
        let rows = self
            .list
            .data
            .as_ref()
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if self
            .selected
            .as_ref()
            .is_none_or(|id| !rows.iter().any(|r| text_of(r, "id") == id))
        {
            self.selected = rows.first().map(|r| text_of(r, "id").to_string());
        }
        // Detalhe: recarrega ao trocar de gravação ou quando o estado dela muda.
        let want = self.selected.as_ref().and_then(|id| {
            rows.iter()
                .find(|r| text_of(r, "id") == id)
                .map(|r| (id.clone(), text_of(r, "status").to_string()))
        });
        if want != self.detail_of && !self.detail.busy() {
            self.detail_of = want.clone();
            self.detail = Loader::default();
            if let Some((id, _)) = want {
                self.detail
                    .start(cx.svc.call(move |c| async move { c.recording(&id).await }));
            }
        }
        self.detail.poll();
        if let Some((what, t)) = &self.action {
            if let Some(r) = t.take() {
                let what = *what;
                self.action = None;
                match r {
                    Ok(v) => {
                        if what == "Perguntar" {
                            self.opened_thread = v
                                .get("thread_id")
                                .and_then(|t| t.as_str())
                                .and_then(|t| ThreadId::parse(t).ok());
                        } else {
                            cx.toasts.success(trf("{}: ok", &[&tr(what)]));
                        }
                        self.list.loaded_at = None;
                        self.detail_of = None;
                    }
                    Err(e) => cx.toasts.error(format!("{}: {e}", tr(what))),
                }
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        self.poll(cx);
        self.poll_audio(cx);
        let pal = ui::palette(ui.visuals().dark_mode);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.heading(tr("Gravações"));
                ui.label(
                    egui::RichText::new(tr(
                        "Sua voz e o som do computador em dois canais, com transcrição, resumo e busca por assunto.",
                    ))
                    .size(12.0)
                    .color(pal.text_weak),
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.recorder.is_some() {
                    if ui::danger_button(ui, tr("Parar gravação")).clicked() {
                        self.toggle = true;
                    }
                } else if ui::primary_button(ui, tr("Gravar conversa")).clicked() {
                    self.toggle = true;
                }
                let hotkey = cx.snap.settings.realtime.recording_hotkey.trim();
                if !hotkey.is_empty() {
                    ui.label(
                        egui::RichText::new(crate::hotkey::display(hotkey))
                            .size(12.0)
                            .color(pal.text_weak),
                    )
                    .on_hover_text(tr(
                        "Atalho global: começa a gravar e, de novo, para. Mude em Configurações → Voz.",
                    ));
                }
            });
        });
        ui.add_space(10.0);
        self.pending_ui(ui);
        if let Some(e) = &self.list.error {
            ui::error_text(ui, e);
        }
        let rows = self
            .list
            .data
            .as_ref()
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if rows.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                let msg = if self.list.data.is_none() {
                    tr("carregando…")
                } else {
                    tr("Nenhuma gravação ainda. Use \"Gravar conversa\" ou o atalho.")
                };
                ui.label(egui::RichText::new(msg).color(pal.text_weak));
            });
        } else {
            let height = ui.available_height();
            let detail_width = (ui.available_width() - LIST_WIDTH - 24.0).max(280.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                // Cada coluna tem o próprio layout de cima para baixo: sem isso
                // os itens herdam o layout horizontal e ficam lado a lado.
                ui.allocate_ui_with_layout(
                    egui::vec2(LIST_WIDTH, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| self.list_ui(ui, &rows),
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(detail_width, height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::Frame::new()
                            .fill(pal.card)
                            .stroke(egui::Stroke::new(1.0, pal.border))
                            .corner_radius(12.0)
                            .inner_margin(16.0)
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.set_min_height((height - 34.0).max(120.0));
                                self.detail_ui(ui, cx);
                            });
                    },
                );
            });
        }
        self.modals(ui.ctx(), cx);
    }

    /// Carga do áudio para o player.
    fn poll_audio(&mut self, cx: &mut Cx) {
        // Trocou de gravação: o player da anterior para.
        if self
            .player
            .as_ref()
            .is_some_and(|(id, _)| self.selected.as_deref() != Some(id.as_str()))
        {
            self.player = None;
        }
        let Some((id, task)) = &self.audio_task else {
            return;
        };
        let Some(r) = task.take() else {
            return;
        };
        let id = id.clone();
        self.audio_task = None;
        if self.selected.as_deref() != Some(id.as_str()) {
            return;
        }
        let output = cx.snap.settings.realtime.output_device.clone();
        let player = r
            .and_then(|wav| Clip::from_wav(&wav))
            .and_then(|clip| ClipPlayer::new(clip, output.as_deref()));
        match player {
            Ok(p) => {
                if let Some(at) = self.seek_on_load.take() {
                    p.seek_secs(at);
                }
                p.set_playing(true);
                self.player = Some((id, p));
            }
            Err(e) => cx.toasts.error(trf("Não foi possível tocar: {}", &[&e])),
        }
    }

    /// Começa a carregar o áudio da gravação `id` (do disco, se o servidor é
    /// esta máquina; senão pela API).
    fn load_audio(&mut self, cx: &mut Cx, id: &str, local_path: &str) {
        if self.audio_task.is_some() {
            return;
        }
        let rid = id.to_string();
        let local = cx
            .snap
            .server_is_local
            .then(|| PathBuf::from(local_path))
            .filter(|p| p.is_file());
        let task = cx.svc.call(move |c| async move {
            match local {
                Some(p) => tokio::fs::read(&p).await.map_err(|e| e.to_string()),
                None => c.recording_audio(&rid).await,
            }
        });
        self.audio_task = Some((id.to_string(), task));
    }

    /// Toca a partir de `secs` (carregando o áudio antes, se preciso).
    fn play_from(&mut self, cx: &mut Cx, id: &str, local_path: &str, secs: f64) {
        match &self.player {
            Some((pid, p)) if pid == id => {
                p.seek_secs(secs);
                p.set_playing(true);
            }
            _ => {
                self.seek_on_load = Some(secs);
                self.load_audio(cx, id, local_path);
            }
        }
    }

    /// Áudios gravados que ainda não foram enviados (envio falhou ou o app fechou).
    fn pending_ui(&mut self, ui: &mut egui::Ui) {
        if self.finish.is_some() || self.recorder.is_some() {
            return;
        }
        let Ok(dir) = std::fs::read_dir(pending_dir()) else {
            return;
        };
        let mut files: Vec<PathBuf> = dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "wav"))
            .collect();
        files.sort();
        for path in files {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let duration_secs = size.saturating_sub(44) as f64 / 64_000.0;
            let name = path
                .file_stem()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            ui.horizontal(|ui| {
                ui::badge(ui, tr("não enviada"), Tone::Warning);
                ui.label(trf("{} · {}", &[&name, &clock(duration_secs)]));
                if ui.button(tr("Salvar…")).clicked() {
                    let started_at = chrono::NaiveDateTime::parse_from_str(&name, "%Y%m%d-%H%M%S")
                        .ok()
                        .and_then(|n| n.and_local_timezone(chrono::Local).single())
                        .map(|d| d.with_timezone(&chrono::Utc))
                        .unwrap_or_else(chrono::Utc::now);
                    self.finish = Some(Finish {
                        path: path.clone(),
                        started_at,
                        duration_secs,
                        has_system: true,
                        with: String::new(),
                        subject: String::new(),
                        error: None,
                        confirm_discard: false,
                        focus: true,
                    });
                }
            });
            ui.add_space(6.0);
        }
    }

    fn list_ui(&mut self, ui: &mut egui::Ui, rows: &[Value]) {
        let pal = ui::palette(ui.visuals().dark_mode);
        egui::ScrollArea::vertical()
            .id_salt("recordings-list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                for r in rows {
                    let id = text_of(r, "id").to_string();
                    let selected = self.selected.as_deref() == Some(id.as_str());
                    let (fill, stroke) = if selected {
                        (pal.card_alt, egui::Stroke::new(1.5, pal.accent))
                    } else {
                        (pal.card, egui::Stroke::new(1.0, pal.border))
                    };
                    let resp = egui::Frame::new()
                        .fill(fill)
                        .stroke(stroke)
                        .corner_radius(10.0)
                        .inner_margin(12.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing.y = 4.0;
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(title_of(r)).font(ui::semibold(14.0)),
                                )
                                .truncate()
                                .selectable(false),
                            );
                            let subject = text_of(r, "subject").trim();
                            if !subject.is_empty() {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(subject)
                                            .size(12.0)
                                            .color(pal.text_weak),
                                    )
                                    .truncate()
                                    .selectable(false),
                                );
                            }
                            let secs = r
                                .get("duration_secs")
                                .and_then(|d| d.as_f64())
                                .unwrap_or(0.0);
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!(
                                        "{} · {}",
                                        ui::fmt_dt(&r["started_at"]),
                                        clock(secs)
                                    ))
                                    .size(11.0)
                                    .color(pal.text_weak),
                                )
                                .selectable(false),
                            );
                            status_badge(ui, r);
                        })
                        .response
                        .interact(egui::Sense::click())
                        .on_hover_cursor(egui::CursorIcon::PointingHand);
                    if resp.clicked() {
                        self.selected = Some(id);
                    }
                }
            });
    }

    /// Barra do player: tocar/pausar, tempo e a barra para arrastar.
    fn player_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, id: &str, path: &str, total: f64) {
        let pal = ui::palette(ui.visuals().dark_mode);
        let loaded = self.player.as_ref().filter(|(pid, _)| pid == id);
        let loading = self.audio_task.as_ref().is_some_and(|(aid, _)| aid == id);
        let playing = loaded.is_some_and(|(_, p)| p.playing());
        let mut pos = loaded.map(|(_, p)| p.position_secs()).unwrap_or(0.0);
        let total = loaded.map(|(_, p)| p.duration_secs()).unwrap_or(total);
        let mut toggle = false;
        let mut seek: Option<f64> = None;
        egui::Frame::new()
            .fill(pal.card_alt)
            .corner_radius(10.0)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if loading {
                        ui.add(egui::Spinner::new().size(16.0));
                    } else {
                        let (icon, tip) = if playing {
                            (crate::icons::PAUSE, tr("Pausar"))
                        } else {
                            (
                                crate::icons::PLAY,
                                tr("Ouvir: você num lado, o computador no outro"),
                            )
                        };
                        if ui::icon_button_colored(ui, icon, tip, pal.accent)
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            toggle = true;
                        }
                    }
                    ui.label(
                        egui::RichText::new(clock(pos))
                            .size(11.5)
                            .monospace()
                            .color(pal.text_weak),
                    );
                    let right = 56.0;
                    ui.spacing_mut().slider_width = (ui.available_width() - right).max(80.0);
                    let slider = ui.add(
                        egui::Slider::new(&mut pos, 0.0..=total.max(0.1))
                            .show_value(false)
                            .trailing_fill(true),
                    );
                    if slider.changed() {
                        seek = Some(pos);
                    }
                    ui.label(
                        egui::RichText::new(clock(total))
                            .size(11.5)
                            .monospace()
                            .color(pal.text_weak),
                    );
                });
            });
        if let Some(to) = seek {
            match &self.player {
                Some((pid, p)) if pid == id => p.seek_secs(to),
                // Arrastou antes de carregar: carrega e começa dali.
                _ => self.play_from(cx, id, path, to),
            }
        }
        if toggle {
            match &self.player {
                Some((pid, p)) if pid == id => p.set_playing(!p.playing()),
                _ => self.play_from(cx, id, path, 0.0),
            }
        }
        if playing {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
    }

    fn detail_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        let pal = ui::palette(ui.visuals().dark_mode);
        let Some(d) = self.detail.data.clone() else {
            if let Some(e) = &self.detail.error {
                ui::error_text(ui, e);
            } else {
                ui.label(egui::RichText::new(tr("carregando…")).color(pal.text_weak));
            }
            return;
        };
        let r = &d["recording"];
        let id = text_of(r, "id").to_string();
        let status = text_of(r, "status").to_string();
        let audio_path = text_of(r, "audio_path").to_string();
        let busy = self.action.is_some();
        let secs = r
            .get("duration_secs")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0);
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(egui::RichText::new(title_of(r)).heading()).truncate());
            status_badge(ui, r);
        });
        let mut meta = format!("{} · {}", ui::fmt_dt(&r["started_at"]), clock(secs));
        let subject = text_of(r, "subject").trim();
        if !subject.is_empty() {
            meta.push_str(" · ");
            meta.push_str(subject);
        }
        ui.label(egui::RichText::new(meta).size(12.0).color(pal.text_weak));
        if r.get("has_system").and_then(|x| x.as_bool()) == Some(false) {
            ui.label(
                egui::RichText::new(tr(
                    "Só o microfone foi gravado: o áudio do computador não pôde ser capturado.",
                ))
                .size(12.0)
                .color(Tone::Warning.color(ui.visuals().dark_mode)),
            );
        }
        if status == "error" {
            ui::error_text(ui, text_of(r, "error"));
        }
        ui.add_space(8.0);
        self.player_ui(ui, cx, &id, &audio_path, secs);
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if status == "done"
                    && ui::primary_button(ui, tr("Perguntar sobre esta conversa"))
                        .on_hover_text(tr(
                            "Abre uma thread com a transcrição e pede um resumo; depois é só perguntar.",
                        ))
                        .clicked()
                {
                    let rid = id.clone();
                    self.action = Some((
                        "Perguntar",
                        cx.svc
                            .call(move |c| async move { c.ask_recording(&rid).await }),
                    ));
                }
                if status != "transcribing" {
                    let label = if status == "done" {
                        tr("Transcrever de novo")
                    } else {
                        tr("Transcrever")
                    };
                    if ui.button(label).clicked() {
                        let rid = id.clone();
                        self.action = Some((
                            "Transcrever",
                            cx.svc
                                .call(move |c| async move { c.transcribe_recording(&rid).await }),
                        ));
                    }
                }
                if ui.button(tr("Editar nome")).clicked() {
                    self.edit = Some((
                        id.clone(),
                        text_of(r, "with_whom").to_string(),
                        text_of(r, "subject").to_string(),
                    ));
                }
                if cx.snap.server_is_local && ui.button(tr("Abrir pasta")).clicked() {
                    let path = PathBuf::from(text_of(r, "folder_path"));
                    if let Err(e) = will_platform::open_path(&path) {
                        cx.toasts.error(e);
                    }
                }
                if status != "transcribing" && ui.button(tr("Apagar")).clicked() {
                    self.confirm_delete = Some(id.clone());
                }
            });
        });
        ui.add_space(10.0);
        let other = text_of(&d, "other_label").to_string();
        let segments = d["segments"].as_array().cloned().unwrap_or_default();
        let summary = text_of(r, "summary").trim().to_string();
        let indexed = d["indexed_chunks"].as_i64().unwrap_or(0);
        let mut play_at: Option<f64> = None;
        egui::ScrollArea::vertical()
            .id_salt("recording-detail")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if !summary.is_empty() {
                    egui::Frame::new()
                        .fill(pal.card_alt)
                        .corner_radius(10.0)
                        .inner_margin(12.0)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(tr("Resumo")).font(ui::semibold(13.0)));
                                if indexed > 0 {
                                    ui::badge(ui, tr("na busca"), Tone::Info).on_hover_text(tr(
                                        "Esta conversa está indexada: o agente a encontra por assunto com a ferramenta recordings_search.",
                                    ));
                                }
                            });
                            ui.add_space(2.0);
                            ui.add(egui::Label::new(&summary).wrap());
                        });
                    ui.add_space(12.0);
                }
                if segments.is_empty() {
                    let msg = match status.as_str() {
                        "transcribing" => {
                            tr("Transcrevendo… isso leva alguns minutos em gravações longas.")
                        }
                        "done" => tr("Nenhuma fala reconhecida nesta gravação."),
                        _ => tr("Esta gravação ainda não foi transcrita."),
                    };
                    ui.label(egui::RichText::new(msg).color(pal.text_weak));
                    return;
                }
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tr("Transcrição")).font(ui::semibold(13.0)));
                    if ui.small_button(tr("Copiar")).clicked() {
                        ui.ctx().copy_text(text_of(&d, "text").to_string());
                        cx.toasts.success(tr("Transcrição copiada."));
                    }
                });
                ui.add_space(4.0);
                let width = ui.available_width();
                let bubble = (width * 0.82).min(720.0);
                for s in &segments {
                    let me = text_of(s, "speaker") == "me";
                    let start = s.get("start").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    let (who, name_color, fill) = if me {
                        (
                            tr("Você").to_string(),
                            pal.accent,
                            pal.accent.linear_multiply(0.14),
                        )
                    } else {
                        (
                            other.clone(),
                            egui::Color32::from_rgb(60, 170, 120),
                            pal.card_alt,
                        )
                    };
                    // As suas falas ficam recuadas à direita, como num chat.
                    // O recuo vem da largura real do balão (medida pelo texto), para a
                    // fala curta também encostar na borda direita.
                    let indent = if me {
                        let inner = bubble - 24.0;
                        let body = egui::TextStyle::Body.resolve(ui.style());
                        let text_w = ui
                            .painter()
                            .layout(text_of(s, "text").to_string(), body, pal.text, inner)
                            .size()
                            .x;
                        let who_w = ui
                            .painter()
                            .layout_no_wrap(who.clone(), ui::medium(12.0), pal.text)
                            .size()
                            .x;
                        let clock_w = ui
                            .painter()
                            .layout_no_wrap(clock(start), egui::FontId::proportional(11.0), pal.text)
                            .size()
                            .x;
                        let head_w = who_w + ui.spacing().item_spacing.x + clock_w;
                        let content = text_w.max(head_w).min(inner).ceil();
                        (width - content - 24.0 - 2.0).max(0.0)
                    } else {
                        0.0
                    };
                    ui.horizontal(|ui| {
                        ui.add_space(indent);
                        let resp = egui::Frame::new()
                            .fill(fill)
                            .corner_radius(10.0)
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.set_max_width(bubble - 24.0);
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(who)
                                                .font(ui::medium(12.0))
                                                .color(name_color),
                                        );
                                        ui.label(
                                            egui::RichText::new(clock(start))
                                                .size(11.0)
                                                .color(pal.text_weak),
                                        );
                                    });
                                    ui.add(egui::Label::new(text_of(s, "text")).wrap());
                                });
                            })
                            .response
                            .interact(egui::Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text(tr("Clique para ouvir a partir desta fala"));
                        if resp.clicked() {
                            play_at = Some(start);
                        }
                    });
                    ui.add_space(6.0);
                }
            });
        if let Some(at) = play_at {
            self.play_from(cx, &id, &audio_path, at);
        }
    }

    fn modals(&mut self, ctx: &egui::Context, cx: &mut Cx) {
        if let Some(id) = self.confirm_delete.clone() {
            let c = ui::Confirm {
                title: tr("Apagar a gravação").into(),
                body: tr("Apaga o áudio e a transcrição desta conversa. Isso não tem volta.")
                    .into(),
                confirm_label: tr("Apagar").into(),
                danger: true,
                tag: "delete-recording".into(),
            };
            if let Some(ok) = ui::confirm_modal(ctx, &c) {
                self.confirm_delete = None;
                if ok {
                    self.selected = None;
                    self.action = Some((
                        "Apagar",
                        cx.svc
                            .call(move |c| async move { c.delete_recording(&id).await }),
                    ));
                }
            }
        }
        let mut save = false;
        let mut cancel = false;
        if let Some((_, with, subject)) = &mut self.edit {
            let m = egui::Modal::new(egui::Id::unique("recording-edit")).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading(tr("Com quem você falou?"));
                ui.add_space(6.0);
                ui.label(tr("Pessoa, turma ou grupo"));
                ui::text_input(ui, with, "");
                ui.add_space(6.0);
                ui.label(tr("Assunto (opcional)"));
                ui::text_input(ui, subject, "");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(tr("Cancelar")).clicked() {
                        cancel = true;
                    }
                    if ui::primary_button(ui, tr("Salvar")).clicked() {
                        save = true;
                    }
                });
            });
            if m.should_close() {
                cancel = true;
            }
        }
        if save {
            if let Some((id, with, subject)) = self.edit.take() {
                self.action = Some((
                    "Salvar",
                    cx.svc.call(move |c| async move {
                        c.set_recording_labels(&id, with.trim(), subject.trim())
                            .await
                    }),
                ));
            }
        } else if cancel {
            self.edit = None;
        }
    }
}
