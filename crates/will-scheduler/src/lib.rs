//! will-scheduler — agendamentos (cron recorrente ou data única) que viram
//! mensagem na thread dona quando chega a hora. Ferramentas `schedule_*` para o
//! agente; API/UI para o usuário. Ver `docs/24-scheduler.md`.
//!
//! Modelo: um laço único (`run_loop`) dorme até o próximo `next_run_at` (ou no
//! máximo 60 s) e dispara o que venceu. Disparos perdidos com o app fechado são
//! **ignorados**: na partida, cada cron recalcula a próxima ocorrência a partir
//! de agora, e um agendamento único no passado é pausado com motivo.
//! Ao disparar, se a thread dona foi apagada, ela é recriada com a mesma origem
//! e sessão (`ThreadHost::revive_thread`); a mensagem sempre chega a alguém.

pub mod cron;
pub mod tools;

use chrono::{DateTime, Utc};
use cron::Zone;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use will_core::settings::SchedulerSettings;
use will_core::{
    Event, EventBus, Module, ModuleStatus, Origin, OriginKind, Schedule, ScheduleId,
    ScheduleTrigger, SharedThreadHost, ThreadId,
};
use will_storage::schedules::NewSchedule;
use will_storage::Storage;

pub const FAMILY: &str = "scheduler";
const MAX_SLEEP: Duration = Duration::from_secs(60);
const NAME_MAX: usize = 80;
const MESSAGE_MAX: usize = 8_000;

pub struct SchedulerDeps {
    pub storage: Storage,
    pub host: SharedThreadHost,
    pub bus: EventBus,
}

/// Entrada de criação/edição vinda do agente, da UI ou da API.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct ScheduleInput {
    pub name: Option<String>,
    pub message: Option<String>,
    /// Expressão cron (exclusivo com `run_at`).
    pub cron: Option<String>,
    /// Data/hora única (ISO-8601; sem fuso = fuso do agendamento).
    pub run_at: Option<String>,
    pub timezone: Option<String>,
    pub enabled: Option<bool>,
    /// Só UI/API: thread dona. O agente usa a thread de onde chamou.
    pub thread_id: Option<String>,
}

/// Agendamento + campos derivados para listas (próximo disparo legível etc.).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduleView {
    #[serde(flatten)]
    pub schedule: Schedule,
    pub trigger_short: String,
    /// Descrição do cron em inglês (biblioteca), vazia para únicos.
    pub cron_description: String,
    pub timezone_label: String,
    pub next_run_local: Option<String>,
    pub next_run_in: Option<String>,
    pub thread_title: Option<String>,
    pub thread_exists: bool,
}

pub struct SchedulerModule {
    deps: SchedulerDeps,
    cfg: RwLock<SchedulerSettings>,
    wake: Notify,
    cancel: RwLock<Option<CancellationToken>>,
}

impl SchedulerModule {
    pub fn new(deps: SchedulerDeps, cfg: SchedulerSettings) -> Arc<Self> {
        Arc::new(Self {
            deps,
            cfg: RwLock::new(cfg),
            wake: Notify::new(),
            cancel: RwLock::new(None),
        })
    }

    pub fn config(&self) -> SchedulerSettings {
        self.cfg.read().clone()
    }

    pub fn enabled(&self) -> bool {
        self.cfg.read().enabled
    }

    /// Aplica configuração nova; liga/desliga o laço conforme `enabled`.
    pub fn update_config(self: &Arc<Self>, cfg: SchedulerSettings) {
        *self.cfg.write() = cfg;
        self.start();
        self.wake.notify_one();
    }

    /// Sobe o laço (idempotente). Com `enabled = false` para o laço, se houver.
    pub fn start(self: &Arc<Self>) {
        if !self.enabled() {
            self.stop();
            self.set_status(ModuleStatus::Disabled);
            return;
        }
        if self.cancel.read().is_some() {
            return;
        }
        let token = CancellationToken::new();
        *self.cancel.write() = Some(token.clone());
        let me = self.clone();
        tokio::spawn(async move {
            me.reconcile_on_start().await;
            me.set_status(ModuleStatus::Running);
            me.run_loop(token).await;
        });
    }

    pub fn stop(&self) {
        if let Some(t) = self.cancel.write().take() {
            t.cancel();
        }
    }

    fn set_status(&self, status: ModuleStatus) {
        self.deps.bus.emit(Event::ModuleStatus {
            module: Module::Scheduler,
            status,
        });
    }

    fn log(&self, level: &str, msg: impl Into<String>) {
        self.deps.bus.log(level, FAMILY, msg);
    }

    fn zone_for(&self, tz: Option<&str>) -> Result<Zone, String> {
        let cfg = self.cfg.read();
        Zone::parse(tz.or(cfg.timezone.as_deref()))
    }

    /// Próximo disparo a partir de `now` (cron) ou o instante único, se futuro.
    fn compute_next(
        &self,
        s: &Schedule,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, String> {
        let zone = self.zone_for(s.timezone.as_deref())?;
        Ok(match &s.trigger {
            ScheduleTrigger::Cron { expr } => cron::next_after(&cron::parse_cron(expr)?, zone, now),
            ScheduleTrigger::Once { at } => (*at > now).then_some(*at),
        })
    }

    // ----- ciclo de vida -----

    /// Na partida: ignora disparos perdidos. Cron → próxima a partir de agora;
    /// único no passado → pausa com motivo.
    async fn reconcile_on_start(&self) {
        let now = Utc::now();
        let Ok(list) = self.deps.storage.list_schedules().await else {
            return;
        };
        for s in list.into_iter().filter(|s| s.enabled) {
            match &s.trigger {
                ScheduleTrigger::Cron { .. } => {
                    let next = self.compute_next(&s, now).ok().flatten();
                    let missed = s.next_run_at.map(|n| n < now).unwrap_or(false);
                    if missed {
                        self.log(
                            "warn",
                            format!(
                                "\"{}\": disparo de {} perdido com o app fechado; próximo {}",
                                s.name,
                                s.next_run_at.map(|d| d.to_rfc3339()).unwrap_or_default(),
                                next.map(|d| d.to_rfc3339())
                                    .unwrap_or_else(|| "nenhum".into())
                            ),
                        );
                    }
                    if next != s.next_run_at {
                        let _ = self.deps.storage.set_schedule_next(s.id, next, None).await;
                    }
                }
                ScheduleTrigger::Once { at } => {
                    if *at <= now {
                        self.log(
                            "warn",
                            format!(
                                "\"{}\": horário único {} passou com o app fechado; pausado",
                                s.name,
                                at.to_rfc3339()
                            ),
                        );
                        let _ = self
                            .deps
                            .storage
                            .set_schedule_next(
                                s.id,
                                None,
                                Some("perdido: o app estava fechado no horário"),
                            )
                            .await;
                    } else if s.next_run_at != Some(*at) {
                        let _ = self
                            .deps
                            .storage
                            .set_schedule_next(s.id, Some(*at), None)
                            .await;
                    }
                }
            }
        }
        self.deps.bus.emit(Event::SchedulesChanged);
    }

    async fn run_loop(self: Arc<Self>, token: CancellationToken) {
        loop {
            let sleep_for = self.fire_due().await;
            tokio::select! {
                _ = token.cancelled() => break,
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(sleep_for) => {}
            }
        }
    }

    /// Dispara tudo que venceu e devolve quanto dormir até o próximo.
    async fn fire_due(&self) -> Duration {
        let now = Utc::now();
        let list = match self.deps.storage.list_schedules().await {
            Ok(l) => l,
            Err(e) => {
                self.log("error", format!("falha ao ler agendamentos: {e}"));
                return MAX_SLEEP;
            }
        };
        let mut earliest: Option<DateTime<Utc>> = None;
        for s in list.into_iter().filter(|s| s.enabled) {
            let Some(next) = s.next_run_at else { continue };
            if next <= now {
                self.fire(s).await;
            } else {
                earliest = Some(earliest.map_or(next, |e: DateTime<Utc>| e.min(next)));
            }
        }
        match earliest {
            Some(t) => {
                let d = (t - Utc::now()).to_std().unwrap_or(Duration::ZERO);
                d.min(MAX_SLEEP)
            }
            None => MAX_SLEEP,
        }
    }

    /// Entrega a mensagem na thread dona (recriando-a se sumiu) e agenda a próxima.
    async fn fire(&self, s: Schedule) {
        let now = Utc::now();
        let zone = self.zone_for(s.timezone.as_deref()).unwrap_or(Zone::Local);
        let text = will_core::schedule::fired_message(&s, &zone.format(now));
        let outcome = self.deliver(&s, &text).await;
        let next = match &s.trigger {
            ScheduleTrigger::Cron { .. } => self.compute_next(&s, now).ok().flatten(),
            ScheduleTrigger::Once { .. } => None,
        };
        let (thread_id, ok, detail) = match &outcome {
            Ok(t) => (Some(*t), true, None),
            Err(e) => (None, false, Some(e.as_str())),
        };
        // Único: concluído se entregue; se falhou, pausa com o motivo (o usuário
        // vê em amarelo e pode "Disparar agora" depois de corrigir a causa).
        let failed_once: String;
        let paused: Option<&str> = match (&s.trigger, &next, ok) {
            (ScheduleTrigger::Once { .. }, _, true) => Some("concluído (único)"),
            (ScheduleTrigger::Once { .. }, _, false) => {
                failed_once = format!("falha na entrega: {}", detail.unwrap_or("erro"));
                Some(failed_once.as_str())
            }
            (_, None, _) => Some("sem próxima ocorrência"),
            _ => None,
        };
        if let Err(e) = self
            .deps
            .storage
            .record_schedule_run(s.id, thread_id, ok, detail, next, paused)
            .await
        {
            self.log(
                "error",
                format!("falha ao registrar disparo de \"{}\": {e}", s.name),
            );
        }
        match &outcome {
            Ok(t) => self.log(
                "info",
                format!(
                    "\"{}\" disparou na thread {} (próximo: {})",
                    s.name,
                    t,
                    next.map(|d| d.to_rfc3339())
                        .unwrap_or_else(|| "nenhum".into())
                ),
            ),
            Err(e) => self.log("error", format!("\"{}\" falhou ao disparar: {e}", s.name)),
        }
        self.deps.bus.emit(Event::ScheduleFired {
            schedule_id: s.id,
            thread_id,
            name: s.name.clone(),
            ok,
            detail: detail.map(|d| d.to_string()),
        });
        self.deps.bus.emit(Event::SchedulesChanged);
        let notify = self.cfg.read().notify_on_fire;
        if notify && s.origin.kind() != OriginKind::Telegram {
            let body = match &outcome {
                Ok(_) => format!(
                    "\"{}\" disparou. A resposta do agente aparece na thread.",
                    s.name
                ),
                Err(e) => format!("\"{}\" não pôde ser entregue: {e}", s.name),
            };
            self.deps.bus.emit(Event::Notification {
                title: "Agendamento".into(),
                body,
            });
        }
    }

    async fn deliver(&self, s: &Schedule, text: &str) -> Result<ThreadId, String> {
        let host = &self.deps.host;
        let thread = match host.get_thread(s.thread_id).await {
            Ok(t) => t.id,
            Err(_) => {
                self.log(
                    "warn",
                    format!(
                        "thread {} de \"{}\" não existe mais; recriando com a sessão salva",
                        s.thread_id, s.name
                    ),
                );
                let t = host
                    .revive_thread(&s.origin, Some(&s.name), s.claude_session_id.as_deref())
                    .await?;
                t.id
            }
        };
        host.send_message(thread, text).await?;
        Ok(thread)
    }

    // ----- operações (ferramentas, API, UI) -----

    fn validate_text(name: &str, message: &str) -> Result<(), String> {
        if name.trim().is_empty() {
            return Err("nome obrigatório".into());
        }
        if name.chars().count() > NAME_MAX {
            return Err(format!("nome com mais de {NAME_MAX} caracteres"));
        }
        if message.trim().is_empty() {
            return Err("mensagem obrigatória".into());
        }
        if message.chars().count() > MESSAGE_MAX {
            return Err(format!("mensagem com mais de {MESSAGE_MAX} caracteres"));
        }
        Ok(())
    }

    /// Converte `cron`/`run_at` da entrada em gatilho validado.
    fn trigger_from(
        &self,
        input: &ScheduleInput,
        zone: Zone,
    ) -> Result<Option<ScheduleTrigger>, String> {
        let cron_s = input
            .cron
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let at_s = input
            .run_at
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match (cron_s, at_s) {
            (Some(_), Some(_)) => Err("informe `cron` ou `run_at`, não os dois".into()),
            (None, None) => Ok(None),
            (Some(c), None) => {
                let parsed = cron::parse_cron(c)?;
                let min = self.cfg.read().min_interval_minutes;
                cron::check_min_interval(&parsed, zone, min)?;
                Ok(Some(ScheduleTrigger::Cron {
                    expr: c.to_string(),
                }))
            }
            (None, Some(a)) => {
                let at = cron::parse_run_at(a, zone)?;
                if at <= Utc::now() + chrono::Duration::seconds(5) {
                    return Err(format!(
                        "`run_at` precisa estar no futuro ({} já passou)",
                        zone.format(at)
                    ));
                }
                Ok(Some(ScheduleTrigger::Once { at }))
            }
        }
    }

    /// Cria. `thread`: dona (do contexto da ferramenta ou escolhida na UI).
    pub async fn create(
        &self,
        input: ScheduleInput,
        thread: ThreadId,
        created_by: &str,
    ) -> Result<ScheduleView, String> {
        if !self.enabled() {
            return Err("scheduler desligado nas configurações".into());
        }
        let name = input.name.clone().unwrap_or_default();
        let message = input.message.clone().unwrap_or_default();
        Self::validate_text(&name, &message)?;
        let zone = self.zone_for(input.timezone.as_deref())?;
        let trigger = self
            .trigger_from(&input, zone)?
            .ok_or_else(|| "informe `cron` (recorrente) ou `run_at` (único)".to_string())?;
        let max = self.cfg.read().max_schedules as i64;
        let count = self
            .deps
            .storage
            .count_enabled_schedules()
            .await
            .map_err(|e| e.to_string())?;
        if count >= max {
            return Err(format!(
                "limite de {max} agendamentos ativos atingido; apague ou pause algum"
            ));
        }
        let t = self.deps.host.get_thread(thread).await?;
        let tmp = Schedule {
            id: ScheduleId::new(),
            name: name.trim().to_string(),
            message: message.trim().to_string(),
            trigger: trigger.clone(),
            timezone: input
                .timezone
                .clone()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            thread_id: t.id,
            claude_session_id: t.claude_session_id.clone(),
            origin: t.origin.clone(),
            enabled: input.enabled.unwrap_or(true),
            created_by: created_by.to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            next_run_at: None,
            last_run_at: None,
            last_result: None,
            run_count: 0,
            paused_reason: None,
        };
        let next = self.compute_next(&tmp, Utc::now())?;
        let created = self
            .deps
            .storage
            .insert_schedule(&NewSchedule {
                name: tmp.name,
                message: tmp.message,
                trigger,
                timezone: tmp.timezone,
                thread_id: t.id,
                claude_session_id: t.claude_session_id,
                origin: t.origin,
                enabled: tmp.enabled,
                created_by: created_by.to_string(),
                next_run_at: next,
            })
            .await
            .map_err(|e| e.to_string())?;
        self.log(
            "info",
            format!(
                "\"{}\" criado por {created_by} ({})",
                created.name,
                created.trigger.short()
            ),
        );
        self.changed();
        self.view(created).await
    }

    /// Edita campos presentes em `input`. Trocar `thread_id` só pela UI/API.
    pub async fn update(
        &self,
        id: ScheduleId,
        input: ScheduleInput,
    ) -> Result<ScheduleView, String> {
        let mut s = self
            .deps
            .storage
            .get_schedule(id)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(n) = &input.name {
            s.name = n.trim().to_string();
        }
        if let Some(m) = &input.message {
            s.message = m.trim().to_string();
        }
        Self::validate_text(&s.name, &s.message)?;
        if let Some(tz) = &input.timezone {
            s.timezone = Some(tz.trim().to_string()).filter(|t| !t.is_empty());
        }
        let zone = self.zone_for(s.timezone.as_deref())?;
        if let Some(t) = self.trigger_from(&input, zone)? {
            s.trigger = t;
        }
        if let Some(tid) = input.thread_id.as_deref().filter(|t| !t.is_empty()) {
            let tid = ThreadId::parse(tid).map_err(|_| "thread_id inválido".to_string())?;
            let t = self.deps.host.get_thread(tid).await?;
            s.thread_id = t.id;
            s.claude_session_id = t.claude_session_id;
            s.origin = t.origin;
        }
        if let Some(e) = input.enabled {
            s.enabled = e;
        }
        let now = Utc::now();
        s.next_run_at = if s.enabled {
            self.compute_next(&s, now)?
        } else {
            None
        };
        s.paused_reason = match (&s.enabled, &s.trigger, s.next_run_at) {
            (true, ScheduleTrigger::Once { .. }, None) => {
                s.enabled = false;
                Some("concluído (único)".into())
            }
            (true, _, None) => {
                s.enabled = false;
                Some("sem próxima ocorrência".into())
            }
            _ => None,
        };
        let saved = self
            .deps
            .storage
            .update_schedule(&s)
            .await
            .map_err(|e| e.to_string())?;
        self.changed();
        self.view(saved).await
    }

    pub async fn delete(&self, id: ScheduleId) -> Result<(), String> {
        self.deps
            .storage
            .delete_schedule(id)
            .await
            .map_err(|e| e.to_string())?;
        self.changed();
        Ok(())
    }

    /// Dispara agora, fora do horário (não altera a próxima ocorrência de um cron).
    pub async fn run_now(&self, id: ScheduleId) -> Result<(), String> {
        let s = self
            .deps
            .storage
            .get_schedule(id)
            .await
            .map_err(|e| e.to_string())?;
        let now = Utc::now();
        let zone = self.zone_for(s.timezone.as_deref())?;
        let text =
            will_core::schedule::fired_message(&s, &format!("{} (manual)", zone.format(now)));
        let thread = self.deliver(&s, &text).await?;
        let next = if s.enabled { s.next_run_at } else { None };
        self.deps
            .storage
            .record_schedule_run(s.id, Some(thread), true, Some("manual"), next, None)
            .await
            .map_err(|e| e.to_string())?;
        self.log(
            "info",
            format!("\"{}\" disparado manualmente na thread {thread}", s.name),
        );
        self.changed();
        Ok(())
    }

    pub async fn get(&self, id: ScheduleId) -> Result<ScheduleView, String> {
        let s = self
            .deps
            .storage
            .get_schedule(id)
            .await
            .map_err(|e| e.to_string())?;
        self.view(s).await
    }

    /// Aceita id completo ou prefixo único (o agente costuma abreviar).
    pub async fn resolve_id(&self, id_or_prefix: &str) -> Result<ScheduleId, String> {
        let p = id_or_prefix.trim();
        if let Ok(id) = ScheduleId::parse(p) {
            return Ok(id);
        }
        if p.len() < 4 {
            return Err("id muito curto".into());
        }
        let all = self
            .deps
            .storage
            .list_schedules()
            .await
            .map_err(|e| e.to_string())?;
        let m: Vec<ScheduleId> = all
            .iter()
            .filter(|s| s.id.to_string().starts_with(p))
            .map(|s| s.id)
            .collect();
        match m.as_slice() {
            [one] => Ok(*one),
            [] => Err(format!("agendamento não encontrado: {p}")),
            _ => Err(format!("prefixo ambíguo: {p}")),
        }
    }

    pub async fn list(&self, thread: Option<ThreadId>) -> Result<Vec<ScheduleView>, String> {
        let list = match thread {
            Some(t) => self.deps.storage.list_schedules_for_thread(t).await,
            None => self.deps.storage.list_schedules().await,
        }
        .map_err(|e| e.to_string())?;
        let threads = self.deps.host.list_threads().await.unwrap_or_default();
        let now = Utc::now();
        Ok(list
            .into_iter()
            .map(|s| {
                let t = threads.iter().find(|t| t.id == s.thread_id);
                self.view_with(
                    s,
                    t.map(|t| t.title.clone()).unwrap_or(None),
                    t.is_some(),
                    now,
                )
            })
            .collect())
    }

    pub async fn runs(&self, id: ScheduleId) -> Result<Vec<will_core::ScheduleRun>, String> {
        self.deps
            .storage
            .list_schedule_runs(id, 20)
            .await
            .map_err(|e| e.to_string())
    }

    /// Prévia de uma expressão: próximas ocorrências e descrição (para a UI).
    pub fn preview(
        &self,
        expr: &str,
        tz: Option<&str>,
        n: usize,
    ) -> Result<serde_json::Value, String> {
        let zone = self.zone_for(tz)?;
        let cron = cron::parse_cron(expr)?;
        let min = self.cfg.read().min_interval_minutes;
        let interval_error = cron::check_min_interval(&cron, zone, min).err();
        let now = Utc::now();
        let next: Vec<serde_json::Value> = cron::next_n(&cron, zone, now, n.clamp(1, 20))
            .into_iter()
            .map(|d| serde_json::json!({"at": d, "local": zone.format(d), "in": cron::humanize_until(d, now)}))
            .collect();
        Ok(serde_json::json!({
            "description": cron::describe(&cron),
            "timezone": zone.name(),
            "next": next,
            "error": interval_error,
        }))
    }

    async fn view(&self, s: Schedule) -> Result<ScheduleView, String> {
        let t = self.deps.host.get_thread(s.thread_id).await.ok();
        Ok(self.view_with(
            s,
            t.as_ref().and_then(|t| t.title.clone()),
            t.is_some(),
            Utc::now(),
        ))
    }

    fn view_with(
        &self,
        s: Schedule,
        thread_title: Option<String>,
        thread_exists: bool,
        now: DateTime<Utc>,
    ) -> ScheduleView {
        let zone = self.zone_for(s.timezone.as_deref()).unwrap_or(Zone::Local);
        let cron_description = match &s.trigger {
            ScheduleTrigger::Cron { expr } => cron::parse_cron(expr)
                .map(|c| cron::describe(&c))
                .unwrap_or_default(),
            ScheduleTrigger::Once { .. } => String::new(),
        };
        ScheduleView {
            trigger_short: s.trigger.short(),
            cron_description,
            timezone_label: zone.name(),
            next_run_local: s.next_run_at.map(|d| zone.format(d)),
            next_run_in: s.next_run_at.map(|d| cron::humanize_until(d, now)),
            thread_title,
            thread_exists,
            schedule: s,
        }
    }

    fn changed(&self) {
        self.deps.bus.emit(Event::SchedulesChanged);
        self.wake.notify_one();
    }

    /// Origem de um contexto de ferramenta, para mensagens de erro.
    pub fn origin_label(o: &Origin) -> &'static str {
        match o.kind() {
            OriginKind::Ui => "UI",
            OriginKind::Telegram => "Telegram",
            OriginKind::Realtime => "voz",
        }
    }
}
