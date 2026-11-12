//! will-tasks — tarefas com início/fim e prioridade, vistas num Gantt no app e
//! manipuladas pelo agente (`tasks_*`). Ver `docs/31-tarefas.md`.
//!
//! O estado de cada tarefa é derivado do relógio (`Task::state_at`). Um laço
//! único (`run_loop`) acorda a cada 30 s, nota quem **entrou em andamento** ou
//! **em atraso** e, se as flags das configurações pedirem, avisa no Telegram
//! (destino padrão) por um [`TaskNotifier`] injetado pelo servidor. A marca
//! `notified_*` no banco garante um aviso só por transição.

pub mod sync;
pub mod tools;

pub use sync::{CalendarBridge, CalendarInfo, NoCalendar, SyncReport, SyncRequest};

use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Offset, TimeZone, Utc};
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use will_core::settings::TasksSettings;
use will_core::{Event, EventBus, Module, ModuleStatus, Task, TaskId, TaskPriority, TaskState};
use will_storage::tasks::{NewTask, TaskPatch};
use will_storage::Storage;

pub const FAMILY: &str = "tasks";
const TICK: Duration = Duration::from_secs(30);
/// Sem tarefa "chegando", de quanto em quanto o laço do bipe confere.
/// Espera antes da primeira sincronização automática e passo de conferência.
const AUTO_SYNC_WARMUP: Duration = Duration::from_secs(90);
const AUTO_SYNC_POLL: Duration = Duration::from_secs(20);
const BEEP_IDLE: Duration = Duration::from_secs(5);
/// Teto da espera do laço do bipe (mudança de configuração vale em até isso).
const BEEP_MAX_WAIT: Duration = Duration::from_secs(10);
pub(crate) const TITLE_MAX: usize = 120;
pub(crate) const DESCRIPTION_MAX: usize = 8_000;

/// Quem entrega o aviso (o servidor liga ao Telegram, destino padrão).
#[async_trait]
pub trait TaskNotifier: Send + Sync {
    /// Texto em HTML básico do Telegram (`<b>`, `<i>`). `Err` = não entregue.
    async fn notify(&self, text: &str) -> Result<(), String>;
    /// Bipe curto de "tarefa está chegando" na máquina do serviço (o servidor
    /// toca pela saída de áudio). Padrão: nada.
    fn soon_beep(&self) {}
}

pub struct TasksDeps {
    pub storage: Storage,
    pub bus: EventBus,
    pub notifier: Arc<dyn TaskNotifier>,
    /// Google Calendar (sincronização e exclusão espelhada); [`NoCalendar`] sem o módulo.
    pub calendar: Arc<dyn CalendarBridge>,
}

/// Entrada de criação/edição (agente, UI ou API). Datas: ISO-8601 com fuso ou
/// `YYYY-MM-DDTHH:MM` / `YYYY-MM-DD HH:MM` no fuso das configurações.
/// Na edição, `end_at: ""` limpa o fim.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct TaskInput {
    pub title: Option<String>,
    pub description: Option<String>,
    pub start_at: Option<String>,
    pub end_at: Option<String>,
    /// `high|medium|low` (ou alta/média/baixa, 1/2/3).
    pub priority: Option<String>,
    /// Etiquetas (texto livre; normalizadas ao salvar). Na edição, substitui a lista.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// Tarefa + campos derivados: estado, rótulos e horários no fuso configurado.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskView {
    #[serde(flatten)]
    pub task: Task,
    pub state: TaskState,
    pub state_label: String,
    pub priority_rank: u8,
    pub priority_label: String,
    /// `YYYY-MM-DDTHH:MM` no fuso configurado (para formulários).
    pub start_local: String,
    pub end_local: Option<String>,
    pub done_local: Option<String>,
    /// Legível: `10/11 14:00`.
    pub start_label: String,
    pub end_label: Option<String>,
}

/// Fuso das tarefas: IANA válido ou o do sistema.
#[derive(Debug, Clone, Copy)]
pub enum Zone {
    Tz(chrono_tz::Tz),
    Local,
}

impl Zone {
    pub fn parse(name: &str) -> Zone {
        match name.trim().parse::<chrono_tz::Tz>() {
            Ok(tz) => Zone::Tz(tz),
            Err(_) => Zone::Local,
        }
    }
    pub fn name(&self) -> String {
        match self {
            Zone::Tz(tz) => tz.name().to_string(),
            Zone::Local => "local".into(),
        }
    }
    /// Deslocamento atual em minutos (o app desenha o Gantt com ele).
    pub fn offset_minutes(&self) -> i32 {
        let now = Utc::now();
        let secs = match self {
            Zone::Tz(tz) => now.with_timezone(tz).offset().fix().local_minus_utc(),
            Zone::Local => now.with_timezone(&chrono::Local).offset().local_minus_utc(),
        };
        secs / 60
    }
    pub fn format(&self, at: DateTime<Utc>, pat: &str) -> String {
        match self {
            Zone::Tz(tz) => at.with_timezone(tz).format(pat).to_string(),
            Zone::Local => at.with_timezone(&chrono::Local).format(pat).to_string(),
        }
    }
    pub(crate) fn resolve_naive(&self, n: NaiveDateTime) -> Result<DateTime<Utc>, String> {
        let r = match self {
            Zone::Tz(tz) => tz
                .from_local_datetime(&n)
                .earliest()
                .map(|d| d.with_timezone(&Utc)),
            Zone::Local => chrono::Local
                .from_local_datetime(&n)
                .earliest()
                .map(|d| d.with_timezone(&Utc)),
        };
        r.ok_or_else(|| "horário inexistente neste fuso".into())
    }
}

/// Aceita ISO-8601 com fuso (`2026-11-10T14:00:00-03:00`), `YYYY-MM-DDTHH:MM[:SS]`
/// ou `YYYY-MM-DD HH:MM[:SS]` (no fuso dado). Só data → erro (a hora é obrigatória).
pub fn parse_datetime(s: &str, zone: Zone) -> Result<DateTime<Utc>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("data vazia".into());
    }
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Ok(d.with_timezone(&Utc));
    }
    if s.ends_with('Z') {
        if let Ok(d) = DateTime::parse_from_rfc3339(&s.replace('Z', "+00:00")) {
            return Ok(d.with_timezone(&Utc));
        }
    }
    let norm = s.replacen(' ', "T", 1);
    for pat in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(&norm, pat) {
            return zone.resolve_naive(n);
        }
    }
    if chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok() {
        return Err(format!(
            "\"{s}\" não tem hora: use YYYY-MM-DDTHH:MM (a hora é obrigatória)"
        ));
    }
    Err(format!(
        "data inválida \"{s}\": use YYYY-MM-DDTHH:MM (fuso configurado) ou ISO-8601 com fuso"
    ))
}

pub struct TasksModule {
    deps: TasksDeps,
    cfg: RwLock<TasksSettings>,
    cancel: RwLock<Option<CancellationToken>>,
    /// Uma sincronização com a agenda por vez.
    sync_lock: tokio::sync::Mutex<()>,
}

impl TasksModule {
    pub fn new(deps: TasksDeps, cfg: TasksSettings) -> Arc<Self> {
        Arc::new(Self {
            deps,
            cfg: RwLock::new(cfg),
            cancel: RwLock::new(None),
            sync_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub fn config(&self) -> TasksSettings {
        self.cfg.read().clone()
    }

    pub fn enabled(&self) -> bool {
        self.cfg.read().enabled
    }

    pub fn zone(&self) -> Zone {
        Zone::parse(&self.cfg.read().timezone)
    }

    pub fn update_config(self: &Arc<Self>, cfg: TasksSettings) {
        tracing::debug!(enabled = cfg.enabled, timezone = %cfg.timezone, notify_start = cfg.notify_start, notify_overdue = cfg.notify_overdue, "configuração de tarefas aplicada");
        *self.cfg.write() = cfg;
        self.start();
    }

    fn set_status(&self, st: ModuleStatus) {
        self.deps.bus.emit(Event::ModuleStatus {
            module: Module::Tasks,
            status: st,
        });
    }

    /// Sobe o laço (idempotente); desligado nas configurações, para.
    pub fn start(self: &Arc<Self>) {
        if !self.enabled() {
            self.stop();
            self.set_status(ModuleStatus::Disabled);
            return;
        }
        if self.cancel.read().is_some() {
            self.set_status(ModuleStatus::Running);
            return;
        }
        let token = CancellationToken::new();
        *self.cancel.write() = Some(token.clone());
        let me = self.clone();
        let beep_token = token.clone();
        tokio::spawn(async move { me.beep_loop(beep_token).await });
        let me = self.clone();
        let sync_token = token.clone();
        tokio::spawn(async move { me.auto_sync_loop(sync_token).await });
        let me = self.clone();
        tokio::spawn(async move {
            me.set_status(ModuleStatus::Running);
            tracing::info!(module = FAMILY, status = "running", "laço de tarefas no ar");
            me.run_loop(token).await;
            tracing::info!(
                module = FAMILY,
                status = "stopped",
                "laço de tarefas parado"
            );
        });
    }

    pub fn stop(&self) {
        if let Some(t) = self.cancel.write().take() {
            t.cancel();
        }
    }

    async fn run_loop(self: Arc<Self>, token: CancellationToken) {
        loop {
            self.tick().await;
            tokio::select! {
                _ = token.cancelled() => break,
                _ = tokio::time::sleep(TICK) => {}
            }
        }
    }

    /// Tarefas abertas que começam em até `soon_minutes` (o "está chegando").
    async fn soon_count(&self, soon_minutes: u32) -> usize {
        let Ok(list) = self.deps.storage.list_tasks(false).await else {
            return 0;
        };
        let now = Utc::now();
        let secs = i64::from(soon_minutes) * 60;
        list.iter().filter(|t| t.starts_within(now, secs)).count()
    }

    /// Bipe do "está chegando": toca quando a primeira tarefa entra na janela
    /// (`soon_minutes`) e repete a cada `soon_beep_seconds` enquanto houver
    /// alguma. Sai do serviço, então toca com o app fechado. `0` em qualquer
    /// um dos dois desliga. Sem tarefa na janela, confere a cada [`BEEP_IDLE`].
    async fn beep_loop(self: Arc<Self>, token: CancellationToken) {
        let mut last: Option<tokio::time::Instant> = None;
        loop {
            let (soon_minutes, every) = {
                let c = self.cfg.read();
                (c.soon_minutes, c.soon_beep_seconds)
            };
            let mut wait = BEEP_IDLE;
            if soon_minutes == 0 || every == 0 {
                last = None;
            } else {
                let every = Duration::from_secs(u64::from(every));
                let due = last.is_none_or(|t| t.elapsed() >= every);
                if !due {
                    wait = every.saturating_sub(last.map(|t| t.elapsed()).unwrap_or_default());
                } else if self.soon_count(soon_minutes).await > 0 {
                    self.deps.notifier.soon_beep();
                    last = Some(tokio::time::Instant::now());
                    wait = every;
                } else {
                    last = None;
                }
            }
            tokio::select! {
                _ = token.cancelled() => break,
                _ = tokio::time::sleep(wait.clamp(Duration::from_millis(200), BEEP_MAX_WAIT)) => {}
            }
        }
    }

    /// Sincronização automática com a agenda: a mesma do botão (hoje → +2
    /// meses, agenda das configurações), a cada `sync_interval_minutes` (0
    /// desliga). A primeira roda [`AUTO_SYNC_WARMUP`] depois de o serviço subir,
    /// para o módulo Google estar no ar. Sem conta Google conectada, a rodada
    /// só registra o motivo e espera a próxima.
    async fn auto_sync_loop(self: Arc<Self>, token: CancellationToken) {
        let mut next = tokio::time::Instant::now() + AUTO_SYNC_WARMUP;
        let mut seen = self.cfg.read().sync_interval_minutes;
        loop {
            tokio::select! {
                _ = token.cancelled() => break,
                _ = tokio::time::sleep(AUTO_SYNC_POLL) => {}
            }
            let minutes = self.cfg.read().sync_interval_minutes;
            if minutes != seen {
                // intervalo mudou nas configurações: conta de novo a partir de agora
                seen = minutes;
                next = tokio::time::Instant::now() + Duration::from_secs(u64::from(minutes) * 60);
            }
            if minutes == 0 || tokio::time::Instant::now() < next {
                continue;
            }
            next = tokio::time::Instant::now() + Duration::from_secs(u64::from(minutes) * 60);
            match self.sync_calendar(sync::SyncRequest::default()).await {
                Ok(r) => {
                    let moved = r.pulled_created
                        + r.pulled_updated
                        + r.pulled_deleted
                        + r.pushed_created
                        + r.pushed_updated
                        + r.pushed_deleted;
                    if moved > 0 {
                        self.deps.bus.log(
                            "info",
                            FAMILY,
                            format!(
                                "sincronização automática com \"{}\": agenda → tarefas {} nova(s), {} atualizada(s), {} removida(s); tarefas → agenda {} nova(s), {} atualizada(s), {} removida(s)",
                                r.calendar,
                                r.pulled_created,
                                r.pulled_updated,
                                r.pulled_deleted,
                                r.pushed_created,
                                r.pushed_updated,
                                r.pushed_deleted
                            ),
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(module = FAMILY, error = %e, "sincronização automática não rodou")
                }
            }
        }
    }

    /// Nota transições (futura → em andamento, em andamento → atrasada) e avisa.
    async fn tick(&self) {
        let Ok(list) = self.deps.storage.list_tasks(false).await else {
            return;
        };
        let now = Utc::now();
        let (notify_start, notify_overdue, notify_native) = {
            let c = self.cfg.read();
            (c.notify_start, c.notify_overdue, c.notify_native)
        };
        let zone = self.zone();
        let mut changed = false;
        for t in list {
            let state = t.state_at(now);
            if matches!(state, TaskState::Running | TaskState::Overdue)
                && t.notified_start_at.is_none()
            {
                changed = true;
                if let Err(e) = self.deps.storage.mark_task_notified(t.id, false).await {
                    tracing::warn!(task_id = %t.id, error = %e, "marca de início não gravada");
                }
                tracing::info!(task_id = %t.id, title = %t.title, "tarefa entrou em andamento");
                if state == TaskState::Running {
                    if notify_native {
                        self.deps.bus.emit(Event::Notification {
                            title: "Tarefa em andamento".into(),
                            body: format!(
                                "{} · início {}",
                                t.title,
                                zone.format(t.start_at, "%d/%m %H:%M")
                            ),
                        });
                    }
                    if notify_start {
                        self.send(&self.text_started(&t)).await;
                    }
                }
            }
            if state == TaskState::Overdue && t.notified_overdue_at.is_none() {
                changed = true;
                if let Err(e) = self.deps.storage.mark_task_notified(t.id, true).await {
                    tracing::warn!(task_id = %t.id, error = %e, "marca de atraso não gravada");
                }
                tracing::info!(task_id = %t.id, title = %t.title, "tarefa entrou em atraso");
                if notify_native {
                    self.deps.bus.emit(Event::Notification {
                        title: "Tarefa atrasada".into(),
                        body: match t.end_at {
                            Some(e) => {
                                format!("{} · venceu {}", t.title, zone.format(e, "%d/%m %H:%M"))
                            }
                            None => t.title.clone(),
                        },
                    });
                }
                if notify_overdue {
                    self.send(&self.text_overdue(&t)).await;
                }
            }
        }
        if changed {
            self.deps.bus.emit(Event::TasksChanged);
        }
    }

    async fn send(&self, text: &str) {
        match self.deps.notifier.notify(text).await {
            Ok(()) => tracing::debug!("aviso de tarefa enviado"),
            Err(e) => {
                tracing::warn!(error = %e, "aviso de tarefa não enviado");
                self.deps
                    .bus
                    .log("warn", FAMILY, format!("aviso de tarefa não enviado: {e}"));
            }
        }
    }

    fn text_started(&self, t: &Task) -> String {
        let z = self.zone();
        let mut s = format!(
            "▶️ <b>Tarefa em andamento</b>\n{}\n🕒 início {}",
            esc(&t.title),
            z.format(t.start_at, "%d/%m %H:%M")
        );
        if let Some(e) = t.end_at {
            s.push_str(&format!(" · fim {}", z.format(e, "%d/%m %H:%M")));
        }
        s.push_str(&format!("\n🚩 prioridade {}", t.priority.label_pt()));
        if !t.description.trim().is_empty() {
            s.push_str(&format!(
                "\n<i>{}</i>",
                esc(&will_core::tools::truncate(t.description.trim(), 300))
            ));
        }
        s
    }

    fn text_overdue(&self, t: &Task) -> String {
        let z = self.zone();
        let mut s = format!("⏰ <b>Tarefa atrasada</b>\n{}", esc(&t.title));
        if let Some(e) = t.end_at {
            s.push_str(&format!("\n🕒 venceu {}", z.format(e, "%d/%m %H:%M")));
        }
        s.push_str(&format!("\n🚩 prioridade {}", t.priority.label_pt()));
        s.push_str("\n<i>marque como concluída no app ou peça ao agente (tasks_complete)</i>");
        s
    }

    // ---------- CRUD ----------

    pub fn view(&self, t: Task) -> TaskView {
        let z = self.zone();
        let state = t.state_at(Utc::now());
        TaskView {
            state,
            state_label: state.label_pt().into(),
            priority_rank: t.priority.rank(),
            priority_label: t.priority.label_pt().into(),
            start_local: z.format(t.start_at, "%Y-%m-%dT%H:%M"),
            end_local: t.end_at.map(|e| z.format(e, "%Y-%m-%dT%H:%M")),
            done_local: t.done_at.map(|e| z.format(e, "%Y-%m-%dT%H:%M")),
            start_label: z.format(t.start_at, "%d/%m %H:%M"),
            end_label: t.end_at.map(|e| z.format(e, "%d/%m %H:%M")),
            task: t,
        }
    }

    fn check_title(s: &str) -> Result<String, String> {
        let t = s.trim();
        if t.is_empty() {
            return Err("título vazio".into());
        }
        if t.chars().count() > TITLE_MAX {
            return Err(format!("título com mais de {TITLE_MAX} caracteres"));
        }
        Ok(t.to_string())
    }

    fn check_description(s: &str) -> Result<String, String> {
        if s.chars().count() > DESCRIPTION_MAX {
            return Err(format!(
                "descrição com mais de {DESCRIPTION_MAX} caracteres"
            ));
        }
        Ok(s.trim().to_string())
    }

    fn check_range(start: DateTime<Utc>, end: Option<DateTime<Utc>>) -> Result<(), String> {
        if let Some(e) = end {
            if e <= start {
                return Err("o fim precisa ser depois do início".into());
            }
        }
        Ok(())
    }

    fn parse_priority(s: Option<&str>) -> Result<Option<TaskPriority>, String> {
        match s.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(p) => TaskPriority::parse(p)
                .map(Some)
                .ok_or_else(|| format!("prioridade inválida \"{p}\": use high, medium ou low")),
        }
    }

    pub async fn create(&self, input: TaskInput, created_by: &str) -> Result<TaskView, String> {
        let zone = self.zone();
        let title = Self::check_title(input.title.as_deref().unwrap_or(""))?;
        let description = Self::check_description(input.description.as_deref().unwrap_or(""))?;
        let start_at = parse_datetime(
            input
                .start_at
                .as_deref()
                .ok_or("start_at obrigatório (data e hora de início)")?,
            zone,
        )
        .map_err(|e| format!("início: {e}"))?;
        let end_at = match input
            .end_at
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(e) => Some(parse_datetime(e, zone).map_err(|e| format!("fim: {e}"))?),
            None => None,
        };
        Self::check_range(start_at, end_at)?;
        let priority = Self::parse_priority(input.priority.as_deref())?.unwrap_or_default();
        let t = self
            .deps
            .storage
            .insert_task(&NewTask {
                title,
                description,
                start_at,
                end_at,
                priority,
                created_by: created_by.into(),
                done_at: None,
                skipped: false,
                link: None,
                tags: will_core::task::normalize_tags(input.tags.unwrap_or_default()),
            })
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!(task_id = %t.id, title = %t.title, created_by, "tarefa criada");
        self.deps.bus.emit(Event::TasksChanged);
        Ok(self.view(t))
    }

    /// Tags em uso (todas as tarefas), em ordem alfabética.
    pub async fn tags(&self) -> Result<Vec<String>, String> {
        self.deps
            .storage
            .list_task_tags()
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn resolve_id(&self, s: &str) -> Result<TaskId, String> {
        self.deps
            .storage
            .resolve_task_id(s)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("tarefa \"{s}\" não encontrada (use o id ou um prefixo único)"))
    }

    pub async fn get(&self, id: TaskId) -> Result<TaskView, String> {
        let t = self
            .deps
            .storage
            .get_task(id)
            .await
            .map_err(|e| e.to_string())?;
        Ok(self.view(t))
    }

    /// `filter`: `pending` (padrão: tudo que não foi fechado), `running`,
    /// `overdue`, `upcoming`, `done`, `skipped`, `all`.
    pub async fn list(&self, filter: &str) -> Result<Vec<TaskView>, String> {
        let include_done = matches!(filter, "all" | "done" | "skipped");
        let list = self
            .deps
            .storage
            .list_tasks(include_done)
            .await
            .map_err(|e| e.to_string())?;
        let want = match filter {
            "running" => Some(TaskState::Running),
            "overdue" => Some(TaskState::Overdue),
            "upcoming" => Some(TaskState::Upcoming),
            "done" => Some(TaskState::Done),
            "skipped" => Some(TaskState::Skipped),
            "all" | "pending" | "" => None,
            other => {
                return Err(format!(
                "filtro inválido \"{other}\": use pending, running, overdue, upcoming, done, skipped ou all"
            ))
            }
        };
        Ok(list
            .into_iter()
            .map(|t| self.view(t))
            .filter(|v| want.is_none_or(|w| v.state == w))
            .collect())
    }

    pub async fn update(&self, id: TaskId, input: TaskInput) -> Result<TaskView, String> {
        let zone = self.zone();
        let cur = self
            .deps
            .storage
            .get_task(id)
            .await
            .map_err(|e| e.to_string())?;
        let mut p = TaskPatch::default();
        if let Some(t) = &input.title {
            p.title = Some(Self::check_title(t)?);
        }
        if let Some(d) = &input.description {
            p.description = Some(Self::check_description(d)?);
        }
        if let Some(s) = input
            .start_at
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            p.start_at = Some(parse_datetime(s, zone).map_err(|e| format!("início: {e}"))?);
        }
        if let Some(e) = input.end_at.as_deref() {
            let e = e.trim();
            p.end_at = Some(if e.is_empty() {
                None
            } else {
                Some(parse_datetime(e, zone).map_err(|e| format!("fim: {e}"))?)
            });
        }
        if let Some(pr) = Self::parse_priority(input.priority.as_deref())? {
            p.priority = Some(pr);
        }
        if let Some(tags) = input.tags {
            p.tags = Some(will_core::task::normalize_tags(tags));
        }
        let start = p.start_at.unwrap_or(cur.start_at);
        let end = match p.end_at {
            Some(e) => e,
            None => cur.end_at,
        };
        Self::check_range(start, end)?;
        let t = self
            .deps
            .storage
            .update_task(id, &p)
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!(task_id = %t.id, title = %t.title, "tarefa alterada");
        self.deps.bus.emit(Event::TasksChanged);
        Ok(self.view(t))
    }

    /// Conclui (`true`) ou reabre (`false`).
    pub async fn set_done(&self, id: TaskId, done: bool) -> Result<TaskView, String> {
        self.close(id, done, false).await
    }

    /// Fecha como **não realizada**: some da tela como a concluída, mas fica
    /// registrado que não foi feita (reabrir limpa).
    pub async fn skip(&self, id: TaskId) -> Result<TaskView, String> {
        self.close(id, true, true).await
    }

    async fn close(&self, id: TaskId, done: bool, skipped: bool) -> Result<TaskView, String> {
        let t = self
            .deps
            .storage
            .set_task_done(id, done, skipped)
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!(task_id = %t.id, title = %t.title, done, skipped, "tarefa fechada/reaberta");
        self.deps.bus.emit(Event::TasksChanged);
        Ok(self.view(t))
    }

    /// Remove a tarefa; espelhada na agenda, apaga o evento também (lápide se falhar).
    pub async fn delete(&self, id: TaskId) -> Result<(), String> {
        let cur = self
            .deps
            .storage
            .get_task(id)
            .await
            .map_err(|e| e.to_string())?;
        self.deps
            .storage
            .delete_task(id)
            .await
            .map_err(|e| e.to_string())?;
        if cur.is_linked() {
            self.unlink_remote(&cur).await;
        }
        tracing::info!(task_id = %id, "tarefa removida");
        self.deps.bus.emit(Event::TasksChanged);
        Ok(())
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_local_and_iso() {
        let z = Zone::parse("America/Sao_Paulo");
        let a = parse_datetime("2026-11-10T14:00", z).unwrap();
        assert_eq!(a.to_rfc3339(), "2026-11-10T17:00:00+00:00");
        let b = parse_datetime("2026-11-10 14:00:30", z).unwrap();
        assert_eq!(b.to_rfc3339(), "2026-11-10T17:00:30+00:00");
        let c = parse_datetime("2026-11-10T14:00:00-03:00", z).unwrap();
        assert_eq!(c, a);
        let d = parse_datetime("2026-11-10T17:00:00Z", z).unwrap();
        assert_eq!(d, a);
        assert!(parse_datetime("2026-11-10", z)
            .unwrap_err()
            .contains("hora"));
        assert!(parse_datetime("ontem", z).is_err());
    }

    #[test]
    fn zone_fallback_and_offset() {
        assert_eq!(Zone::parse("America/Sao_Paulo").offset_minutes(), -180);
        assert!(matches!(Zone::parse("Marte/Olympus"), Zone::Local));
    }
}
