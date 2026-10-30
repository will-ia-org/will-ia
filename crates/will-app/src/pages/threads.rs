//! Threads: lista à esquerda, conversa à direita (histórico + turno ao vivo),
//! envio imediato (inclusive no meio de um turno), reiniciar/reset/interromper,
//! renomear e apagar.

use super::Cx;
use crate::i18n::{tr, trf};
use crate::service::{Loader, Service, Task};
use crate::ui::{self, Tone};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use will_core::{EngineDelta, Event, ThreadId, ThreadStatus, ThreadSummary, TurnResult};

/// Teto de um anexo enviado para um servidor remoto (`POST /api/uploads`).
const UPLOAD_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Acima disso não se baixa a imagem só para a miniatura (modo remoto).
const THUMB_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Anexos de um servidor remoto: os caminhos são de lá, então miniaturas e
/// "abrir" passam por `GET /api/files`. Bytes baixados ficam em cache por
/// caminho enquanto a página viver.
#[derive(Default)]
struct RemoteFiles {
    cache: HashMap<String, Arc<[u8]>>,
    /// Downloads de miniatura em voo, por caminho.
    loading: HashMap<String, Task<(String, Vec<u8>)>>,
    /// Caminhos cuja miniatura falhou (não tentar de novo a cada quadro).
    failed: HashMap<String, String>,
    /// Baixar + gravar em pasta temporária + abrir no app padrão.
    opening: Vec<Task<()>>,
}

/// Item da linha do tempo do turno ao vivo, já organizado a partir dos
/// `EngineDelta`: fala do assistente em bloco, ferramenta com o resultado
/// anexado, status e raciocínio. `at` serve à animação de entrada.
#[derive(Clone, Debug)]
enum LiveItem {
    Say {
        text: String,
        at: std::time::Instant,
    },
    Tool {
        name: String,
        input: String,
        /// `(prévia, falhou)` quando o resultado chegou.
        result: Option<(String, bool)>,
        at: std::time::Instant,
        done_at: Option<std::time::Instant>,
    },
    Status {
        text: String,
        at: std::time::Instant,
    },
    Think {
        text: String,
        at: std::time::Instant,
    },
    /// Mensagem do usuário aceita no meio do turno (steering).
    User {
        text: String,
        at: std::time::Instant,
    },
}

#[derive(Clone, Default)]
struct LiveTurn {
    turn_id: Option<String>,
    items: Vec<LiveItem>,
    /// Texto em streaming ainda não fechado em bloco (`TextDelta`).
    text: String,
    started: Option<std::time::Instant>,
    /// Duração total quando o turno terminou (cartão vira faixa recolhida).
    finished_secs: Option<u64>,
}

impl LiveTurn {
    /// Fecha o texto em streaming num bloco "fala" (antes de uma ferramenta).
    fn flush_text(&mut self) {
        if !self.text.trim().is_empty() {
            self.items.push(LiveItem::Say {
                text: std::mem::take(&mut self.text),
                at: std::time::Instant::now(),
            });
        } else {
            self.text.clear();
        }
    }

    fn push(&mut self, d: &EngineDelta) {
        let now = std::time::Instant::now();
        match d {
            EngineDelta::TextDelta { text } => self.text.push_str(text),
            // Bloco completo: substitui o streaming do mesmo trecho.
            EngineDelta::Narration { text } => {
                self.text.clear();
                if !text.trim().is_empty() {
                    self.items.push(LiveItem::Say {
                        text: text.clone(),
                        at: now,
                    });
                }
            }
            EngineDelta::ToolUse {
                name,
                input_preview,
            } => {
                self.flush_text();
                self.items.push(LiveItem::Tool {
                    name: name.clone(),
                    input: input_preview.clone(),
                    result: None,
                    at: now,
                    done_at: None,
                });
            }
            EngineDelta::ToolResult { preview, is_error } => {
                // anexa à ferramenta mais recente ainda sem resultado
                let open = self
                    .items
                    .iter()
                    .rposition(|i| matches!(i, LiveItem::Tool { result: None, .. }));
                match open {
                    Some(p) => {
                        if let LiveItem::Tool {
                            result, done_at, ..
                        } = &mut self.items[p]
                        {
                            *result = Some((preview.clone(), *is_error));
                            *done_at = Some(now);
                        }
                    }
                    None => self.items.push(LiveItem::Status {
                        text: preview.clone(),
                        at: now,
                    }),
                }
            }
            EngineDelta::Status { text } => self.items.push(LiveItem::Status {
                text: text.clone(),
                at: now,
            }),
            EngineDelta::Thinking { text } => self.items.push(LiveItem::Think {
                text: text.clone(),
                at: now,
            }),
        }
    }

    fn tools_total(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i, LiveItem::Tool { .. }))
            .count()
    }

    /// Subagentes (`Task`/`Agent`) ainda sem resultado — seguem em segundo plano
    /// mesmo depois de o turno responder.
    fn open_tasks(&self) -> usize {
        self.items
            .iter()
            .filter(
                |i| matches!(i, LiveItem::Tool { name, result: None, .. } if is_task_tool(name)),
            )
            .count()
    }

    fn tools_running(&self) -> usize {
        self.items
            .iter()
            .filter(|i| matches!(i, LiveItem::Tool { result: None, .. }))
            .count()
    }
}

/// Mensagem que o usuário acabou de enviar, mostrada no chat **antes** de o
/// servidor confirmar (eco otimista). Some quando o turno real aparece na lista.
#[derive(Clone, Debug)]
struct Echo {
    local_id: u64,
    text: String,
    files: Vec<std::path::PathBuf>,
    sent_at: chrono::DateTime<chrono::Utc>,
    /// `turn_id` devolvido pelo `POST …/messages` (quando já respondeu).
    turn_id: Option<String>,
    /// Instante em que o servidor aceitou: uma lista carregada depois disso já
    /// contém o turno, então o eco pode sair mesmo sem casar o id.
    acked_at: Option<std::time::Instant>,
    /// Falha no envio: o balão fica com aviso e botões "Reenviar"/"Descartar".
    error: Option<String>,
}

/// Prompt visível de um turno da API (sem a nota de anexo).
fn turn_prompt_display(t: &Value) -> String {
    t.get("prompt_display")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| ui::s(t, "prompt"))
}

/// Remove os ecos que a lista de turnos (recém-carregada em `loaded_at`) já
/// cobre: pelo `turn_id`, por uma lista mais nova que a confirmação, ou — sem
/// confirmação ainda — por um turno com o mesmo texto iniciado depois do envio
/// (a lista pode ter sido recarregada por `TurnStarted` antes do POST voltar).
/// Ecos com erro ficam até o usuário reenviar ou descartar.
fn reconcile_echoes(echoes: &mut Vec<Echo>, turns: &[Value], loaded_at: std::time::Instant) {
    echoes.retain(|e| {
        if e.error.is_some() {
            return true;
        }
        if let Some(id) = &e.turn_id {
            if turns.iter().any(|t| ui::s(t, "id") == *id) {
                return false;
            }
        }
        if e.acked_at.is_some_and(|a| loaded_at > a) {
            return false;
        }
        let tol = chrono::Duration::seconds(15);
        let matched = turns.iter().any(|t| {
            turn_prompt_display(t).trim() == e.text.trim()
                && turn_dt(t, "started_at").is_some_and(|dt| dt >= e.sent_at - tol)
        });
        !matched
    });
}

/// Tipo MIME aproximado pela extensão (só para a miniatura do eco).
fn guess_mime(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        Some("txt" | "md") => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Texto e anexos ainda não enviados de uma thread.
#[derive(Clone, Debug, Default)]
struct Draft {
    text: String,
    files: Vec<std::path::PathBuf>,
}

/// Junta a transcrição ao que já estava no rascunho (um espaço entre eles).
fn merge_dictation(draft: &str, text: &str) -> String {
    let mut out = draft.to_string();
    if !out.is_empty() && !out.ends_with(char::is_whitespace) {
        out.push(' ');
    }
    out.push_str(text);
    out
}

/// O que o usuário pediu num eco com erro.
enum EchoAction {
    Resend(u64),
    Discard(u64),
}

#[derive(Default)]
pub struct ThreadsPage {
    list: Loader<Value>,
    threads: Vec<ThreadSummary>,
    selected: Option<ThreadId>,
    turns: Loader<Value>,
    turns_for: Option<ThreadId>,
    live: HashMap<ThreadId, LiveTurn>,
    notes: HashMap<ThreadId, Vec<String>>,
    input: String,
    /// Arquivos escolhidos (botão Anexar ou arrastar-e-soltar) ainda não enviados.
    pending_files: Vec<std::path::PathBuf>,
    /// Envios em voo: thread de origem, id do eco e a tarefa (um por thread).
    send_tasks: Vec<(ThreadId, u64, Task<Value>)>,
    /// Rascunho (texto + anexos pendentes) das threads que não estão abertas;
    /// o da thread aberta vive em `input`/`pending_files` (ver `sync_draft`).
    drafts: HashMap<ThreadId, Draft>,
    /// Thread a que `input`/`pending_files` pertencem neste momento.
    draft_for: Option<ThreadId>,
    /// Ecos otimistas por thread (mensagens enviadas e ainda não refletidas na lista).
    echoes: HashMap<ThreadId, Vec<Echo>>,
    next_echo: u64,
    /// Forçar a rolagem até o fim no próximo quadro (acabou de enviar).
    scroll_bottom: bool,
    action_task: Option<(&'static str, Task<Value>)>,
    create_task: Option<Task<Value>>,
    rename_buf: Option<String>,
    confirm_delete: Option<(ThreadId, bool)>,
    confirm_reset: Option<ThreadId>,
    show_archived: bool,
    focus_input: bool,
    /// Ditado (botão "Falar" segurado): gravador ativo e transcrição em voo.
    dictation: Option<crate::voice::audio::Recorder>,
    /// Thread em que o "Falar" foi pressionado: a transcrição vai para ELA,
    /// mesmo que o usuário abra outra thread enquanto o áudio é transcrito.
    dictation_for: Option<ThreadId>,
    dictation_task: Option<(ThreadId, Task<Value>)>,
    /// "Ouvir resumo" de uma resposta: resumo → TTS → reprodução (um por vez).
    speak: Option<SpeakJob>,
    /// Roteiros/áudios já gerados por turno (memória + disco): ouvir de novo não
    /// refaz resumo nem TTS.
    speak_cache: crate::speak_cache::SpeakCache,
    /// Linhas do tempo de turnos já concluídos (id do turno → atividade), mantidas
    /// em memória para a faixa recolhida acima da resposta.
    done_live: HashMap<String, LiveTurn>,
    /// Último turno concluído por thread (para anexar resultados tardios de
    /// subagentes e mostrar "em segundo plano" na lista).
    done_by_thread: HashMap<ThreadId, String>,
    /// Eventos de turnos antigos sendo baixados do servidor (id do turno → tarefa),
    /// para reconstruir a faixa de atividade retroativamente.
    loading_events: HashMap<String, Task<Value>>,
    /// Triggers por thread alvo (id → "nome (fonte)"), para o ícone de raio.
    triggers: Loader<Value>,
    trigger_map: HashMap<String, Vec<String>>,
    /// Altura medida do bloco de entrada no quadro anterior (define quanto
    /// sobra para o histórico sem a página rolar).
    bottom_h: f32,
    md_cache: egui_commonmark::CommonMarkCache,
    remote: RemoteFiles,
}

fn origin_label(t: &ThreadSummary) -> String {
    match &t.origin {
        will_core::Origin::Ui => "app".into(),
        will_core::Origin::Telegram { chat_id, topic_id } => match topic_id {
            Some(tp) => format!("telegram {chat_id}:{tp}"),
            None => format!("telegram {chat_id}"),
        },
        will_core::Origin::Realtime => tr("voz").into(),
    }
}

fn title_of(t: &ThreadSummary) -> String {
    t.title
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("Thread {}", &t.id.to_string()[..8]))
}

fn status_tone(t: &ThreadSummary) -> Tone {
    match t.status {
        // processando: âmbar (a lista/cabeçalho usam a bolinha pulsante)
        ThreadStatus::Running => Tone::Warning,
        ThreadStatus::Live => Tone::Success,
        ThreadStatus::Error => Tone::Danger,
        ThreadStatus::Archived => Tone::Neutral,
        ThreadStatus::Idle => Tone::Neutral,
    }
}

/// Tempo relativo (traduzido): "agora", "há 1 min", "há 5 min", "há 2 h",
/// "há 1 dia", "há 3 dias", "há 2 sem".
fn time_ago(dt: &chrono::DateTime<chrono::Utc>) -> String {
    let secs = (chrono::Utc::now() - *dt).num_seconds().max(0);
    let mins = secs / 60;
    let hours = secs / 3600;
    let days = secs / 86400;
    if secs < 60 {
        tr("agora").to_string()
    } else if mins < 60 {
        trf("há {} min", &[&mins])
    } else if hours < 24 {
        trf("há {} h", &[&hours])
    } else if days == 1 {
        tr("há 1 dia").to_string()
    } else if days < 7 {
        trf("há {} dias", &[&days])
    } else {
        trf("há {} sem", &[&(days / 7)])
    }
}

/// Data/hora exata, no fuso local, para tooltips.
fn fmt_exact(dt: &chrono::DateTime<chrono::Utc>) -> String {
    dt.with_timezone(&chrono::Local)
        .format("%d/%m/%Y %H:%M:%S")
        .to_string()
}

/// Lê um campo RFC3339 do JSON do turno (`started_at`/`finished_at`).
fn turn_dt(turn: &Value, key: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    turn.get(key)
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// Rótulo discreto de tempo relativo; o hover mostra a data exata.
fn when_label(ui: &mut egui::Ui, pal: &ui::Palette, dt: Option<&chrono::DateTime<chrono::Utc>>) {
    if let Some(dt) = dt {
        ui.add(
            egui::Label::new(
                egui::RichText::new(time_ago(dt))
                    .size(11.0)
                    .color(pal.text_weak),
            )
            .sense(egui::Sense::hover()),
        )
        .on_hover_text(fmt_exact(dt));
    }
}

/// Glifo discreto da origem da thread, desenhado com o painter (sem depender de
/// fontes de ícones): aviãozinho = Telegram, janelinha = criada no app, microfone = voz.
fn origin_icon(ui: &mut egui::Ui, pal: &ui::Palette, t: &ThreadSummary) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(13.0, 13.0), egui::Sense::hover());
    let color = if resp.hovered() {
        pal.text
    } else {
        pal.text_weak
    };
    let st = egui::Stroke::new(1.1, color);
    let p = ui.painter();
    let c = rect.center();
    match &t.origin {
        will_core::Origin::Telegram { .. } => {
            // aviãozinho de papel: triângulo apontando para a direita + dobra
            let a = egui::pos2(rect.left() + 1.0, c.y - 0.5);
            let b = egui::pos2(rect.right() - 1.0, rect.top() + 1.5);
            let d = egui::pos2(rect.left() + 5.5, rect.bottom() - 1.0);
            let m = egui::pos2(rect.left() + 5.5, c.y + 1.0);
            p.line_segment([a, b], st);
            p.line_segment([b, d], st);
            p.line_segment([d, m], st);
            p.line_segment([m, a], st);
            p.line_segment([m, b], st);
        }
        will_core::Origin::Realtime => {
            // microfone: cápsula + haste + base
            let cap = egui::Rect::from_center_size(
                egui::pos2(c.x, rect.top() + 4.5),
                egui::vec2(4.5, 7.0),
            );
            p.rect_stroke(cap, 2.0, st, egui::StrokeKind::Middle);
            p.line_segment(
                [
                    egui::pos2(c.x, rect.top() + 8.5),
                    egui::pos2(c.x, rect.bottom() - 1.0),
                ],
                st,
            );
            p.line_segment(
                [
                    egui::pos2(c.x - 3.0, rect.bottom() - 1.0),
                    egui::pos2(c.x + 3.0, rect.bottom() - 1.0),
                ],
                st,
            );
        }
        will_core::Origin::Ui => {
            // janelinha do app: retângulo com barra de título
            let r = rect.shrink(1.5);
            p.rect_stroke(r, 2.0, st, egui::StrokeKind::Middle);
            p.line_segment(
                [
                    egui::pos2(r.left(), r.top() + 3.0),
                    egui::pos2(r.right(), r.top() + 3.0),
                ],
                st,
            );
        }
    }
    resp.on_hover_text(origin_label(t))
}

/// Raio discreto: a thread é alvo de uma ou mais triggers (hover lista quais).
fn trigger_icon(ui: &mut egui::Ui, pal: &ui::Palette, names: &[String]) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(11.0, 13.0), egui::Sense::hover());
    let color = if resp.hovered() {
        pal.accent
    } else {
        pal.text_weak
    };
    let st = egui::Stroke::new(1.2, color);
    let p = ui.painter();
    let r = rect;
    let pts = [
        egui::pos2(r.left() + 7.0, r.top() + 0.5),
        egui::pos2(r.left() + 2.0, r.top() + 7.0),
        egui::pos2(r.left() + 6.0, r.top() + 7.0),
        egui::pos2(r.left() + 4.0, r.bottom() - 0.5),
        egui::pos2(r.right() - 1.0, r.top() + 5.5),
        egui::pos2(r.left() + 5.5, r.top() + 5.5),
        egui::pos2(r.left() + 7.0, r.top() + 0.5),
    ];
    for w in pts.windows(2) {
        p.line_segment([w[0], w[1]], st);
    }
    resp.on_hover_text(format!("{}\n{}", tr("Alvo de trigger:"), names.join("\n")))
}

/// Conteúdo do "i" do cabeçalho: origem, status, custo, sessão, atividade.
fn thread_info_body(ui: &mut egui::Ui, pal: &ui::Palette, t: &ThreadSummary) {
    ui.set_min_width(260.0);
    let row = |ui: &mut egui::Ui, k: &str, v: String| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(k).small().color(pal.text_weak));
            ui.add(egui::Label::new(egui::RichText::new(v).small()).wrap());
        });
    };
    ui.horizontal(|ui| {
        ui::dot(ui, status_tone(t));
        ui.label(egui::RichText::new(t.status.as_str()).small().strong());
    });
    row(ui, tr("origem"), origin_label(t));
    row(ui, tr("custo"), ui::fmt_usd(t.total_cost_usd));
    if let Some(sid) = &t.claude_session_id {
        row(ui, tr("sessão"), sid.clone());
    }
    row(
        ui,
        tr("processo"),
        if t.alive {
            tr("vivo").into()
        } else {
            tr("dormente").into()
        },
    );
    if t.pending > 0 {
        row(ui, tr("pendentes"), t.pending.to_string());
    }
    row(ui, tr("criada"), fmt_exact(&t.created_at));
    row(ui, tr("atividade"), fmt_exact(&t.last_activity_at));
    row(ui, tr("id"), t.id.to_string());
}

/// Texto que quebra em até `max_rows` linhas e trunca o resto com "…"
/// (resumos na lista e no cabeçalho). `max_width` = largura disponível.
fn wrapped_label(
    ui: &mut egui::Ui,
    text: &str,
    font_id: egui::FontId,
    color: egui::Color32,
    italics: bool,
    max_rows: usize,
    max_width: f32,
) -> egui::Response {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat {
            font_id,
            color,
            italics,
            ..Default::default()
        },
    );
    job.wrap = egui::text::TextWrapping {
        max_width: max_width.max(24.0),
        max_rows,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    // `.wrap()` é obrigatório: num layout horizontal o Label usa `Extend` e
    // troca o `max_width` do job por infinito (só `max_rows` sobrevive).
    ui.scope(|ui| {
        ui.set_max_width(max_width.max(24.0));
        ui.add(egui::Label::new(job).wrap())
    })
    .inner
}

/// Botão pequeno "copiar" com área de clique real (moldura só no hover).
fn copy_button(ui: &mut egui::Ui, pal: &ui::Palette, hover: &str) -> bool {
    ui.add(
        egui::Button::new(
            egui::RichText::new(tr("copiar"))
                .size(11.0)
                .color(pal.text_weak),
        )
        .frame_when_inactive(false)
        .corner_radius(6.0)
        .min_size(egui::vec2(44.0, 18.0)),
    )
    .on_hover_text(hover)
    .clicked()
}

/// Botão pequeno do rodapé (mesmo desenho do "copiar"), com rótulo variável e
/// destaque opcional (usado pelo "ouvir" enquanto está tocando).
fn small_button(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    label: &str,
    hover: &str,
    highlight: bool,
) -> bool {
    ui.add(
        egui::Button::new(egui::RichText::new(label).size(11.0).color(if highlight {
            pal.accent
        } else {
            pal.text_weak
        }))
        .frame_when_inactive(false)
        .corner_radius(6.0)
        .min_size(egui::vec2(44.0, 18.0)),
    )
    .on_hover_text(hover)
    .clicked()
}

/// Etapas do "ouvir resumo": o servidor resume a resposta num roteiro falável
/// (`POST /api/voice/summary`), sintetiza (`POST /api/voice/tts`) e o app toca.
enum SpeakStage {
    Summarizing(Task<Value>),
    Synthesizing(Task<(u32, Vec<u8>)>),
    Playing(crate::voice::audio::Player),
}

/// Leitura em andamento de uma resposta (uma por vez na página).
struct SpeakJob {
    turn_id: String,
    /// Roteiro devolvido pelo resumo (mostrado no hover do botão).
    summary: Option<String>,
    stage: SpeakStage,
}

/// Estado do "ouvir" visto por um card de resposta.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SpeakState {
    Idle,
    Summarizing,
    Synthesizing,
    Playing,
}

/// O que os cards precisam saber sobre o "ouvir" e o que pedem de volta.
struct SpeakUi {
    /// Módulo de voz pronto (chave da OpenAI configurada): mostra o botão.
    enabled: bool,
    /// Leitura em andamento: (id do turno, etapa, roteiro).
    current: Option<(String, SpeakState, Option<String>)>,
    /// Pedido feito neste quadro: `Some((id, Some(texto)))` = começar;
    /// `Some((id, None))` = parar.
    request: Option<(String, Option<String>)>,
    /// Roteiros já gerados (cache): o botão mostra "ouvir de novo" e o texto no hover.
    cached: HashMap<String, String>,
}

/// Ícone "i" desenhado (círculo + letra), sem depender de glifos especiais.
fn info_icon(ui: &mut egui::Ui, pal: &ui::Palette) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
    let color = if resp.hovered() {
        pal.text
    } else {
        pal.text_weak
    };
    let c = rect.center();
    ui.painter()
        .circle_stroke(c, 6.5, egui::Stroke::new(1.0, color));
    ui.painter().text(
        c,
        egui::Align2::CENTER_CENTER,
        "i",
        egui::FontId::proportional(10.0),
        color,
    );
    resp
}

/// Corpo do popover de metadados do turno.
fn turn_info_body(ui: &mut egui::Ui, pal: &ui::Palette, turn: &Value) {
    ui.set_min_width(220.0);
    ui.spacing_mut().item_spacing.y = 2.0;
    let res = turn.get("result").cloned().unwrap_or(Value::Null);
    let row = |ui: &mut egui::Ui, k: &str, v: String| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(k).small().color(pal.text_weak));
            ui.add(
                egui::Label::new(egui::RichText::new(v).small().color(pal.text))
                    .truncate()
                    .selectable(true),
            );
        });
    };
    if let Some(m) = res.get("model").and_then(|c| c.as_str()) {
        row(ui, tr("modelo"), m.to_string());
    }
    if let Some(d) = res.get("duration_ms").and_then(|c| c.as_u64()) {
        row(ui, tr("duração"), ui::fmt_ms(d));
    }
    if let Some(c) = res.get("cost_usd").and_then(|c| c.as_f64()) {
        row(ui, tr("custo"), ui::fmt_usd(c));
    }
    if let Some(dt) = turn_dt(turn, "started_at") {
        row(ui, tr("início"), fmt_exact(&dt));
    }
    if let Some(dt) = turn_dt(turn, "finished_at") {
        row(ui, tr("fim"), fmt_exact(&dt));
    }
    if let Some(sid) = res.get("session_id").and_then(|c| c.as_str()) {
        row(ui, tr("sessão"), sid.to_string());
    }
    let id = ui::s(turn, "id");
    if !id.is_empty() {
        row(ui, tr("turno"), id);
    }
}

impl ThreadsPage {
    /// Mensagens aceitas no meio do turno `parent` (turnos mesclados), com horário.
    fn merged_user_turns(&self, parent: &str) -> Vec<(chrono::DateTime<chrono::Utc>, String)> {
        self.turns
            .data
            .as_ref()
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter(|t| ui::s(t, "merged_into") == parent)
                    .filter_map(|t| {
                        let at = turn_dt(t, "started_at")?;
                        Some((at, ui::s(t, "prompt_display")))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn turn_duration_secs(&self, turn_id: &str) -> Option<u64> {
        self.turns
            .data
            .as_ref()
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.iter().find(|t| ui::s(t, "id") == turn_id))
            .and_then(|t| t.get("duration_ms").and_then(|d| d.as_u64()))
            .map(|ms| ms / 1000)
    }

    /// Subagentes ainda rodando em segundo plano no último turno da thread.
    fn background_tasks(&self, thread: &ThreadId) -> usize {
        self.done_by_thread
            .get(thread)
            .and_then(|tid| self.done_live.get(tid))
            .map(|l| l.open_tasks())
            .unwrap_or(0)
    }

    fn reload_list(&mut self, cx: &Cx) {
        self.list
            .start(cx.svc.call(|c| async move { c.threads().await }));
    }

    fn reload_turns(&mut self, cx: &Cx, id: ThreadId) {
        self.turns_for = Some(id);
        let sid = id.to_string();
        self.turns.start(cx.svc.call(move |c| async move {
            c.get_q(&format!("threads/{sid}/turns"), &[("limit", "200".into())])
                .await
        }));
    }

    /// Seleciona uma thread (usado pela captura de diagnóstico).
    pub fn select(&mut self, id: ThreadId) {
        self.selected = Some(id);
        self.list.loaded_at = None;
        self.turns.loaded_at = None;
    }

    pub fn on_event(&mut self, ev: &Event) {
        match ev {
            Event::ThreadCreated { thread } | Event::ThreadUpdated { thread } => {
                if let Some(t) = self.threads.iter_mut().find(|t| t.id == thread.id) {
                    *t = thread.clone();
                } else {
                    self.threads.insert(0, thread.clone());
                }
            }
            Event::TriggersChanged | Event::TriggerFired { .. } => {
                self.triggers.loaded_at = None;
            }
            Event::ThreadDeleted { thread_id } => {
                self.threads.retain(|t| t.id != *thread_id);
                if self.selected == Some(*thread_id) {
                    self.selected = None;
                }
            }
            Event::TurnStarted { thread_id, turn_id } => {
                let l = self.live.entry(*thread_id).or_default();
                l.turn_id = Some(turn_id.to_string());
                if l.started.is_none() {
                    l.started = Some(std::time::Instant::now());
                }
                // O turno já está gravado: recarrega a lista para o balão do
                // usuário aparecer antes da resposta (e não só no fim do turno).
                if self.selected == Some(*thread_id) {
                    self.turns.loaded_at = None;
                }
            }
            Event::MessageAccepted {
                thread_id,
                mid_turn,
                ..
            } => {
                let steer_text = if *mid_turn {
                    // texto do eco mais recente desta thread (mensagem do app);
                    // de outros canais não há texto: entra só a marca
                    self.echoes
                        .get(thread_id)
                        .and_then(|v| v.last())
                        .map(|e| e.text.clone())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let l = self.live.entry(*thread_id).or_default();
                if l.started.is_none() {
                    l.started = Some(std::time::Instant::now());
                }
                if *mid_turn {
                    l.flush_text();
                    l.items.push(LiveItem::User {
                        text: steer_text,
                        at: std::time::Instant::now(),
                    });
                }
                if self.selected == Some(*thread_id) {
                    self.turns.loaded_at = None;
                }
            }
            Event::TurnDelta {
                thread_id, delta, ..
            } => {
                // Resultado tardio (subagente em segundo plano) depois de o turno
                // responder: anexa à atividade já recolhida, sem reabrir "trabalhando".
                if !self.live.contains_key(thread_id) {
                    if let EngineDelta::ToolResult { .. } = delta {
                        if let Some(done) = self
                            .done_by_thread
                            .get(thread_id)
                            .and_then(|tid| self.done_live.get_mut(tid))
                        {
                            if done.open_tasks() > 0 || done.tools_running() > 0 {
                                done.push(delta);
                                return;
                            }
                        }
                    }
                }
                let l = self.live.entry(*thread_id).or_default();
                l.push(delta);
            }
            Event::TurnFinished { thread_id, .. } | Event::TurnFailed { thread_id, .. } => {
                if let Some(mut l) = self.live.remove(thread_id) {
                    l.flush_text();
                    l.finished_secs = Some(l.started.map(|s| s.elapsed().as_secs()).unwrap_or(0));
                    if let Some(tid) = l.turn_id.clone() {
                        if !l.items.is_empty() {
                            self.done_by_thread.insert(*thread_id, tid.clone());
                            self.done_live.insert(tid, l);
                        }
                    }
                }
                if self.selected == Some(*thread_id) {
                    self.turns.loaded_at = None;
                }
                self.list.loaded_at = None;
            }
            Event::StaleSession { thread_id } => {
                self.notes.entry(*thread_id).or_default().push(
                    tr("sessão expirada: a thread recomeçou do zero e reenviou as pendências")
                        .into(),
                );
            }
            _ => {}
        }
    }

    /// Avança o "ouvir resumo": resumo pronto → pede o áudio; áudio pronto →
    /// toca no dispositivo de saída do módulo de voz; acabou → libera o botão.
    fn poll_speak(&mut self, cx: &mut Cx) {
        let Some(job) = &mut self.speak else {
            return;
        };
        let mut finish: Option<Option<String>> = None; // Some(err) encerra
        match &mut job.stage {
            SpeakStage::Summarizing(t) => {
                if let Some(r) = t.take() {
                    match r {
                        Ok(v) => {
                            let summary = ui::s(&v, "summary").trim().to_string();
                            if summary.is_empty() {
                                finish = Some(Some(tr("o servidor não devolveu um resumo").into()));
                            } else {
                                job.summary = Some(summary.clone());
                                job.stage = SpeakStage::Synthesizing(
                                    cx.svc
                                        .call(move |c| async move { c.voice_tts(&summary).await }),
                                );
                            }
                        }
                        Err(e) => finish = Some(Some(e)),
                    }
                }
            }
            SpeakStage::Synthesizing(t) => {
                if let Some(r) = t.take() {
                    match r {
                        Ok((rate, pcm)) => {
                            let dev = cx.snap.settings.realtime.output_device.clone();
                            let p = crate::voice::audio::Player::start(&pcm, rate, dev.as_deref());
                            match &p.error {
                                Some(e) => finish = Some(Some(e.clone())),
                                None => {
                                    // guarda para ouvir de novo sem gastar tokens
                                    let audio = Arc::new(crate::speak_cache::SpeakAudio {
                                        summary: job.summary.clone().unwrap_or_default(),
                                        rate,
                                        pcm,
                                    });
                                    self.speak_cache.put(&job.turn_id, audio);
                                    job.stage = SpeakStage::Playing(p);
                                }
                            }
                        }
                        Err(e) => finish = Some(Some(e)),
                    }
                }
            }
            SpeakStage::Playing(p) => {
                if !p.playing() {
                    finish = Some(None);
                }
            }
        }
        if let Some(err) = finish {
            self.speak = None;
            if let Some(e) = err {
                cx.toasts.error(trf("Ouvir resumo: {}", &[&e]));
            }
        }
    }

    /// Mantém `input`/`pending_files` presos à thread aberta: ao trocar de
    /// thread, o que estava sendo escrito vai para `drafts` e o rascunho da
    /// nova thread (se houver) volta para a caixa.
    fn sync_draft(&mut self) {
        if self.selected == self.draft_for {
            return;
        }
        if let Some(prev) = self.draft_for {
            let d = Draft {
                text: std::mem::take(&mut self.input),
                files: std::mem::take(&mut self.pending_files),
            };
            if d.text.is_empty() && d.files.is_empty() {
                self.drafts.remove(&prev);
            } else {
                self.drafts.insert(prev, d);
            }
        } else {
            self.input.clear();
            self.pending_files.clear();
        }
        if let Some(cur) = self.selected {
            if let Some(d) = self.drafts.remove(&cur) {
                self.input = d.text;
                self.pending_files = d.files;
            }
        }
        self.draft_for = self.selected;
    }

    fn poll(&mut self, cx: &mut Cx) {
        self.sync_draft();
        self.poll_speak(cx);
        // eventos de turnos antigos → linha do tempo reconstruída
        let mut done_ids = Vec::new();
        for (tid, t) in &self.loading_events {
            if let Some(r) = t.take() {
                done_ids.push((tid.clone(), r));
            }
        }
        for (tid, r) in done_ids {
            self.loading_events.remove(&tid);
            match r {
                Ok(v) => {
                    let merged = self.merged_user_turns(&tid);
                    let live = live_from_events(&v, &merged, self.turn_duration_secs(&tid));
                    self.done_live.insert(tid, live);
                }
                Err(e) => cx.toasts.error(trf("Atividade do turno: {}", &[&e])),
            }
        }
        if self.triggers.stale(std::time::Duration::from_secs(120)) {
            self.triggers
                .start(cx.svc.call(|c| async move { c.triggers().await }));
        }
        if self.triggers.poll() {
            let mut map: HashMap<String, Vec<String>> = HashMap::new();
            if let Some(list) = self
                .triggers
                .data
                .as_ref()
                .and_then(|v| v.get("triggers"))
                .and_then(|v| v.as_array())
            {
                for t in list {
                    let tid = ui::s(t, "thread_id");
                    if tid.is_empty() {
                        continue;
                    }
                    map.entry(tid).or_default().push(format!(
                        "{} ({})",
                        ui::s(t, "name"),
                        ui::s(t, "source_label")
                    ));
                }
            }
            self.trigger_map = map;
        }
        if let Some((tid, t)) = &self.dictation_task {
            if let Some(r) = t.take() {
                let tid = *tid;
                self.dictation_task = None;
                match r {
                    Ok(v) => {
                        let text = v
                            .get("text")
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        if text.is_empty() {
                            cx.toasts.info(tr("Não entendi nada no áudio."));
                        } else if self.selected == Some(tid) {
                            // "Falar" é falar E enviar: a transcrição vai direto pra
                            // thread, junto com o que já estava no campo. Se um envio
                            // ainda está em voo, `send` não faz nada e o texto fica no campo.
                            self.input = merge_dictation(&self.input, &text);
                            self.focus_input = true;
                            self.send(cx);
                        } else {
                            // O usuário abriu outra thread enquanto transcrevia: a fala
                            // vai para a thread ORIGINAL (com o rascunho dela), sem
                            // tocar na caixa da thread aberta.
                            let d = self.drafts.remove(&tid).unwrap_or_default();
                            let merged = merge_dictation(&d.text, &text);
                            if !self.send_to(cx, tid, merged.clone(), d.files.clone()) {
                                self.drafts.insert(
                                    tid,
                                    Draft {
                                        text: merged,
                                        files: d.files,
                                    },
                                );
                                cx.toasts.info(tr(
                                    "A transcrição ficou no rascunho da thread onde você falou.",
                                ));
                            }
                        }
                    }
                    Err(e) => cx.toasts.error(trf("Transcrição falhou: {}", &[&e])),
                }
            }
        }
        if self.list.stale(std::time::Duration::from_secs(30)) {
            self.reload_list(cx);
        }
        if self.list.poll() {
            if let Some(v) = &self.list.data {
                if let Ok(list) = serde_json::from_value::<Vec<ThreadSummary>>(v.clone()) {
                    self.threads = list;
                }
            }
        }
        if let Some(id) = self.selected {
            if self.turns_for != Some(id) || (self.turns.loaded_at.is_none() && !self.turns.busy())
            {
                self.reload_turns(cx, id);
            }
        }
        if self.turns.poll() {
            if let (Some(id), Some(loaded_at)) = (self.turns_for, self.turns.loaded_at) {
                if let Some(echoes) = self.echoes.get_mut(&id) {
                    let turns = self
                        .turns
                        .data
                        .as_ref()
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    reconcile_echoes(echoes, &turns, loaded_at);
                }
            }
        }
        // Envios concluídos: cada um reconcilia na SUA thread de origem.
        let mut done: Vec<(ThreadId, u64, Result<Value, String>)> = Vec::new();
        self.send_tasks.retain(|(tid, eid, t)| match t.take() {
            None => true,
            Some(r) => {
                done.push((*tid, *eid, r));
                false
            }
        });
        for (tid, eid, r) in done {
            let echo = self
                .echoes
                .get_mut(&tid)
                .and_then(|v| v.iter_mut().find(|e| e.local_id == eid));
            match r {
                Ok(v) => {
                    if let Some(e) = echo {
                        e.turn_id = v
                            .get("turn_id")
                            .and_then(|x| x.as_str())
                            .map(str::to_string);
                        e.acked_at = Some(std::time::Instant::now());
                    }
                    // A lista é recarregada para trocar o eco pelo turno real.
                    if self.selected == Some(tid) {
                        self.turns.loaded_at = None;
                    }
                    if ui::b(&v, "mid_turn") {
                        cx.toasts.info(tr("Entregue no meio do turno."));
                    }
                }
                Err(e) => match echo {
                    Some(echo) => echo.error = Some(e),
                    None => cx.toasts.error(e),
                },
            }
        }
        if let Some((what, t)) = &self.action_task {
            if let Some(r) = t.take() {
                let what = *what;
                self.action_task = None;
                match r {
                    Ok(_) => {
                        cx.toasts.success(trf("{}: ok", &[&tr(what)]));
                        self.list.loaded_at = None;
                        self.turns.loaded_at = None;
                    }
                    Err(e) => cx.toasts.error(format!("{}: {e}", tr(what))),
                }
            }
        }
        if let Some(t) = &self.create_task {
            if let Some(r) = t.take() {
                self.create_task = None;
                match r {
                    Ok(v) => {
                        if let Ok(t) = serde_json::from_value::<ThreadSummary>(v) {
                            self.selected = Some(t.id);
                            if !self.threads.iter().any(|x| x.id == t.id) {
                                self.threads.insert(0, t);
                            }
                            self.focus_input = true;
                        }
                    }
                    Err(e) => cx.toasts.error(e),
                }
            }
        }
        // anexos remotos: miniaturas que chegaram e aberturas concluídas
        let done: Vec<String> = self
            .remote
            .loading
            .iter()
            .filter(|(_, t)| !t.pending())
            .map(|(k, _)| k.clone())
            .collect();
        for k in done {
            let Some(t) = self.remote.loading.remove(&k) else {
                continue;
            };
            match t.take() {
                Some(Ok((_mime, bytes))) => {
                    self.remote.cache.insert(k, Arc::from(bytes));
                }
                Some(Err(e)) => {
                    self.remote.failed.insert(k, e);
                }
                None => {}
            }
        }
        let mut open_errs = Vec::new();
        self.remote.opening.retain(|t| match t.take() {
            None => true,
            Some(Ok(())) => false,
            Some(Err(e)) => {
                open_errs.push(e);
                false
            }
        });
        for e in open_errs {
            cx.toasts
                .error(trf("não consegui abrir o arquivo: {}", &[&e]));
        }
    }

    /// Envia o que está na caixa para a thread aberta.
    fn send(&mut self, cx: &mut Cx) {
        let Some(id) = self.selected else { return };
        let text = self.input.trim().to_string();
        if (text.is_empty() && self.pending_files.is_empty()) || self.sending(id) {
            return;
        }
        let files = std::mem::take(&mut self.pending_files);
        let draft = std::mem::take(&mut self.input);
        if !self.send_to(cx, id, text, files.clone()) {
            // recusado antes de sair (ex.: anexo grande demais): nada se perde
            self.input = draft;
            self.pending_files = files;
        }
        self.focus_input = true;
    }

    /// Há um envio em voo para esta thread.
    fn sending(&self, id: ThreadId) -> bool {
        self.send_tasks.iter().any(|(t, _, _)| *t == id)
    }

    /// Envia `text` + `files` para a thread `id` (não precisa ser a aberta):
    /// cria o eco otimista nela e dispara o POST. Devolve `false` se recusou
    /// (envio em voo para a mesma thread ou anexo acima do limite remoto).
    fn send_to(
        &mut self,
        cx: &mut Cx,
        id: ThreadId,
        text: String,
        files: Vec<std::path::PathBuf>,
    ) -> bool {
        let text = text.trim().to_string();
        if (text.is_empty() && files.is_empty()) || self.sending(id) {
            return false;
        }
        let local = cx.snap.server_is_local;
        if !local {
            // Remoto: os bytes sobem pela API; recusa antes de mexer no estado.
            let too_big = files.iter().find(|p| {
                std::fs::metadata(p)
                    .map(|m| m.len() > UPLOAD_MAX_BYTES)
                    .unwrap_or(false)
            });
            if let Some(p) = too_big {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("arquivo");
                cx.toasts.error(trf(
                    "{} passa de 64 MiB; esse é o limite de envio para o servidor.",
                    &[&name],
                ));
                return false;
            }
        }
        let sid = id.to_string();
        // Eco otimista: a mensagem entra no chat agora; a lista real a substitui
        // quando o servidor confirmar.
        self.next_echo += 1;
        let eid = self.next_echo;
        self.echoes.entry(id).or_default().push(Echo {
            local_id: eid,
            text: text.clone(),
            files: files.clone(),
            sent_at: chrono::Utc::now(),
            turn_id: None,
            acked_at: None,
            error: None,
        });
        if self.selected == Some(id) {
            self.scroll_bottom = true;
        }
        let task = if local {
            // Loopback: o servidor lê os arquivos do mesmo disco.
            let attachments: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
            cx.svc
                .call(move |c| async move { c.send_message(&sid, &text, attachments).await })
        } else {
            let no_path = tr("o servidor não devolveu o caminho do anexo").to_string();
            cx.svc.call(move |c| async move {
                let mut paths = Vec::with_capacity(files.len());
                for p in files {
                    let name = p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("arquivo")
                        .to_string();
                    let bytes = tokio::fs::read(&p)
                        .await
                        .map_err(|e| format!("{}: {e}", p.display()))?;
                    let v = c.upload(&name, bytes).await?;
                    let path = v
                        .get("path")
                        .and_then(|x| x.as_str())
                        .filter(|s| !s.is_empty())
                        .ok_or_else(|| no_path.clone())?;
                    paths.push(path.to_string());
                }
                c.send_message(&sid, &text, paths).await
            })
        };
        self.send_tasks.push((id, eid, task));
        true
    }

    /// "Reenviar" num eco com erro: devolve texto e anexos à caixa de entrada.
    fn echo_action(&mut self, thread: ThreadId, what: EchoAction) {
        let Some(list) = self.echoes.get_mut(&thread) else {
            return;
        };
        let (id, resend) = match what {
            EchoAction::Resend(id) => (id, true),
            EchoAction::Discard(id) => (id, false),
        };
        let Some(pos) = list.iter().position(|e| e.local_id == id) else {
            return;
        };
        let e = list.remove(pos);
        if resend {
            if !self.input.trim().is_empty() {
                self.input.push('\n');
            }
            self.input.push_str(&e.text);
            for f in e.files {
                if !self.pending_files.contains(&f) {
                    self.pending_files.push(f);
                }
            }
            self.focus_input = true;
        }
    }

    fn action(&mut self, cx: &Cx, id: ThreadId, what: &'static str) {
        let sid = id.to_string();
        let t: Task<Value> = match what {
            "Reiniciar" => cx
                .svc
                .call(move |c| async move { c.restart_thread(&sid).await }),
            "Reset" => cx
                .svc
                .call(move |c| async move { c.reset_thread(&sid).await }),
            "Interromper" => cx
                .svc
                .call(move |c| async move { c.interrupt_thread(&sid).await }),
            _ => return,
        };
        self.action_task = Some((what, t));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx) {
        self.poll(cx);
        let avail_h = ui.available_height();
        let pal = ui::palette(ui.visuals().dark_mode);
        ui.horizontal_top(|ui| {
            ui.set_min_height(avail_h);
            // ----- lista -----
            ui.vertical(|ui| {
                ui.set_width(385.0);
                ui.set_min_height(avail_h);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(tr("Threads")).font(ui::semibold(16.0)));
                    if ui.add_enabled(self.create_task.is_none(), egui::Button::new(tr("+ Nova"))).clicked() {
                        self.create_task = Some(cx.svc.call(|c| async move { c.create_thread(None).await }));
                    }
                    if ui::icon_button(ui, crate::icons::REFRESH, tr("Atualizar")).clicked() {
                        self.list.loaded_at = None;
                    }
                });
                ui.checkbox(&mut self.show_archived, egui::RichText::new(tr("arquivadas")).small());
                if let Some(e) = &self.list.error {
                    ui::error_text(ui, e);
                }
                egui::ScrollArea::vertical().id_salt("threads-list").auto_shrink([false, false]).max_height(avail_h - 70.0).show(ui, |ui| {
                    let mut sorted: Vec<&ThreadSummary> = self.threads.iter().filter(|t| self.show_archived || t.status != ThreadStatus::Archived).collect();
                    sorted.sort_by_key(|t| std::cmp::Reverse(t.last_activity_at));
                    if sorted.is_empty() {
                        ui.label(egui::RichText::new(tr("Nenhuma thread. Crie uma com “+ Nova” ou mande mensagem pelo Telegram/voz.")).weak());
                    }
                    let mut click = None;
                    let n = sorted.len();
                    let divider = egui::Stroke::new(1.0, pal.border.linear_multiply(0.6));
                    ui.spacing_mut().item_spacing.y = 3.0;
                    for (i, t) in sorted.into_iter().enumerate() {
                        let selected = self.selected == Some(t.id);
                        let fill = if selected { ui.visuals().selection.bg_fill.linear_multiply(0.35) } else { egui::Color32::TRANSPARENT };
                        let r = egui::Frame::new().fill(fill).corner_radius(6.0).inner_margin(egui::Margin::symmetric(8, 3)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);
                            ui.horizontal(|ui| {
                                if self.live.contains_key(&t.id) || t.pending > 0 || t.status == ThreadStatus::Running || self.background_tasks(&t.id) > 0 {
                                    ui::pulse_dot(ui);
                                } else {
                                    ui::dot(ui, status_tone(t));
                                }
                                origin_icon(ui, pal, t);
                                if let Some(names) = self.trigger_map.get(&t.id.to_string()) {
                                    trigger_icon(ui, pal, names);
                                }
                                ui.add(egui::Label::new(egui::RichText::new(title_of(t)).font(ui::medium(13.0))).truncate());
                            });
                            // resumo da conversa: até 3 linhas, itálico, truncado com "…"
                            if let Some(sm) = t.summary.as_deref().filter(|x| !x.trim().is_empty()) {
                                ui.horizontal(|ui| {
                                    ui.add_space(14.0);
                                    let w = ui.available_width() - 2.0;
                                    wrapped_label(ui, sm, egui::FontId::proportional(10.5), pal.text_weak, true, 3, w);
                                });
                            }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                ui.label(egui::RichText::new(time_ago(&t.last_activity_at)).size(10.5).color(pal.text_weak));
                            });
                        });
                        // Detalhes saem da linha e vão para o tooltip.
                        let mut tip = format!("{}\n{}", origin_label(t), fmt_exact(&t.last_activity_at));
                        if let Some(sm) = t.summary.as_deref().filter(|x| !x.trim().is_empty()) {
                            tip.push_str("\n\n");
                            tip.push_str(sm);
                        }
                        if t.pending > 0 {
                            tip.push('\n');
                            tip.push_str(&trf("{} pendente(s)", &[&t.pending]));
                        }
                        tip.push('\n');
                        tip.push_str(if t.alive { tr("processo vivo") } else { tr("processo dormente") });
                        if t.session_expired {
                            tip.push('\n');
                            tip.push_str(tr("sessão expirada"));
                        }
                        if r
                            .response
                            .interact(egui::Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text(tip)
                            .clicked()
                        {
                            click = Some(t.id);
                        }
                        if i + 1 < n {
                            let rect = r.response.rect;
                            ui.painter().hline(
                                egui::Rangef::new(rect.left() + 8.0, rect.right() - 8.0),
                                rect.bottom() + 1.5,
                                divider,
                            );
                        }
                    }
                    if let Some(id) = click {
                        self.selected = Some(id);
                        self.rename_buf = None;
                        self.focus_input = true;
                    }
                });
            });
            ui.separator();
            // ----- conversa -----
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(avail_h);
                match self.selected.and_then(|id| self.threads.iter().find(|t| t.id == id).cloned()) {
                    None => {
                        ui.add_space(40.0);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new(tr("Selecione uma thread ou crie uma nova.")).weak().size(16.0));
                        });
                    }
                    Some(t) => self.chat_ui(ui, cx, &t),
                }
            });
        });

        if let Some((id, purge)) = self.confirm_delete {
            let c = ui::Confirm {
                title: tr("Apagar thread").into(),
                body: if purge {
                    tr("Apagar a thread e a transcrição salva do Claude? Isso não tem volta.")
                        .into()
                } else {
                    tr("Apagar a thread do will-ia? A transcrição do Claude Code fica no disco.")
                        .into()
                },
                confirm_label: tr("Apagar").into(),
                danger: true,
                tag: "del-thread".into(),
            };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_delete = None;
                if ok {
                    let sid = id.to_string();
                    self.action_task = Some((
                        "Apagar",
                        cx.svc
                            .call(move |c| async move { c.delete_thread(&sid, purge).await }),
                    ));
                    self.selected = None;
                }
            }
        }
        if let Some(id) = self.confirm_reset {
            let c = ui::Confirm { title: tr("Reset da thread").into(), body: tr("Mata o processo e esquece a sessão: a próxima mensagem começa uma conversa nova com o Claude.").into(), confirm_label: tr("Reset").into(), danger: true, tag: "reset-thread".into() };
            if let Some(ok) = ui::confirm_modal(ui.ctx(), &c) {
                self.confirm_reset = None;
                if ok {
                    self.action(cx, id, "Reset");
                }
            }
        }
    }

    fn chat_ui(&mut self, ui: &mut egui::Ui, cx: &mut Cx, t: &ThreadSummary) {
        let pal = ui::palette(ui.visuals().dark_mode);
        // cabeçalho: título + "i" (detalhes) à esquerda; ações à direita
        ui.horizontal(|ui| {
            match &mut self.rename_buf {
                Some(buf) => {
                    let r = ui.add(
                        egui::TextEdit::singleline(buf)
                            .margin(crate::ui::input_margin())
                            .desired_width(260.0),
                    );
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button(tr("OK")).clicked() || enter {
                        let title = buf.clone();
                        let sid = t.id.to_string();
                        self.action_task = Some((
                            "Renomear",
                            cx.svc
                                .call(move |c| async move { c.rename_thread(&sid, &title).await }),
                        ));
                        self.rename_buf = None;
                    } else if ui.button(tr("Cancelar")).clicked() {
                        self.rename_buf = None;
                    }
                }
                None => {
                    // Título truncado: um nome longo não pode empurrar o "i" nem
                    // invadir os botões alinhados à direita.
                    let w = (ui.available_width() - 490.0).max(160.0);
                    let bg = self.background_tasks(&t.id);
                    if self.live.contains_key(&t.id) || t.pending > 0 || bg > 0 {
                        ui::pulse_dot(ui);
                    }
                    if bg > 0 {
                        ui::badge(
                            ui,
                            &trf("{} subagente(s) em segundo plano", &[&bg]),
                            Tone::Warning,
                        );
                    }
                    origin_icon(ui, pal, t);
                    if let Some(names) = self.trigger_map.get(&t.id.to_string()) {
                        trigger_icon(ui, pal, names);
                    }
                    ui.scope(|ui| {
                        ui.set_max_width(w);
                        ui.add(
                            egui::Label::new(egui::RichText::new(title_of(t)).heading()).truncate(),
                        )
                        .on_hover_text(title_of(t));
                    });
                    // "i": origem, status, custo, sessão… (popover no clique, igual
                    // ao "i" das respostas)
                    let info = info_icon(ui, pal).on_hover_cursor(egui::CursorIcon::PointingHand);
                    let opened = egui::containers::Popup::menu(&info)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show(|ui| thread_info_body(ui, pal, t));
                    if opened.is_none() {
                        info.on_hover_ui(|ui| thread_info_body(ui, pal, t));
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(tr("apagar")).clicked() {
                    self.confirm_delete = Some((t.id, false));
                }
                if ui
                    .button(tr("reset"))
                    .on_hover_text(tr("mata o processo e esquece a sessão"))
                    .clicked()
                {
                    self.confirm_reset = Some(t.id);
                }
                if ui
                    .button(tr("reiniciar"))
                    .on_hover_text(tr("mata o processo; volta com --resume"))
                    .clicked()
                {
                    self.action(cx, t.id, "Reiniciar");
                }
                if self.rename_buf.is_none() && ui.button(tr("renomear")).clicked() {
                    self.rename_buf = Some(title_of(t));
                }
            });
        });
        // resumo da conversa: linha inteira à esquerda, até 2 linhas (inteiro no hover)
        if let Some(sm) = t.summary.as_deref().filter(|x| !x.trim().is_empty()) {
            let w = ui.available_width() - 4.0;
            wrapped_label(
                ui,
                sm,
                egui::FontId::proportional(12.0),
                pal.text_weak,
                true,
                2,
                w,
            )
            .on_hover_text(sm);
        }
        if t.session_expired {
            ui::badge(
                ui,
                tr("sessão expirada no disco; vai recomeçar"),
                Tone::Warning,
            );
        }
        ui.separator();

        // histórico + ao vivo: usa exatamente o que sobra acima da caixa de
        // entrada (altura medida no quadro anterior), sem estourar a janela.
        let bottom_h = if self.bottom_h > 0.0 {
            self.bottom_h
        } else {
            84.0
        };
        let hist_h = (ui.available_height() - bottom_h - 8.0).max(120.0);
        let loading = self.turns.loading() && self.turns.data.is_none();
        let turns_err = self.turns.error.clone();
        let turns = self
            .turns
            .data
            .as_ref()
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let notes = self.notes.get(&t.id).cloned().unwrap_or_default();
        let echoes: Vec<Echo> = self.echoes.get(&t.id).cloned().unwrap_or_default();
        let mut echo_actions: Vec<EchoAction> = Vec::new();
        let scroll_bottom = std::mem::take(&mut self.scroll_bottom);
        let live = self.live.get(&t.id).cloned();
        let has_live = live.is_some();
        let dark = ui.visuals().dark_mode;
        let pal = ui::palette(dark);
        // "Ouvir resumo": só com o módulo de voz pronto (mesma regra do "Falar").
        let mut speak_ui = SpeakUi {
            enabled: cx.snap.settings.realtime.summary.enabled
                && matches!(
                    cx.snap.modules.get(&will_core::Module::Realtime),
                    Some(will_core::ModuleStatus::Running)
                ),
            current: self.speak.as_ref().map(|j| {
                let st = match j.stage {
                    SpeakStage::Summarizing(_) => SpeakState::Summarizing,
                    SpeakStage::Synthesizing(_) => SpeakState::Synthesizing,
                    SpeakStage::Playing(_) => SpeakState::Playing,
                };
                (j.turn_id.clone(), st, j.summary.clone())
            }),
            request: None,
            cached: turns
                .iter()
                .filter_map(|t| {
                    let id = ui::s(t, "id");
                    self.speak_cache.summary(&id).map(|s| (id, s))
                })
                .collect(),
        };
        // Anexos são caminhos do servidor: em loopback a UI lê o disco; remoto,
        // miniaturas e "abrir" passam por `GET /api/files`.
        let mut acx = AttachCx {
            toasts: &mut *cx.toasts,
            svc: cx.svc,
            remote: &mut self.remote,
            local: cx.snap.server_is_local,
        };
        egui::ScrollArea::vertical()
            .id_salt(("chat", t.id))
            .auto_shrink([false, false])
            .max_height(hist_h)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add_space(6.0);
                if loading {
                    ui::busy(ui, tr("carregando…"));
                }
                if let Some(e) = &turns_err {
                    ui::error_text(ui, e);
                }
                for g in group_turns(&turns) {
                    let turn = g.main;
                    // Escopo de id por turno: respostas com o mesmo Markdown
                    // (tabelas, blocos de código) não colidem entre si.
                    ui.push_id(ui::s(turn, "id"), |ui| {
                        // Balões do usuário do mesmo turno (a mensagem inicial
                        // e as enviadas no meio dele) ficam agrupados.
                        let prompts: Vec<&Value> = std::iter::once(turn)
                            .chain(g.extra.iter().copied())
                            .filter(|t| !ui::s(t, "prompt").is_empty())
                            .collect();
                        let n = prompts.len();
                        for (i, t) in prompts.iter().enumerate() {
                            let pos = match (n, i) {
                                (1, _) => GroupPos::Single,
                                (_, 0) => GroupPos::First,
                                (_, i) if i + 1 == n => GroupPos::Last,
                                _ => GroupPos::Middle,
                            };
                            let started = turn_dt(t, "started_at");
                            // Autor (conta que escreveu): no primeiro balão do
                            // grupo e sempre que muda em relação ao anterior.
                            let author = ui::s(t, "author");
                            let show_author = !author.is_empty()
                                && author != "local"
                                && (i == 0 || ui::s(prompts[i - 1], "author") != author);
                            ui.push_id(ui::s(t, "id"), |ui| {
                                user_bubble(
                                    ui,
                                    pal,
                                    &mut acx,
                                    t,
                                    started.as_ref(),
                                    pos,
                                    show_author.then_some(author.as_str()),
                                );
                            });
                        }
                        let text = ui::s(turn, "text");
                        let is_err = ui::b(turn, "is_error");
                        // atividade do turno (ferramentas, falas intermediárias), recolhida:
                        // em memória (turno desta sessão) ou reconstruída dos eventos
                        // gravados no servidor ao expandir
                        let turn_key = ui::s(turn, "id");
                        let events_n = turn.get("events").and_then(|e| e.as_u64()).unwrap_or(0);
                        if let Some(done) = self.done_live.get(&turn_key) {
                            activity_strip(ui, pal, &mut self.md_cache, done, &turn_key, false);
                        } else if events_n > 0 && !turn_key.is_empty() {
                            let loading = self.loading_events.contains_key(&turn_key);
                            let placeholder = LiveTurn {
                                turn_id: Some(turn_key.clone()),
                                items: Vec::new(),
                                text: String::new(),
                                started: None,
                                finished_secs: turn
                                    .get("duration_ms")
                                    .and_then(|d| d.as_u64())
                                    .map(|ms| ms / 1000),
                            };
                            let wants = activity_strip(
                                ui,
                                pal,
                                &mut self.md_cache,
                                &placeholder,
                                &turn_key,
                                loading,
                            );
                            if wants && !loading {
                                let id = turn_key.clone();
                                self.loading_events.insert(
                                    turn_key.clone(),
                                    cx.svc
                                        .call(move |c| async move { c.turn_events(&id).await }),
                                );
                            }
                        }
                        if is_err {
                            let msg = if text.is_empty() {
                                ui::s(turn, "error_kind")
                            } else {
                                text.clone()
                            };
                            error_card(ui, pal, &msg);
                        } else if !text.is_empty() {
                            assistant_card(
                                ui,
                                pal,
                                &mut self.md_cache,
                                acx.toasts,
                                &mut speak_ui,
                                &text,
                                turn,
                            );
                        } else if turn.get("finished_at").map(|f| f.is_null()).unwrap_or(true)
                            && !has_live
                        {
                            ui.label(
                                egui::RichText::new(tr("aguardando resposta…"))
                                    .weak()
                                    .small(),
                            );
                        }
                    });
                }
                // Mensagens recém-enviadas, ainda sem turno na lista (eco otimista).
                for e in &echoes {
                    ui.push_id(("echo", e.local_id), |ui| {
                        if let Some(a) = echo_bubble(ui, pal, &mut acx, e) {
                            echo_actions.push(a);
                        }
                    });
                }
                for n in &notes {
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(n).small().weak().italics());
                    });
                }
                if let Some(l) = &live {
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(120));
                    ui.push_id(("live", t.id), |ui| {
                        live_card(ui, pal, &mut self.md_cache, l);
                    });
                }
                ui.add_space(8.0);
                if scroll_bottom {
                    ui.scroll_to_cursor(Some(egui::Align::BOTTOM));
                }
            });
        for a in echo_actions {
            self.echo_action(t.id, a);
        }

        // "Ouvir resumo": começar (resumo no servidor) ou parar (solta o player).
        if let Some((turn_id, text)) = speak_ui.request.take() {
            match text {
                Some(text) => {
                    // Já gerado antes (memória/disco): toca direto, sem resumo nem TTS.
                    if let Some(audio) = self.speak_cache.get(&turn_id) {
                        let dev = cx.snap.settings.realtime.output_device.clone();
                        let p = crate::voice::audio::Player::start(
                            &audio.pcm,
                            audio.rate,
                            dev.as_deref(),
                        );
                        match &p.error {
                            Some(e) => cx.toasts.error(trf("Ouvir resumo: {}", &[e])),
                            None => {
                                self.speak = Some(SpeakJob {
                                    turn_id,
                                    summary: Some(audio.summary.clone()),
                                    stage: SpeakStage::Playing(p),
                                });
                            }
                        }
                    } else {
                        self.speak = Some(SpeakJob {
                            turn_id,
                            summary: None,
                            stage: SpeakStage::Summarizing(
                                cx.svc
                                    .call(move |c| async move { c.voice_summary(&text).await }),
                            ),
                        });
                    }
                }
                None => self.speak = None,
            }
        }

        // arrastar-e-soltar: arquivos soltos na janela viram anexos pendentes
        let (dropped, hovering) = ui.ctx().input(|i| {
            (
                i.raw
                    .dropped_files
                    .iter()
                    .map(|f| f.path().to_path_buf())
                    .collect::<Vec<_>>(),
                !i.raw.hovered_files.is_empty(),
            )
        });
        for p in dropped {
            if p.is_file() && !self.pending_files.contains(&p) {
                self.pending_files.push(p);
            }
        }

        // entrada: anexos pendentes + caixa de texto + Anexar/Enviar
        let mut send_now = false;
        let busy = self.sending(t.id);
        let pal2 = ui::palette(ui.visuals().dark_mode);
        let bottom = ui.scope(|ui| {
            ui.add_space(6.0);
            egui::Frame::new()
                .fill(pal2.card)
                .stroke(egui::Stroke::new(
                    1.0,
                    if hovering { pal2.accent } else { pal2.border },
                ))
                .corner_radius(10.0)
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if !self.pending_files.is_empty() {
                        let mut remove: Option<usize> = None;
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
                            for (i, p) in self.pending_files.iter().enumerate() {
                                let name =
                                    p.file_name().and_then(|n| n.to_str()).unwrap_or("arquivo");
                                egui::Frame::new()
                                    .fill(pal2.accent.linear_multiply(0.18))
                                    .stroke(egui::Stroke::new(
                                        1.0,
                                        pal2.accent.linear_multiply(0.5),
                                    ))
                                    .corner_radius(8.0)
                                    .inner_margin(egui::Margin::symmetric(8, 3))
                                    .show(ui, |ui| {
                                        ui.spacing_mut().item_spacing.x = 4.0;
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(name)
                                                    .size(12.0)
                                                    .color(pal2.text),
                                            )
                                            .truncate(),
                                        )
                                        .on_hover_text(p.display().to_string());
                                        if ui::icon_button_colored(
                                            ui,
                                            crate::icons::CLOSE,
                                            tr("remover anexo"),
                                            pal2.text_weak,
                                        )
                                        .clicked()
                                        {
                                            remove = Some(i);
                                        }
                                    });
                            }
                        });
                        if let Some(i) = remove {
                            self.pending_files.remove(i);
                        }
                        ui.add_space(4.0);
                    }
                    ui.horizontal(|ui| {
                        let btn_w = 92.0 + 76.0;
                        let text_w = (ui.available_width() - btn_w - 12.0).max(120.0);
                        let hint = if hovering {
                            tr("Solte os arquivos para anexar.")
                        } else if !self.pending_files.is_empty() {
                            tr("Comentário opcional sobre os anexos. Ctrl+Enter envia.")
                        } else if t.pending > 0 {
                            tr("Turno em andamento: a mensagem entra no meio. Ctrl+Enter envia.")
                        } else {
                            tr("Mensagem para o Claude. Ctrl+Enter envia.")
                        };
                        let r = ui.add_sized(
                            [text_w, 54.0],
                            egui::TextEdit::multiline(&mut self.input).margin(crate::ui::input_margin())
                                .frame(egui::Frame::NONE)
                                .desired_rows(2)
                                .hint_text(hint),
                        );
                        if self.focus_input {
                            r.request_focus();
                            self.focus_input = false;
                        }
                        if r.has_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command)
                        {
                            send_now = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let can = !busy
                                && (!self.input.trim().is_empty()
                                    || !self.pending_files.is_empty());
                            let b = egui::Button::new(if busy {
                                egui::WidgetText::from(
                                    egui::RichText::new("…").color(egui::Color32::WHITE),
                                )
                            } else {
                                ui::icon_text_colored(
                                    crate::icons::SEND,
                                    tr("Enviar"),
                                    14.0,
                                    egui::Color32::WHITE,
                                )
                            })
                            .fill(if can {
                                pal2.accent
                            } else {
                                pal2.accent.linear_multiply(0.45)
                            })
                            .min_size(egui::vec2(84.0, 34.0));
                            if ui.add_enabled(can, b).clicked() {
                                send_now = true;
                            }
                            // Falar (ditado): segure para gravar; solte para transcrever
                            // (Whisper via servidor) e ENVIAR direto. Só com o módulo de voz pronto.
                            let voice_ready = matches!(
                                cx.snap.modules.get(&will_core::Module::Realtime),
                                Some(will_core::ModuleStatus::Running)
                            );
                            if voice_ready {
                                let recording = self.dictation.is_some();
                                let transcribing = self
                                    .dictation_task
                                    .as_ref()
                                    .is_some_and(|(_, t)| t.pending());
                                let label = if recording {
                                    trf(
                                        "{}s",
                                        &[&format!(
                                            "{:.0}",
                                            self.dictation
                                                .as_ref()
                                                .map(|d| d.elapsed_secs())
                                                .unwrap_or(0.0)
                                        )],
                                    )
                                } else if transcribing {
                                    tr("…").into()
                                } else {
                                    tr("Falar").into()
                                };
                                let color = if recording {
                                    egui::Color32::WHITE
                                } else {
                                    pal2.text
                                };
                                let text: egui::WidgetText = if recording {
                                    ui::icon_text_colored(crate::icons::RECORD, &label, 14.0, color)
                                } else if transcribing {
                                    egui::RichText::new(label).color(color).into()
                                } else {
                                    ui::icon_text_colored(crate::icons::MIC, &label, 14.0, color)
                                };
                                let b = egui::Button::new(text)
                                .fill(if recording {
                                    egui::Color32::from_rgb(200, 60, 60)
                                } else {
                                    pal2.card
                                })
                                .min_size(egui::vec2(68.0, 34.0));
                                let r = ui
                                    .add_enabled(!busy && !transcribing, b)
                                    .on_hover_text(tr(
                                    "Segure para falar; ao soltar, transcreve (Whisper) e envia na hora.",
                                ));
                                // Começa ao pressionar sobre o botão; continua
                                // enquanto o botão do mouse estiver pressionado em
                                // QUALQUER lugar (não só sobre o botão) e só para ao
                                // soltar de verdade. `is_pointer_button_down_on` é
                                // frágil entre quadros (o rótulo muda a cada segundo),
                                // então a parada usa o estado global do ponteiro.
                                let pressed_on = r.is_pointer_button_down_on();
                                let primary_down = ui.input(|i| i.pointer.primary_down());
                                if pressed_on && !recording && !transcribing {
                                    let dev = cx.snap.settings.realtime.input_device.clone();
                                    let rec = crate::voice::audio::Recorder::start(dev.as_deref());
                                    if let Some(e) = &rec.error {
                                        cx.toasts.error(e.clone());
                                    } else {
                                        self.dictation = Some(rec);
                                        self.dictation_for = Some(t.id);
                                    }
                                } else if recording && !primary_down {
                                    if let Some(rec) = self.dictation.take() {
                                        if let Some(e) = rec.runtime_error() {
                                            cx.toasts.error(trf("Microfone: {}", &[&e]));
                                        } else {
                                            match rec.stop() {
                                                Some(wav) => {
                                                    let for_thread =
                                                        self.dictation_for.take().unwrap_or(t.id);
                                                    self.dictation_task = Some((
                                                        for_thread,
                                                        cx.svc.call(move |c| async move {
                                                            c.transcribe(wav, Some("pt")).await
                                                        }),
                                                    ));
                                                }
                                                None => cx.toasts.info(tr("Gravação muito curta.")),
                                            }
                                        }
                                    }
                                }
                                if recording || transcribing {
                                    ui.ctx().request_repaint_after(
                                        std::time::Duration::from_millis(200),
                                    );
                                }
                            }
                            // Anexar: diálogo nativo de arquivos (vários de uma vez)
                            let a = egui::Button::new(ui::icon_text_colored(
                                crate::icons::ATTACH,
                                tr("Anexar"),
                                14.0,
                                pal2.text,
                            ))
                            .min_size(egui::vec2(68.0, 34.0));
                            if ui
                                .add_enabled(!busy, a)
                                .on_hover_text(tr("Anexar arquivos (ou arraste para a janela)"))
                                .clicked()
                            {
                                if let Some(files) = rfd::FileDialog::new()
                                    .set_title(tr("Anexar arquivos"))
                                    .pick_files()
                                {
                                    for p in files {
                                        if !self.pending_files.contains(&p) {
                                            self.pending_files.push(p);
                                        }
                                    }
                                }
                            }
                            // Interromper: só com turno em execução, em vermelho
                            // (à esquerda de Anexar; manda o interrupt ao claude).
                            if t.pending > 0 || has_live {
                                let b = egui::Button::new(
                                    egui::RichText::new(tr("Interromper"))
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(200, 60, 60))
                                .min_size(egui::vec2(96.0, 34.0));
                                if ui
                                    .add(b)
                                    .on_hover_text(tr(
                                        "Para o turno atual (o processo continua vivo).",
                                    ))
                                    .clicked()
                                {
                                    self.action(cx, t.id, "Interromper");
                                }
                            }
                        });
                    });
                });
        });
        self.bottom_h = bottom.response.rect.height();
        if send_now {
            self.send(cx);
        }
    }
}

/// Renderiza Markdown (a IA responde em Markdown).
fn md(ui: &mut egui::Ui, cache: &mut egui_commonmark::CommonMarkCache, text: &str) {
    let text = ui::clean_emoji(text);
    // Sem `max_image_width`: o viewer usa a largura disponível como teto, então
    // blocos de código e imagens nunca alargam o balão além do chat.
    egui_commonmark::CommonMarkViewer::new().show(ui, cache, &text);
}

/// Um turno "principal" e os turnos que ele absorveu: mensagens enviadas
/// enquanto ele rodava (`merged_into`) ou, ao vivo, ainda sem `result`.
struct TurnGroup<'a> {
    main: &'a Value,
    extra: Vec<&'a Value>,
}

/// Agrupa a lista (em ordem cronológica) de turnos da API. Um turno com
/// `merged_into` entra no grupo do pai; um turno ainda aberto logo depois de
/// outro também aberto é steering ao vivo e entra no grupo anterior.
fn group_turns(turns: &[Value]) -> Vec<TurnGroup<'_>> {
    let mut groups: Vec<TurnGroup<'_>> = Vec::new();
    for t in turns {
        let merged_into = ui::s(t, "merged_into");
        if !merged_into.is_empty() {
            if let Some(g) = groups
                .iter_mut()
                .find(|g| ui::s(g.main, "id") == merged_into)
            {
                g.extra.push(t);
                continue;
            }
        }
        let open = t.get("finished_at").map(|f| f.is_null()).unwrap_or(true);
        if open && ui::s(t, "text").is_empty() {
            if let Some(g) = groups.last_mut() {
                let prev_open = g
                    .main
                    .get("finished_at")
                    .map(|f| f.is_null())
                    .unwrap_or(true);
                if prev_open && ui::s(g.main, "text").is_empty() {
                    g.extra.push(t);
                    continue;
                }
            }
        }
        groups.push(TurnGroup {
            main: t,
            extra: Vec::new(),
        });
    }
    groups
}

/// Posição do balão dentro de um grupo de mensagens do usuário.
#[derive(Clone, Copy, PartialEq, Eq)]
enum GroupPos {
    Single,
    First,
    Middle,
    Last,
}

/// O que um balão precisa para mostrar e abrir anexos: toasts, cliente da API,
/// cache dos anexos remotos e se o servidor é esta máquina.
struct AttachCx<'a> {
    toasts: &'a mut ui::Toasts,
    svc: &'a Arc<Service>,
    remote: &'a mut RemoteFiles,
    /// O servidor é esta máquina: os caminhos existem aqui (`file://`, abrir direto).
    local: bool,
}

fn user_bubble(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    acx: &mut AttachCx<'_>,
    turn: &Value,
    started: Option<&chrono::DateTime<chrono::Utc>>,
    pos: GroupPos,
    // Conta que escreveu (`turns.author`), quando vale a pena mostrar.
    author: Option<&str>,
) {
    // `prompt_display` vem sem a nota de anexo que o canal acrescenta para o
    // cérebro; os anexos chegam estruturados em `attachments`.
    let text = turn
        .get("prompt_display")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| ui::s(turn, "prompt"));
    let atts: Vec<will_core::Attachment> = turn
        .get("attachments")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let raw = text.as_str();
    let shown = ui::clean_emoji(&text);
    let dark = ui.visuals().dark_mode;
    let fill = pal.accent.linear_multiply(if dark { 0.28 } else { 0.14 });
    // Cantos: num grupo, o lado direito fica "colado" entre os balões.
    let (ne, se) = match pos {
        GroupPos::Single => (12, 3),
        GroupPos::First => (12, 4),
        GroupPos::Middle => (4, 4),
        GroupPos::Last => (4, 3),
    };
    let last = matches!(pos, GroupPos::Single | GroupPos::Last);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(fill)
            .stroke(egui::Stroke::new(1.0, pal.accent.linear_multiply(0.45)))
            .corner_radius(egui::CornerRadius {
                nw: 12,
                ne,
                sw: 12,
                se,
            })
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width() * 0.72);
                ui.vertical(|ui| {
                    if let Some(a) = author {
                        ui.label(egui::RichText::new(a).size(11.0).color(pal.text_weak));
                    }
                    if !shown.trim().is_empty() {
                        ui.add(
                            egui::Label::new(egui::RichText::new(&shown).color(pal.text))
                                .wrap()
                                .selectable(true),
                        );
                    }
                    for a in &atts {
                        attachment_view(ui, pal, acx, a);
                    }
                    ui.add_space(2.0);
                    // rodapé: uma linha, discreto (copiar + tempo relativo).
                    // Num grupo, só o último balão mostra a hora; os outros
                    // mostram a hora exata no hover do "copiar".
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let hover = match started {
                            Some(dt) if !last => {
                                format!("{} · {}", tr("copiar a mensagem enviada"), fmt_exact(dt))
                            }
                            _ => tr("copiar a mensagem enviada").to_string(),
                        };
                        if copy_button(ui, pal, &hover) {
                            ui.ctx().copy_text(raw.to_string());
                            acx.toasts.success(tr("Mensagem copiada."));
                        }
                        if last {
                            when_label(ui, pal, started);
                        }
                    });
                });
            });
    });
    ui.add_space(if last { 6.0 } else { 2.0 });
}

/// Balão de uma mensagem ainda não confirmada pelo servidor: o mesmo balão do
/// usuário, esmaecido, com "enviando…" no rodapé; em erro, o motivo e os botões
/// "Reenviar"/"Descartar". Devolve a ação clicada.
fn echo_bubble(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    acx: &mut AttachCx<'_>,
    e: &Echo,
) -> Option<EchoAction> {
    let atts: Vec<Value> = e
        .files
        .iter()
        .map(|p| {
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("arquivo")
                .to_string();
            let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
            json!({
                "name": name,
                "mime": guess_mime(p),
                "path": p.display().to_string(),
                "size": size,
            })
        })
        .collect();
    // Remoto: os caminhos são desta máquina, não do servidor — sem miniatura,
    // só o chip; em loopback o balão lê o arquivo direto.
    let turn = if acx.local {
        json!({ "prompt_display": e.text, "attachments": atts })
    } else {
        let names: Vec<String> = atts
            .iter()
            .map(|a| format!("📎 {}", ui::s(a, "name")))
            .collect();
        let mut text = e.text.clone();
        if !names.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&names.join("\n"));
        }
        json!({ "prompt_display": text, "attachments": [] })
    };
    let failed = e.error.is_some();
    ui.scope(|ui| {
        if !failed {
            ui.set_opacity(0.62);
        }
        user_bubble(ui, pal, acx, &turn, None, GroupPos::Single, None);
    });
    let mut action = None;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        ui.add_space(8.0);
        match &e.error {
            None => {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(250));
                ui.label(
                    egui::RichText::new(tr("enviando…"))
                        .size(11.0)
                        .color(pal.text_weak),
                );
                ui.add(egui::Spinner::new().size(10.0));
            }
            Some(err) => {
                if ui.small_button(tr("Descartar")).clicked() {
                    action = Some(EchoAction::Discard(e.local_id));
                }
                if ui.small_button(tr("Reenviar")).clicked() {
                    action = Some(EchoAction::Resend(e.local_id));
                }
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(trf("não enviada: {}", &[err]))
                            .size(11.0)
                            .color(Tone::Danger.color(ui.visuals().dark_mode)),
                    )
                    .truncate(),
                )
                .on_hover_text(err);
            }
        }
    });
    ui.add_space(6.0);
    action
}

/// Tamanho legível de um arquivo.
fn fmt_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if bytes == 0 {
        String::new()
    } else if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{:.1} MB", b / KB / KB)
    }
}

/// Anexo dentro do balão do usuário: miniatura clicável para imagens, chip com
/// nome e tamanho para os demais. O clique abre o arquivo no app padrão.
///
/// Em loopback (`acx.local`) o caminho existe aqui: `file://` e abrir direto.
/// Remoto, os bytes vêm de `GET /api/files` (`docs/26`): a miniatura é baixada
/// uma vez por caminho e guardada em `acx.remote`; abrir baixa para uma pasta
/// temporária do app e chama o app padrão.
fn attachment_view(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    acx: &mut AttachCx<'_>,
    a: &will_core::Attachment,
) {
    let local = acx.local;
    let path = std::path::Path::new(&a.path);
    // Remoto: não dá para conferir o disco do servidor; só se sabe que o
    // arquivo falta quando o download da miniatura falha.
    let remote_err = if local {
        None
    } else {
        acx.remote.failed.get(&a.path).cloned()
    };
    let exists = if local {
        path.is_file()
    } else {
        remote_err.is_none()
    };
    let size = fmt_size(a.size);
    let mut caption = a.name.clone();
    if !size.is_empty() {
        caption.push_str(" · ");
        caption.push_str(&size);
    }
    if !exists {
        caption.push_str(" · ");
        caption.push_str(tr("arquivo não encontrado"));
    }
    ui.add_space(4.0);
    const THUMB: egui::Vec2 = egui::vec2(320.0, 240.0);
    // Fonte da miniatura: `Some(Some(img))` = há imagem para decodificar;
    // `Some(None)` = ainda baixando do servidor; `None` = chip.
    let source: Option<Option<egui::Image<'static>>> = if !a.is_image() || !exists {
        None
    } else if local {
        Some(Some(egui::Image::new(format!("file://{}", a.path))))
    } else if let Some(bytes) = acx.remote.cache.get(&a.path) {
        Some(Some(egui::Image::from_bytes(
            format!("bytes://{}", a.path),
            bytes.clone(),
        )))
    } else if a.size > THUMB_MAX_BYTES {
        None
    } else {
        if !acx.remote.loading.contains_key(&a.path) {
            let p = a.path.clone();
            acx.remote.loading.insert(
                a.path.clone(),
                acx.svc.call(move |c| async move { c.file_bytes(&p).await }),
            );
        }
        Some(None)
    };
    // Miniatura só quando a imagem já decodificou: o tamanho vem do arquivo
    // (não do espaço disponível, que varia dentro do balão) e é limitado a THUMB.
    let thumb = source.and_then(|s| match s {
        None => Some(None),
        Some(img) => match img.load_for_size(ui.ctx(), THUMB) {
            Ok(egui::load::TexturePoll::Ready { .. }) => Some(Some(img)),
            Ok(egui::load::TexturePoll::Pending { .. }) => Some(None),
            Err(_) => None,
        },
    });
    let clicked = if let Some(img) = thumb {
        match img {
            Some(img) => {
                let r = ui
                    .add(
                        img.fit_to_original_size(1.0)
                            .max_size(THUMB)
                            .corner_radius(8.0)
                            .sense(egui::Sense::click()),
                    )
                    .on_hover_text(format!("{caption}\n{}", tr("clique para abrir")));
                ui.label(
                    egui::RichText::new(&caption)
                        .size(11.0)
                        .color(pal.text_weak),
                );
                r.clicked()
            }
            None => {
                // Chip com spinner enquanto a imagem baixa/decodifica.
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(
                        egui::RichText::new(&caption)
                            .size(11.0)
                            .color(pal.text_weak),
                    );
                });
                false
            }
        }
    } else {
        let label = format!("{} {caption}", tr("anexo:"));
        let hover = match &remote_err {
            Some(e) => format!("{}\n{e}", a.path),
            None => a.path.clone(),
        };
        ui.add_enabled(
            exists,
            egui::Button::new(egui::RichText::new(label).size(12.0).color(pal.text))
                .fill(pal.card.linear_multiply(0.6))
                .stroke(egui::Stroke::new(1.0, pal.border))
                .corner_radius(8.0),
        )
        .on_hover_text(hover)
        .clicked()
    };
    if clicked {
        if local {
            if let Err(e) = will_platform::open_path(path) {
                acx.toasts
                    .error(trf("não consegui abrir o arquivo: {}", &[&e]));
            }
        } else {
            // Baixa (ou reusa os bytes da miniatura), grava em
            // <temp>/will-ia-app/<nome> e abre no app padrão.
            let p = a.path.clone();
            let name = std::path::Path::new(&a.name)
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|n| !n.is_empty())
                .unwrap_or("anexo")
                .to_string();
            let cached = acx.remote.cache.get(&a.path).cloned();
            acx.toasts.info(tr("Baixando o anexo…"));
            acx.remote.opening.push(acx.svc.call(move |c| async move {
                let bytes = match cached {
                    Some(b) => b.to_vec(),
                    None => c.file_bytes(&p).await?.1,
                };
                let dir = std::env::temp_dir().join("will-ia-app");
                tokio::fs::create_dir_all(&dir)
                    .await
                    .map_err(|e| e.to_string())?;
                let dest = dir.join(&name);
                tokio::fs::write(&dest, bytes)
                    .await
                    .map_err(|e| e.to_string())?;
                will_platform::open_path(&dest)
            }));
        }
    }
}

fn assistant_card(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    cache: &mut egui_commonmark::CommonMarkCache,
    toasts: &mut ui::Toasts,
    speak: &mut SpeakUi,
    text: &str,
    turn: &Value,
) {
    let finished = turn_dt(turn, "finished_at").or_else(|| turn_dt(turn, "started_at"));
    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(pal.card)
            .stroke(egui::Stroke::new(1.0, pal.border))
            .corner_radius(egui::CornerRadius {
                nw: 3,
                ne: 12,
                sw: 12,
                se: 12,
            })
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_max_width(ui.available_width() * 0.88);
                ui.set_width(ui.available_width() * 0.88);
                ui.vertical(|ui| {
                    md(ui, cache, text);
                    ui.add_space(4.0);
                    // rodapé: tempo relativo à esquerda; "i" + copiar à direita
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        when_label(ui, pal, finished.as_ref());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let info =
                                info_icon(ui, pal).on_hover_cursor(egui::CursorIcon::PointingHand);
                            // clique abre um popover fixo; hover mostra o mesmo conteúdo
                            let opened = egui::containers::Popup::menu(&info)
                                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                                .show(|ui| turn_info_body(ui, pal, turn));
                            if opened.is_none() {
                                info.on_hover_ui(|ui| turn_info_body(ui, pal, turn));
                            }
                            if copy_button(ui, pal, tr("copiar a resposta (Markdown)")) {
                                ui.ctx().copy_text(text.to_string());
                                toasts.success(tr("Resposta copiada."));
                            }
                            // ouvir: resumo falável + TTS (só com o módulo de voz pronto)
                            if speak.enabled {
                                let turn_id = ui::s(turn, "id");
                                let mine = speak
                                    .current
                                    .as_ref()
                                    .filter(|(id, _, _)| *id == turn_id);
                                let state = mine.map(|(_, st, _)| *st).unwrap_or(SpeakState::Idle);
                                let summary = mine.and_then(|(_, _, s)| s.clone());
                                let cached = speak.cached.get(&turn_id);
                                let (label, hover): (String, String) = match state {
                                    SpeakState::Idle => match cached {
                                        Some(s) => (
                                            tr("ouvir de novo").into(),
                                            format!("{}\n\n{}", tr("Áudio já gerado: toca na hora, sem gastar tokens."), s),
                                        ),
                                        None => (
                                            tr("ouvir").into(),
                                            tr("Ouvir um resumo falado desta resposta (resumo + voz pela OpenAI).").into(),
                                        ),
                                    },
                                    SpeakState::Summarizing => (
                                        tr("resumindo…").into(),
                                        tr("Preparando um roteiro fluido para ouvir… (clique para cancelar)").into(),
                                    ),
                                    SpeakState::Synthesizing => (
                                        tr("gerando áudio…").into(),
                                        summary.clone().unwrap_or_else(|| tr("Gerando a voz… (clique para cancelar)").into()),
                                    ),
                                    SpeakState::Playing => (
                                        tr("parar").into(),
                                        summary.clone().unwrap_or_else(|| tr("Tocando o resumo (clique para parar)").into()),
                                    ),
                                };
                                if state != SpeakState::Idle {
                                    ui.ctx().request_repaint_after(
                                        std::time::Duration::from_millis(250),
                                    );
                                }
                                // Só uma leitura por vez: outro card em andamento desabilita este.
                                let other_busy = speak.current.is_some() && mine.is_none();
                                ui.add_enabled_ui(!other_busy, |ui| {
                                    if small_button(ui, pal, &label, &hover, state == SpeakState::Playing) {
                                        speak.request = Some(if state == SpeakState::Idle {
                                            (turn_id.clone(), Some(text.to_string()))
                                        } else {
                                            (turn_id.clone(), None)
                                        });
                                    }
                                });
                            }
                        });
                    });
                });
            });
    });
    ui.add_space(8.0);
}

fn error_card(ui: &mut egui::Ui, _pal: &ui::Palette, msg: &str) {
    let red = Tone::Danger.color(ui.visuals().dark_mode);
    egui::Frame::new()
        .fill(red.linear_multiply(0.12))
        .stroke(egui::Stroke::new(1.0, red.linear_multiply(0.6)))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width() * 0.88);
            ui.horizontal_top(|ui| {
                ui.label(
                    egui::RichText::new(tr("Erro"))
                        .font(ui::medium(13.0))
                        .color(red),
                );
                ui.add(
                    egui::Label::new(egui::RichText::new(msg).color(red))
                        .wrap()
                        .selectable(true),
                );
            });
        });
    ui.add_space(8.0);
}

/// Cartão do turno em andamento: cabeçalho animado + linha do tempo dos eventos
/// intermediários (ferramentas, raciocínio, status) + texto parcial em Markdown.
fn live_card(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    cache: &mut egui_commonmark::CommonMarkCache,
    live: &LiveTurn,
) {
    let t = ui.input(|i| i.time);
    let phase = ((t * 2.0) as usize) % 4;
    let dots = ".".repeat(phase);
    let dark = ui.visuals().dark_mode;
    let amber = Tone::Warning.color(dark);
    egui::Frame::new()
        .fill(pal.card)
        .stroke(egui::Stroke::new(1.0, amber.linear_multiply(0.7)))
        .corner_radius(egui::CornerRadius {
            nw: 3,
            ne: 12,
            sw: 12,
            se: 12,
        })
        .inner_margin(egui::Margin::symmetric(14, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width() * 0.88);
            // ---- cabeçalho: pulso âmbar, título, tempo, contadores ----
            ui.horizontal(|ui| {
                ui::pulse_dot(ui);
                ui.label(
                    egui::RichText::new(trf("Claude está trabalhando{}", &[&dots]))
                        .font(ui::medium(13.0))
                        .color(pal.text),
                );
                if let Some(s) = live.started {
                    ui.label(
                        egui::RichText::new(format!("{} s", s.elapsed().as_secs()))
                            .small()
                            .color(pal.text_weak),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let total = live.tools_total();
                    let running = live.tools_running();
                    let tasks = live.open_tasks();
                    if tasks > 0 {
                        ui::badge(ui, &trf("{} subagente(s)", &[&tasks]), Tone::Warning);
                    }
                    if running > tasks {
                        ui::badge(
                            ui,
                            &trf("{} em andamento", &[&(running - tasks)]),
                            Tone::Info,
                        );
                    }
                    if total > 0 {
                        ui.label(
                            egui::RichText::new(trf("{} ferramenta(s)", &[&total]))
                                .small()
                                .color(pal.text_weak),
                        );
                    }
                });
            });

            // ---- linha do tempo ----
            if !live.items.is_empty() || !live.text.is_empty() {
                ui.add_space(6.0);
            }
            let line_color = pal.border;
            let start_y = ui.cursor().top();
            for (idx, item) in live.items.iter().enumerate() {
                live_item(ui, pal, cache, dark, idx, item, false);
            }
            // texto em streaming (ainda aberto)
            if !live.text.is_empty() {
                let caret = if phase.is_multiple_of(2) { "|" } else { " " };
                say_block(ui, pal, cache, &format!("{}{caret}", live.text), 1.0);
            }
            let end_y = ui.cursor().top();
            if end_y > start_y + 4.0 {
                // trilho vertical à esquerda ligando os itens
                let x = ui.min_rect().left() + 9.0;
                ui.painter().line_segment(
                    [egui::pos2(x, start_y + 2.0), egui::pos2(x, end_y - 6.0)],
                    egui::Stroke::new(1.0, line_color),
                );
            }
        });
    ui.add_space(8.0);
}

/// Faixa recolhida acima da resposta com a atividade do turno concluído
/// (ferramentas, falas intermediárias); clique expande a linha do tempo.
fn activity_strip(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    cache: &mut egui_commonmark::CommonMarkCache,
    done: &LiveTurn,
    turn_id: &str,
    loading: bool,
) -> bool {
    let mut wants_load = false;
    let dark = ui.visuals().dark_mode;
    let key = egui::Id::unique(("activity-open", turn_id));
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
    let tools = done.tools_total();
    let failed = done.items.iter().any(|i| {
        matches!(
            i,
            LiveItem::Tool {
                result: Some((_, true)),
                ..
            }
        )
    });
    // Layout vertical explícito: a faixa fica no fluxo do chat e o conteúdo
    // expandido (linha do tempo) empilha abaixo do cabeçalho.
    ui.horizontal_top(|ui| {
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.set_width(ui.available_width() * 0.88);
            egui::Frame::new()
                .fill(if open {
                    pal.card_alt
                } else {
                    egui::Color32::TRANSPARENT
                })
                .stroke(egui::Stroke::new(1.0, pal.border.linear_multiply(0.7)))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(10, 5))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let header = ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let chevron = if open {
                            crate::icons::EXPAND_MORE
                        } else {
                            crate::icons::CHEVRON_RIGHT
                        };
                        ui.label(chevron.rich_text().size(14.0).color(pal.text_weak));
                        ui.label(crate::icons::BUILD.rich_text().size(13.0).color(if failed {
                            Tone::Danger.color(dark)
                        } else {
                            pal.text_weak
                        }));
                        let mut parts: Vec<String> = vec![tr("atividade do turno").to_string()];
                        if tools > 0 {
                            parts.push(trf("{} ferramenta(s)", &[&tools]));
                        }
                        if let Some(secs) = done.finished_secs {
                            parts.push(format!("{secs} s"));
                        }
                        if loading {
                            parts.push(tr("carregando…").to_string());
                        }
                        ui.label(
                            egui::RichText::new(parts.join(" · "))
                                .size(11.5)
                                .color(pal.text_weak),
                        );
                        if failed {
                            ui::badge(ui, tr("com falha"), Tone::Danger);
                        }
                        let bg = done.open_tasks();
                        if bg > 0 {
                            ui::pulse_dot(ui);
                            ui::badge(
                                ui,
                                &trf("{} subagente(s) em segundo plano", &[&bg]),
                                Tone::Warning,
                            );
                        }
                    });
                    if full_row_click(ui, &header.response.rect, ("activity-hdr", turn_id))
                        .clicked()
                    {
                        open = !open;
                        ui.ctx().data_mut(|d| d.insert_temp(key, open));
                    }
                    if open {
                        ui.add_space(6.0);
                        if done.items.is_empty() {
                            // ainda não baixado do servidor
                            wants_load = true;
                            ui::busy(ui, tr("carregando atividade…"));
                        }
                        ui.vertical(|ui| {
                            for (idx, item) in done.items.iter().enumerate() {
                                live_item(ui, pal, cache, dark, idx, item, true);
                            }
                        });
                    }
                });
        });
    });
    ui.add_space(4.0);
    wants_load && !loading
}

/// Reconstrói a linha do tempo de um turno antigo a partir dos eventos gravados
/// no servidor (`GET /api/turns/{id}/events`: `{at, kind, payload}` com o
/// `EngineDelta` serializado) e dos turnos mesclados (mensagens no meio do
/// turno). Os instantes são recriados relativos a agora para manter as
/// durações das ferramentas.
fn live_from_events(
    events: &Value,
    merged: &[(chrono::DateTime<chrono::Utc>, String)],
    finished_secs: Option<u64>,
) -> LiveTurn {
    let now_dt = chrono::Utc::now();
    let now = std::time::Instant::now();
    let to_instant = |dt: chrono::DateTime<chrono::Utc>| -> std::time::Instant {
        let back = (now_dt - dt).to_std().unwrap_or_default();
        now.checked_sub(back).unwrap_or(now)
    };
    // (horário, delta ou mensagem do usuário)
    enum Row {
        Delta(EngineDelta),
        User(String),
    }
    let mut rows: Vec<(chrono::DateTime<chrono::Utc>, Row)> = Vec::new();
    if let Some(arr) = events.as_array() {
        for e in arr {
            let Some(at) = e
                .get("at")
                .and_then(|a| a.as_str())
                .and_then(|a| chrono::DateTime::parse_from_rfc3339(a).ok())
                .map(|a| a.with_timezone(&chrono::Utc))
            else {
                continue;
            };
            let Some(delta) = e
                .get("payload")
                .cloned()
                .and_then(|p| serde_json::from_value::<EngineDelta>(p).ok())
            else {
                continue;
            };
            rows.push((at, Row::Delta(delta)));
        }
    }
    for (at, text) in merged {
        rows.push((*at, Row::User(text.clone())));
    }
    rows.sort_by_key(|(at, _)| *at);
    let mut live = LiveTurn {
        finished_secs,
        ..Default::default()
    };
    for (at, row) in rows {
        let inst = to_instant(at);
        match row {
            Row::Delta(d) => {
                live.push(&d);
                // o `push` carimba `now`; recoloca o horário real
                match live.items.last_mut() {
                    Some(LiveItem::Say { at, .. })
                    | Some(LiveItem::Status { at, .. })
                    | Some(LiveItem::Think { at, .. })
                    | Some(LiveItem::User { at, .. }) => *at = inst,
                    Some(LiveItem::Tool { at, .. }) if matches!(d, EngineDelta::ToolUse { .. }) => {
                        *at = inst
                    }
                    _ => {}
                }
                if let EngineDelta::ToolResult { .. } = d {
                    if let Some(LiveItem::Tool { done_at, .. }) =
                        live.items.iter_mut().rev().find(|i| {
                            matches!(
                                i,
                                LiveItem::Tool {
                                    done_at: Some(_),
                                    ..
                                }
                            )
                        })
                    {
                        *done_at = Some(inst);
                    }
                }
            }
            Row::User(text) => {
                live.flush_text();
                live.items.push(LiveItem::User { text, at: inst });
            }
        }
    }
    live.flush_text();
    live
}

/// Área de clique que cobre a **linha inteira** (toda a largura do `ui`) na
/// altura do conteúdo desenhado — o `InnerResponse` de um `horizontal` só
/// cobre o que foi desenhado, e deixava a maior parte da linha sem reação.
fn full_row_click(
    ui: &mut egui::Ui,
    drawn: &egui::Rect,
    salt: impl std::hash::Hash + std::fmt::Debug,
) -> egui::Response {
    let full = ui.max_rect();
    let rect = egui::Rect::from_min_max(
        egui::pos2(full.left(), drawn.top()),
        egui::pos2(full.right(), drawn.bottom()),
    );
    ui.interact(
        rect,
        egui::Id::unique(("full-row", ui.scope_id(), salt)),
        egui::Sense::click(),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Opacidade de entrada (0→1 em ~280 ms) a partir do instante em que o item chegou.
fn enter_alpha(at: std::time::Instant) -> f32 {
    (at.elapsed().as_secs_f32() / 0.28).clamp(0.0, 1.0)
}

/// Marcador na linha do tempo: círculo (ou ícone) alinhado ao trilho.
const MARK_W: f32 = 18.0;
const MARK_H: f32 = 18.0;

fn timeline_mark(
    ui: &mut egui::Ui,
    color: egui::Color32,
    icon: Option<crate::icons::MaterialIcon>,
    center_y: f32,
) {
    // Em linhas `horizontal` (centralizadas) `center_y = MARK_H / 2` põe o
    // marcador no eixo do texto; em blocos multilinha, o chamador passa o
    // centro da primeira linha.
    let h = MARK_H.max(center_y + MARK_H / 2.0);
    let (r, _) = ui.allocate_exact_size(egui::vec2(MARK_W, h), egui::Sense::hover());
    let c = egui::pos2(r.center().x, r.top() + center_y);
    // apaga o trilho atrás do marcador
    ui.painter().circle_filled(c, 7.0, ui.visuals().panel_fill);
    match icon {
        Some(ic) => {
            ui.put(
                egui::Rect::from_center_size(c, egui::vec2(16.0, 16.0)),
                egui::Label::new(ic.rich_text().size(13.0).color(color)),
            );
        }
        None => {
            ui.painter().circle_filled(c, 3.5, color);
        }
    }
}

fn live_item(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    cache: &mut egui_commonmark::CommonMarkCache,
    dark: bool,
    idx: usize,
    item: &LiveItem,
    finished: bool,
) {
    // Linhas de uma só altura (ferramenta, status, raciocínio) usam `horizontal`
    // com alinhamento central: marcador e texto ficam na mesma linha por
    // construção. O bloco de fala (multilinha) alinha o marcador ao centro da
    // primeira linha, calculado pela altura real da fonte.
    match item {
        LiveItem::Say { text, at } => {
            let line_h = ui.text_style_height(&egui::TextStyle::Body);
            ui.horizontal_top(|ui| {
                timeline_mark(
                    ui,
                    pal.accent,
                    Some(crate::icons::CHAT_BUBBLE),
                    6.0 + line_h / 2.0,
                );
                ui.vertical(|ui| say_block(ui, pal, cache, text, enter_alpha(*at)));
            });
        }
        LiveItem::Tool {
            name,
            input,
            result,
            at,
            done_at,
        } => {
            let alpha = enter_alpha(*at);
            let (icon, short, server) = tool_display(name);
            let (status_color, status_icon) = match result {
                None => (Tone::Warning.color(dark), None),
                Some((_, false)) => (Tone::Success.color(dark), Some(crate::icons::CHECK_CIRCLE)),
                Some((_, true)) => (Tone::Danger.color(dark), Some(crate::icons::ERROR)),
            };
            let key = egui::Id::unique(("live-tool-open", idx, name.as_str()));
            let mut open = ui.ctx().data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
            ui.vertical(|ui| {
                ui.scope(|ui| {
                    ui.set_opacity(alpha);
                    let row = ui.horizontal(|ui| {
                        timeline_mark(ui, status_color, Some(icon), MARK_H / 2.0);
                        egui::Frame::new()
                            .fill(if open {
                                pal.card_alt
                            } else {
                                egui::Color32::TRANSPARENT
                            })
                            .corner_radius(6.0)
                            .inner_margin(egui::Margin::symmetric(6, 3))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    ui.label(
                                        egui::RichText::new(&short)
                                            .font(ui::medium(12.5))
                                            .color(pal.text),
                                    );
                                    if let Some(srv) = &server {
                                        ui.label(
                                            egui::RichText::new(srv)
                                                .size(10.5)
                                                .color(pal.text_weak),
                                        );
                                    }
                                    let summary = tool_summary(input);
                                    let right_w = 70.0;
                                    let avail = (ui.available_width() - right_w).max(60.0);
                                    ui.scope(|ui| {
                                        ui.set_max_width(avail);
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(summary)
                                                    .monospace()
                                                    .size(11.5)
                                                    .color(pal.text_weak),
                                            )
                                            .truncate(),
                                        );
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| match result {
                                            None if finished => {
                                                ui.label(
                                                    egui::RichText::new(tr("sem resultado"))
                                                        .size(10.5)
                                                        .color(pal.text_weak),
                                                );
                                            }
                                            None => {
                                                ui.add(egui::Spinner::new().size(12.0));
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{:.0}s",
                                                        at.elapsed().as_secs_f32()
                                                    ))
                                                    .size(10.5)
                                                    .color(pal.text_weak),
                                                );
                                            }
                                            Some((_, failed)) => {
                                                if let Some(ic) = status_icon {
                                                    ui.label(
                                                        ic.rich_text()
                                                            .size(14.0)
                                                            .color(status_color),
                                                    );
                                                }
                                                let took = done_at
                                                    .map(|d| d.duration_since(*at))
                                                    .unwrap_or_default();
                                                ui.label(
                                                    egui::RichText::new(format!(
                                                        "{}{}",
                                                        if *failed { tr("falhou · ") } else { "" },
                                                        ui::fmt_ms(took.as_millis() as u64)
                                                    ))
                                                    .size(10.5)
                                                    .color(if *failed {
                                                        status_color
                                                    } else {
                                                        pal.text_weak
                                                    }),
                                                );
                                            }
                                        },
                                    );
                                });
                            });
                    });
                    let resp = full_row_click(ui, &row.response.rect, ("tool-row", idx))
                        .on_hover_text(if open {
                            tr("recolher detalhes")
                        } else {
                            tr("ver entrada e resultado")
                        });
                    if resp.clicked() {
                        open = !open;
                        ui.ctx().data_mut(|d| d.insert_temp(key, open));
                    }
                    if open {
                        ui.horizontal_top(|ui| {
                            ui.add_space(MARK_W + 6.0);
                            ui.vertical(|ui| tool_details(ui, pal, dark, input, result.as_ref()));
                        });
                    }
                });
            });
        }
        LiveItem::User { text, at } => {
            ui.horizontal(|ui| {
                timeline_mark(
                    ui,
                    Tone::Info.color(dark),
                    Some(crate::icons::SEND),
                    MARK_H / 2.0,
                );
                ui.scope(|ui| {
                    ui.set_opacity(enter_alpha(*at));
                    egui::Frame::new()
                        .fill(Tone::Info.color(dark).linear_multiply(if dark {
                            0.25
                        } else {
                            0.12
                        }))
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::symmetric(8, 4))
                        .show(ui, |ui| {
                            let shown = if text.trim().is_empty() {
                                tr("mensagem entregue no meio do turno").to_string()
                            } else {
                                text.split_whitespace().collect::<Vec<_>>().join(" ")
                            };
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("{} {}", tr("você:"), shown))
                                        .size(12.0)
                                        .color(pal.text),
                                )
                                .truncate(),
                            )
                            .on_hover_text(text);
                        });
                });
            });
        }
        LiveItem::Status { text, at } => {
            ui.horizontal(|ui| {
                timeline_mark(ui, pal.text_weak, None, MARK_H / 2.0);
                ui.scope(|ui| {
                    ui.set_opacity(enter_alpha(*at));
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(text)
                                .small()
                                .italics()
                                .color(pal.text_weak),
                        )
                        .truncate(),
                    )
                    .on_hover_text(text);
                });
            });
        }
        LiveItem::Think { text, at } => {
            ui.horizontal_top(|ui| {
                let line_h = ui.text_style_height(&egui::TextStyle::Small);
                timeline_mark(
                    ui,
                    Tone::Warning.color(dark),
                    Some(crate::icons::PSYCHOLOGY),
                    2.0 + line_h / 2.0,
                );
                ui.scope(|ui| {
                    ui.set_opacity(enter_alpha(*at));
                    egui::CollapsingHeader::new(
                        egui::RichText::new(tr("raciocínio"))
                            .small()
                            .color(pal.text_weak),
                    )
                    .id_salt(("think", idx))
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(text)
                                    .italics()
                                    .size(12.5)
                                    .color(pal.text_weak),
                            )
                            .wrap(),
                        );
                    });
                });
            });
        }
    }
    ui.add_space(2.0);
}

/// Bloco de fala do assistente: borda de destaque à esquerda + Markdown.
fn say_block(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    cache: &mut egui_commonmark::CommonMarkCache,
    text: &str,
    alpha: f32,
) {
    ui.scope(|ui| {
        ui.set_opacity(alpha);
        let r = egui::Frame::new()
            .fill(pal.card_alt)
            .corner_radius(8.0)
            .inner_margin(egui::Margin {
                left: 12,
                right: 10,
                top: 6,
                bottom: 6,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                md(ui, cache, text);
            })
            .response
            .rect;
        ui.painter().rect_filled(
            egui::Rect::from_min_max(r.left_top(), egui::pos2(r.left() + 3.0, r.bottom())),
            egui::CornerRadius::same(2),
            pal.accent,
        );
    });
}

/// Entrada e resultado da ferramenta, formatados, em áreas roláveis curtas.
fn tool_details(
    ui: &mut egui::Ui,
    pal: &ui::Palette,
    dark: bool,
    input: &str,
    result: Option<&(String, bool)>,
) {
    let mono =
        |s: &str, color: egui::Color32| egui::RichText::new(s).monospace().size(11.5).color(color);
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(tr("entrada"))
            .small()
            .color(pal.text_weak),
    );
    egui::ScrollArea::vertical()
        .id_salt(("tool-in", input.len()))
        .max_height(160.0)
        .show(ui, |ui| {
            ui.add(egui::Label::new(mono(&pretty_json(input), pal.text)).wrap());
        });
    if let Some((out, failed)) = result {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(if *failed {
                tr("resultado (erro)")
            } else {
                tr("resultado")
            })
            .small()
            .color(if *failed {
                Tone::Danger.color(dark)
            } else {
                pal.text_weak
            }),
        );
        egui::ScrollArea::vertical()
            .id_salt(("tool-out", out.len()))
            .max_height(200.0)
            .show(ui, |ui| {
                ui.add(egui::Label::new(mono(&pretty_json(out), pal.text)).wrap());
            });
    }
    ui.add_space(2.0);
}

/// Ferramenta que delega a um subagente (pode continuar em segundo plano).
fn is_task_tool(name: &str) -> bool {
    matches!(name, "Task" | "Agent")
}

/// Ícone, nome curto e servidor MCP (se houver) de uma ferramenta.
fn tool_display(name: &str) -> (crate::icons::MaterialIcon, String, Option<String>) {
    use crate::icons as ic;
    // mcp__<servidor>__<ferramenta>
    if let Some(rest) = name.strip_prefix("mcp__") {
        if let Some((srv, tool)) = rest.split_once("__") {
            return (ic::EXTENSION, tool.to_string(), Some(srv.replace('_', "-")));
        }
    }
    let icon = match name {
        "Bash" => ic::TERMINAL,
        "Read" | "Glob" => ic::DESCRIPTION,
        "Grep" => ic::SEARCH,
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ic::EDIT,
        "WebFetch" | "WebSearch" => ic::LANGUAGE,
        "Task" | "Agent" => ic::SMART_TOY,
        "TodoWrite" => ic::SCHEDULE,
        _ => ic::BUILD,
    };
    (icon, name.to_string(), None)
}

/// Resumo de uma linha dos argumentos: os campos que importam (`command`,
/// `file_path`, `pattern`…) como `chave: valor`; sem JSON, o texto cru.
fn tool_summary(input: &str) -> String {
    const KEYS: &[&str] = &[
        "command",
        "description",
        "file_path",
        "path",
        "pattern",
        "query",
        "q",
        "url",
        "prompt",
        "text",
        "message",
        "title",
        "name",
        "id",
        "chat",
        "to",
        "subject",
    ];
    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(input) {
        let mut parts = Vec::new();
        for k in KEYS {
            if let Some(v) = map.get(*k) {
                let v = match v {
                    Value::String(s) => one_line(s),
                    other => one_line(&other.to_string()),
                };
                if v.is_empty() {
                    continue;
                }
                let v: String = v.chars().take(80).collect();
                parts.push(format!("{k}: {v}"));
                if parts.len() == 2 {
                    break;
                }
            }
        }
        if parts.is_empty() {
            for (k, v) in map.iter().take(2) {
                let v = one_line(&v.to_string());
                parts.push(format!("{k}: {}", v.chars().take(60).collect::<String>()));
            }
        }
        return parts.join(" · ");
    }
    one_line(input).chars().take(160).collect()
}

/// JSON indentado quando o texto é JSON; caso contrário, o texto como veio.
fn pretty_json(s: &str) -> String {
    serde_json::from_str::<Value>(s)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| s.to_string())
}

#[allow(dead_code)]
fn _assert_types(_: TurnResult) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo(id: u64, text: &str) -> Echo {
        Echo {
            local_id: id,
            text: text.into(),
            files: vec![],
            sent_at: chrono::Utc::now(),
            turn_id: None,
            acked_at: None,
            error: None,
        }
    }

    fn turn(id: &str, prompt: &str, started: chrono::DateTime<chrono::Utc>) -> Value {
        json!({ "id": id, "prompt": prompt, "prompt_display": prompt, "started_at": started.to_rfc3339() })
    }

    #[test]
    fn reconcile_drops_echo_when_turn_id_is_listed() {
        let mut e = echo(1, "oi");
        e.turn_id = Some("t1".into());
        let mut echoes = vec![e, echo(2, "outra")];
        let turns = vec![turn("t1", "oi", chrono::Utc::now())];
        reconcile_echoes(&mut echoes, &turns, std::time::Instant::now());
        assert_eq!(echoes.len(), 1);
        assert_eq!(echoes[0].local_id, 2);
    }

    #[test]
    fn reconcile_drops_echo_when_list_is_newer_than_ack() {
        let mut e = echo(1, "oi");
        e.turn_id = Some("t-sumido".into());
        e.acked_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        let mut echoes = vec![e];
        reconcile_echoes(&mut echoes, &[], std::time::Instant::now());
        assert!(echoes.is_empty());
        // lista mais velha que a confirmação: o eco fica
        let mut e = echo(1, "oi");
        e.turn_id = Some("t-sumido".into());
        e.acked_at = Some(std::time::Instant::now());
        let mut echoes = vec![e];
        reconcile_echoes(
            &mut echoes,
            &[],
            std::time::Instant::now() - std::time::Duration::from_secs(5),
        );
        assert_eq!(echoes.len(), 1);
    }

    #[test]
    fn reconcile_matches_by_text_before_ack() {
        // o POST ainda não voltou, mas `TurnStarted` já recarregou a lista
        let mut echoes = vec![echo(1, "  manda ver  ")];
        let turns = vec![turn("t9", "manda ver", chrono::Utc::now())];
        reconcile_echoes(&mut echoes, &turns, std::time::Instant::now());
        assert!(echoes.is_empty());
        // mesmo texto, mas turno antigo: não é este envio
        let mut echoes = vec![echo(1, "manda ver")];
        let old = chrono::Utc::now() - chrono::Duration::minutes(10);
        let turns = vec![turn("t0", "manda ver", old)];
        reconcile_echoes(&mut echoes, &turns, std::time::Instant::now());
        assert_eq!(echoes.len(), 1);
    }

    #[test]
    fn reconcile_keeps_failed_echo() {
        let mut e = echo(1, "oi");
        e.error = Some("500".into());
        e.turn_id = Some("t1".into());
        let mut echoes = vec![e];
        let turns = vec![turn("t1", "oi", chrono::Utc::now())];
        reconcile_echoes(&mut echoes, &turns, std::time::Instant::now());
        assert_eq!(echoes.len(), 1);
    }

    #[test]
    fn dictation_merges_with_a_single_space() {
        assert_eq!(merge_dictation("", "oi"), "oi");
        assert_eq!(merge_dictation("antes", "oi"), "antes oi");
        assert_eq!(merge_dictation("antes ", "oi"), "antes oi");
        assert_eq!(merge_dictation("linha\n", "oi"), "linha\noi");
    }

    #[test]
    fn drafts_follow_the_selected_thread() {
        let a = ThreadId::new();
        let b = ThreadId::new();
        let mut p = ThreadsPage {
            selected: Some(a),
            ..Default::default()
        };
        p.sync_draft();
        p.input = "rascunho da A".into();
        p.pending_files.push("/tmp/x.png".into());
        // abre B: a caixa esvazia e o rascunho da A fica guardado
        p.selected = Some(b);
        p.sync_draft();
        assert!(p.input.is_empty() && p.pending_files.is_empty());
        assert_eq!(p.drafts[&a].text, "rascunho da A");
        p.input = "B".into();
        // volta para A: o rascunho dela volta (texto + anexo) e o de B fica guardado
        p.selected = Some(a);
        p.sync_draft();
        assert_eq!(p.input, "rascunho da A");
        assert_eq!(p.pending_files.len(), 1);
        assert_eq!(p.drafts[&b].text, "B");
        assert!(!p.drafts.contains_key(&a));
        // sem troca, nada muda
        p.sync_draft();
        assert_eq!(p.input, "rascunho da A");
    }

    #[test]
    fn mime_by_extension() {
        assert_eq!(guess_mime(std::path::Path::new("a/b.JPG")), "image/jpeg");
        assert_eq!(guess_mime(std::path::Path::new("x.png")), "image/png");
        assert_eq!(
            guess_mime(std::path::Path::new("sem-ext")),
            "application/octet-stream"
        );
    }
}
