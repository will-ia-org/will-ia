//! Sincronização Tarefas ↔ Google Calendar (`docs/31`, seção "Sincronizar
//! com a agenda"). Ação explícita do usuário (botão em Configurações ›
//! Tarefas) sobre uma **janela de datas** e **uma agenda**:
//!
//! - tarefa sem vínculo com início na janela → vira evento (vínculo gravado);
//! - evento da janela sem tarefa → vira tarefa (vinculada);
//! - par já vinculado → **última edição vence** (`updated_at` da tarefa ×
//!   `updated` do evento, ambos contra `synced_at`);
//! - evento vinculado que sumiu do Google → tarefa apagada; tarefa apagada no
//!   app → evento apagado na hora (`TasksModule::delete`), com lápide se falhar.
//!
//! Identidade: a tarefa guarda `calendar_id` + `event_id`; o evento guarda
//! `extendedProperties.private.will_task_id` (recupera o vínculo após um
//! restore em outra máquina). Conclusão = prefixo `✓ ` no título do evento
//! (+ `will_done_at`). Prioridade e "sem fim" vão em propriedades privadas.
//! Evento de dia inteiro vira tarefa 00:00 → 23:59; tarefa 00:00 → 23:59 volta
//! como dia inteiro. Ocorrências de evento recorrente entram uma a uma.
//!
//! Eventos **especiais** do Google (`eventType` ≠ `default`: aniversário de
//! contato, vindo do Gmail, fora do escritório, foco) não aceitam escrita pela
//! API (nem as propriedades privadas do vínculo): entram como tarefa marcada
//! `event_read_only`, que só **recebe** — edição local fica só aqui (contada em
//! `kept_local`) e apagar a tarefa não apaga o evento. Local de trabalho
//! (`workingLocation`) não é compromisso: ignorado.

use crate::{TasksModule, Zone};
use async_trait::async_trait;
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, SubsecRound, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use will_core::{Task, TaskId, TaskPriority};
use will_storage::tasks::{NewTask, RemoteTask, TaskLink};

/// Prefixo do título de evento que marca tarefa concluída.
pub const DONE_PREFIX: &str = "✓ ";
/// Prefixo do título de evento que marca tarefa **não realizada**.
pub const SKIPPED_PREFIX: &str = "✗ ";
const PROP_TASK_ID: &str = "will_task_id";
const PROP_PRIORITY: &str = "will_priority";
const PROP_OPEN_END: &str = "will_open_end";
const PROP_DONE_AT: &str = "will_done_at";
const PROP_SKIPPED: &str = "will_skipped";
/// Fim fictício enviado ao Google para tarefa sem fim (a API exige `end`).
const OPEN_END: Duration = Duration::hours(1);

/// Agenda alvo (resolvida pela ponte).
#[derive(Debug, Clone, Serialize)]
pub struct CalendarInfo {
    pub calendar_id: String,
    pub summary: String,
}

/// Quem fala com o Google Calendar (o servidor liga ao módulo Google).
/// Eventos entram e saem **crus** (JSON da API v3).
#[async_trait]
pub trait CalendarBridge: Send + Sync {
    /// Agenda escolhida (`wanted` = id, vazio/`None` = padrão); precisa ser gravável.
    async fn resolve_calendar(&self, wanted: Option<&str>) -> Result<CalendarInfo, String>;
    /// Ocorrências com início em `[min, max)` (RFC 3339).
    async fn list_events(
        &self,
        calendar_id: &str,
        time_min: &str,
        time_max: &str,
    ) -> Result<Vec<Value>, String>;
    /// `None` = não existe mais (ou cancelado).
    async fn get_event(&self, calendar_id: &str, event_id: &str) -> Result<Option<Value>, String>;
    async fn insert_event(&self, calendar_id: &str, body: &Value) -> Result<Value, String>;
    async fn patch_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        body: &Value,
    ) -> Result<Value, String>;
    /// Já inexistente conta como sucesso.
    async fn delete_event(&self, calendar_id: &str, event_id: &str) -> Result<(), String>;
}

/// Ponte inerte (servidor sem o módulo Google compilado).
pub struct NoCalendar;

#[async_trait]
impl CalendarBridge for NoCalendar {
    async fn resolve_calendar(&self, _: Option<&str>) -> Result<CalendarInfo, String> {
        Err("módulo Google não disponível".into())
    }
    async fn list_events(&self, _: &str, _: &str, _: &str) -> Result<Vec<Value>, String> {
        Err("módulo Google não disponível".into())
    }
    async fn get_event(&self, _: &str, _: &str) -> Result<Option<Value>, String> {
        Err("módulo Google não disponível".into())
    }
    async fn insert_event(&self, _: &str, _: &Value) -> Result<Value, String> {
        Err("módulo Google não disponível".into())
    }
    async fn patch_event(&self, _: &str, _: &str, _: &Value) -> Result<Value, String> {
        Err("módulo Google não disponível".into())
    }
    async fn delete_event(&self, _: &str, _: &str) -> Result<(), String> {
        Err("módulo Google não disponível".into())
    }
}

/// Pedido do botão: janela (datas `YYYY-MM-DD`, inclusivas, no fuso das
/// tarefas) e agenda (vazio = a das configurações / padrão).
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(default)]
pub struct SyncRequest {
    pub from: Option<String>,
    pub to: Option<String>,
    pub calendar_id: Option<String>,
}

/// Resultado mostrado no app.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncReport {
    pub calendar_id: String,
    pub calendar: String,
    pub from: String,
    pub to: String,
    /// Agenda → tarefas.
    pub pulled_created: usize,
    pub pulled_updated: usize,
    pub pulled_deleted: usize,
    /// Tarefas → agenda.
    pub pushed_created: usize,
    pub pushed_updated: usize,
    pub pushed_deleted: usize,
    pub unchanged: usize,
    pub skipped: usize,
    /// Edições locais que **não sobem**: o evento é especial do Google
    /// (aniversário, Gmail, fora do escritório, foco) e não aceita escrita.
    pub kept_local: usize,
    pub warnings: Vec<String>,
    pub duration_ms: u64,
}

/// Evento da API já interpretado.
#[derive(Debug, Clone)]
pub(crate) struct RemoteEvent {
    pub id: String,
    pub title: String,
    pub description: String,
    pub start_at: DateTime<Utc>,
    pub end_at: Option<DateTime<Utc>>,
    pub done_at: Option<DateTime<Utc>>,
    pub skipped: bool,
    pub priority: TaskPriority,
    pub updated: DateTime<Utc>,
    pub task_id: Option<TaskId>,
    pub cancelled: bool,
    /// Eu recusei o convite: não vira tarefa.
    pub declined: bool,
    /// Evento especial do Google: a API não aceita escrita nele.
    pub read_only: bool,
}

/// `eventType` do evento: `default` (ou ausente) é o evento comum, que aceita
/// escrita; `workingLocation` não é compromisso (`None` = ignorar); os demais
/// (`birthday`, `fromGmail`, `outOfOffice`, `focusTime`…) são só de leitura.
fn read_only_kind(ev: &Value) -> Option<bool> {
    match ev.get("eventType").and_then(|t| t.as_str()) {
        None | Some("default") => Some(false),
        Some("workingLocation") => None,
        Some(_) => Some(true),
    }
}

fn prop<'a>(ev: &'a Value, key: &str) -> Option<&'a str> {
    ev.get("extendedProperties")?
        .get("private")?
        .get(key)?
        .as_str()
        .filter(|s| !s.is_empty())
}

fn rfc3339(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Lê `start`/`end` do evento: `dateTime` (com offset) ou `date` (dia inteiro,
/// no fuso das tarefas; fim exclusivo → 23:59 do último dia).
fn when(v: Option<&Value>, zone: Zone, end_of_all_day: bool) -> Option<DateTime<Utc>> {
    let v = v?;
    if let Some(s) = v.get("dateTime").and_then(|d| d.as_str()) {
        return rfc3339(s);
    }
    let d = NaiveDate::parse_from_str(v.get("date")?.as_str()?, "%Y-%m-%d").ok()?;
    let naive = if end_of_all_day {
        (d - Duration::days(1)).and_time(NaiveTime::from_hms_opt(23, 59, 0)?)
    } else {
        d.and_time(NaiveTime::MIN)
    };
    zone.resolve_naive(naive).ok()
}

impl RemoteEvent {
    pub(crate) fn parse(ev: &Value, zone: Zone) -> Option<RemoteEvent> {
        let id = ev.get("id")?.as_str()?.to_string();
        let read_only = read_only_kind(ev)?;
        let cancelled = ev.get("status").and_then(|s| s.as_str()) == Some("cancelled");
        let raw_title = ev
            .get("summary")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .trim();
        let (done_prefixed, skipped_prefixed, title) =
            match raw_title.strip_prefix(DONE_PREFIX.trim_end()) {
                Some(rest) => (true, false, rest.trim()),
                None => match raw_title.strip_prefix(SKIPPED_PREFIX.trim_end()) {
                    Some(rest) => (false, true, rest.trim()),
                    None => (false, false, raw_title),
                },
            };
        let title = if title.is_empty() {
            "(sem título)".to_string()
        } else {
            title.to_string()
        };
        let start_at = when(ev.get("start"), zone, false)?;
        let all_day = ev.get("start").and_then(|s| s.get("date")).is_some();
        let mut end_at = when(ev.get("end"), zone, all_day);
        if !all_day
            && prop(ev, PROP_OPEN_END) == Some("1")
            && end_at.is_some_and(|e| e == start_at + OPEN_END)
        {
            end_at = None;
        }
        if end_at.is_some_and(|e| e <= start_at) {
            end_at = None;
        }
        let updated = remote_updated(ev);
        let skipped = skipped_prefixed || prop(ev, PROP_SKIPPED) == Some("1");
        let done_at = if done_prefixed || skipped || prop(ev, PROP_DONE_AT).is_some() {
            Some(prop(ev, PROP_DONE_AT).and_then(rfc3339).unwrap_or(updated))
        } else {
            None
        };
        let declined = ev
            .get("attendees")
            .and_then(|a| a.as_array())
            .is_some_and(|a| {
                a.iter().any(|p| {
                    p.get("self").and_then(|v| v.as_bool()).unwrap_or(false)
                        && p.get("responseStatus").and_then(|v| v.as_str()) == Some("declined")
                })
            });
        Some(RemoteEvent {
            id,
            title,
            description: ev
                .get("description")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .trim()
                .to_string(),
            start_at,
            end_at,
            done_at,
            skipped,
            priority: prop(ev, PROP_PRIORITY)
                .and_then(TaskPriority::parse)
                .unwrap_or_default(),
            updated,
            task_id: prop(ev, PROP_TASK_ID).and_then(|s| TaskId::parse(s).ok()),
            cancelled,
            declined,
            read_only,
        })
    }

    fn to_remote_task(&self, link: TaskLink) -> RemoteTask {
        RemoteTask {
            title: self.title.clone(),
            description: self.description.clone(),
            start_at: self.start_at,
            end_at: self.end_at,
            done_at: self.done_at,
            skipped: self.skipped,
            priority: self.priority,
            link,
        }
    }
}

/// Tarefa 00:00 → 23:59 (no fuso) é "dia inteiro" para o Google.
fn all_day_span(t: &Task, zone: Zone) -> Option<(NaiveDate, NaiveDate)> {
    let end = t.end_at?;
    let s = zone.format(t.start_at, "%H:%M");
    let e = zone.format(end, "%H:%M");
    if s != "00:00" || e != "23:59" {
        return None;
    }
    let sd = NaiveDate::parse_from_str(&zone.format(t.start_at, "%Y-%m-%d"), "%Y-%m-%d").ok()?;
    let ed = NaiveDate::parse_from_str(&zone.format(end, "%Y-%m-%d"), "%Y-%m-%d").ok()?;
    (ed >= sd).then_some((sd, ed))
}

/// Corpo do evento (insert/patch) a partir da tarefa.
pub(crate) fn event_body(t: &Task, zone: Zone) -> Value {
    let title = match (t.is_done(), t.skipped) {
        (true, true) => format!("{SKIPPED_PREFIX}{}", t.title),
        (true, false) => format!("{DONE_PREFIX}{}", t.title),
        _ => t.title.clone(),
    };
    let (start, end, open_end) = match all_day_span(t, zone) {
        Some((sd, ed)) => (
            json!({ "date": sd.format("%Y-%m-%d").to_string() }),
            json!({ "date": (ed + Duration::days(1)).format("%Y-%m-%d").to_string() }),
            false,
        ),
        None => {
            let dt = |at: DateTime<Utc>| {
                let mut v = json!({ "dateTime": zone.format(at, "%Y-%m-%dT%H:%M:%S%:z") });
                if let Zone::Tz(tz) = zone {
                    v["timeZone"] = json!(tz.name());
                }
                v
            };
            let (end, open) = match t.end_at {
                Some(e) => (e, false),
                None => (t.start_at + OPEN_END, true),
            };
            (dt(t.start_at), dt(end), open)
        }
    };
    json!({
        "summary": title,
        "description": t.description,
        "start": start,
        "end": end,
        "extendedProperties": { "private": {
            PROP_TASK_ID: t.id.to_string(),
            PROP_PRIORITY: t.priority.as_str(),
            PROP_OPEN_END: if open_end { "1" } else { "0" },
            PROP_DONE_AT: t.done_at.map(|d| d.to_rfc3339()).unwrap_or_default(),
            PROP_SKIPPED: if t.is_done() && t.skipped { "1" } else { "0" },
        }},
    })
}

/// `YYYY-MM-DD` → dia no fuso; `from` às 00:00, `to` exclusivo (dia seguinte 00:00).
fn parse_day(s: Option<&str>, fallback: NaiveDate) -> Result<NaiveDate, String> {
    match s.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(fallback),
        Some(s) => NaiveDate::parse_from_str(s.get(..10).unwrap_or(s), "%Y-%m-%d")
            .map_err(|_| format!("data inválida \"{s}\": use YYYY-MM-DD")),
    }
}

/// `updated` do evento (milissegundos, como o Google devolve e o banco guarda).
fn remote_updated(ev: &Value) -> DateTime<Utc> {
    ev.get("updated")
        .and_then(|s| s.as_str())
        .and_then(rfc3339)
        .unwrap_or_else(Utc::now)
        .trunc_subsecs(3)
}

fn link(
    cal_id: &str,
    event_id: &str,
    remote_updated_at: DateTime<Utc>,
    read_only: bool,
) -> TaskLink {
    TaskLink {
        calendar_id: cal_id.to_string(),
        event_id: event_id.to_string(),
        synced_at: Utc::now(),
        remote_updated_at,
        read_only,
    }
}

impl TasksModule {
    /// Roda a sincronização inteira (serializada por `sync_lock`).
    pub async fn sync_calendar(&self, req: SyncRequest) -> Result<SyncReport, String> {
        let _guard = self
            .sync_lock
            .try_lock()
            .map_err(|_| "já existe uma sincronização em andamento".to_string())?;
        let started = std::time::Instant::now();
        let zone = self.zone();
        let now = Utc::now();
        let today = NaiveDate::parse_from_str(&zone.format(now, "%Y-%m-%d"), "%Y-%m-%d")
            .map_err(|e| e.to_string())?;
        let from = parse_day(req.from.as_deref(), today)?;
        let to = parse_day(req.to.as_deref(), today + chrono::Months::new(2))?;
        if to < from {
            return Err("a data final precisa ser igual ou depois da inicial".into());
        }
        if (to - from).num_days() > 400 {
            return Err("janela grande demais (máximo 400 dias)".into());
        }
        let min = zone.resolve_naive(from.and_time(NaiveTime::MIN))?;
        let max = zone.resolve_naive((to + Duration::days(1)).and_time(NaiveTime::MIN))?;
        let wanted = req
            .calendar_id
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.config().sync_calendar_id.clone());
        let bridge = &self.deps.calendar;
        let cal = bridge
            .resolve_calendar(Some(wanted.as_str()).filter(|s| !s.trim().is_empty()))
            .await?;
        let cal_id = cal.calendar_id.clone();
        let mut rep = SyncReport {
            calendar_id: cal_id.clone(),
            calendar: cal.summary.clone(),
            from: from.format("%Y-%m-%d").to_string(),
            to: to.format("%Y-%m-%d").to_string(),
            ..Default::default()
        };
        tracing::info!(calendar_id = %cal_id, from = %rep.from, to = %rep.to, "sincronização de tarefas iniciada");

        // 1. lápides: exclusões que falharam antes (tenta de novo, nunca reimporta)
        let storage = &self.deps.storage;
        let mut tombstones: HashSet<String> = HashSet::new();
        for eid in storage
            .list_task_tombstones(&cal_id)
            .await
            .map_err(|e| e.to_string())?
        {
            match bridge.delete_event(&cal_id, &eid).await {
                Ok(()) => {
                    let _ = storage.remove_task_tombstone(&cal_id, &eid).await;
                    rep.pushed_deleted += 1;
                }
                Err(e) => {
                    rep.warnings
                        .push(format!("exclusão pendente do evento {eid}: {e}"));
                    tombstones.insert(eid);
                }
            }
        }

        // 2. lado remoto
        let raw = bridge
            .list_events(&cal_id, &min.to_rfc3339(), &max.to_rfc3339())
            .await?;
        let mut remote: HashMap<String, RemoteEvent> = HashMap::new();
        let mut by_task: HashMap<TaskId, String> = HashMap::new();
        for ev in &raw {
            let Some(r) = RemoteEvent::parse(ev, zone) else {
                rep.skipped += 1;
                continue;
            };
            if tombstones.contains(&r.id) {
                continue;
            }
            if let Some(tid) = r.task_id {
                by_task.insert(tid, r.id.clone());
            }
            remote.insert(r.id.clone(), r);
        }

        // 3. lado local: vinculadas a esta agenda + soltas com início na janela
        let locals: Vec<Task> = storage
            .list_tasks(true)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|t| match (&t.calendar_id, &t.event_id) {
                (Some(c), Some(_)) => c == &cal_id,
                (Some(_), None) | (None, Some(_)) => false,
                (None, None) => t.start_at >= min && t.start_at < max,
            })
            .collect();
        let mut matched: HashSet<String> = HashSet::new();

        for t in locals {
            // vínculo gravado, ou recuperado pela propriedade privada do evento
            let known = match (&t.calendar_id, &t.event_id) {
                (Some(_), Some(e)) => Some((e.clone(), t.synced_at, t.remote_updated_at)),
                _ => by_task.get(&t.id).map(|e| (e.clone(), None, None)),
            };
            let Some((eid, synced_at, remote_seen)) = known else {
                // tarefa nova → evento novo
                match bridge.insert_event(&cal_id, &event_body(&t, zone)).await {
                    Ok(created) => {
                        let Some(new_id) = created.get("id").and_then(|s| s.as_str()) else {
                            rep.warnings
                                .push(format!("evento de \"{}\" criado sem id", t.title));
                            continue;
                        };
                        let link = link(&cal_id, new_id, remote_updated(&created), false);
                        if let Err(e) = storage.set_task_link(t.id, &link).await {
                            rep.warnings
                                .push(format!("vínculo de \"{}\": {e}", t.title));
                        }
                        matched.insert(new_id.to_string());
                        rep.pushed_created += 1;
                    }
                    Err(e) => rep
                        .warnings
                        .push(format!("criar evento de \"{}\": {e}", t.title)),
                }
                continue;
            };
            matched.insert(eid.clone());
            let ev = match remote.get(&eid) {
                Some(r) => Some(r.clone()),
                None => match bridge.get_event(&cal_id, &eid).await {
                    Ok(Some(v)) => RemoteEvent::parse(&v, zone),
                    Ok(None) => None,
                    Err(e) => {
                        rep.warnings
                            .push(format!("consultar evento de \"{}\": {e}", t.title));
                        continue;
                    }
                },
            };
            let Some(ev) = ev.filter(|e| !e.cancelled) else {
                // sumiu do Google → some daqui
                match storage.delete_task(t.id).await {
                    Ok(()) => rep.pulled_deleted += 1,
                    Err(e) => rep
                        .warnings
                        .push(format!("remover tarefa \"{}\": {e}", t.title)),
                }
                continue;
            };
            let local_changed = synced_at.is_none_or(|s| t.updated_at > s);
            let remote_changed = remote_seen.is_none_or(|r| ev.updated != r);
            let push = match (local_changed, remote_changed) {
                (false, false) => {
                    if t.event_read_only != ev.read_only {
                        // tarefa de antes da marca (ou evento que mudou de tipo)
                        let link = link(&cal_id, &eid, ev.updated, ev.read_only);
                        if let Err(e) = storage.set_task_link(t.id, &link).await {
                            rep.warnings
                                .push(format!("vínculo de \"{}\": {e}", t.title));
                        }
                    }
                    rep.unchanged += 1;
                    continue;
                }
                (true, false) => true,
                (false, true) => false,
                (true, true) => t.updated_at >= ev.updated,
            };
            if push && ev.read_only {
                // o Google não aceita escrita: a edição fica só aqui e a
                // tarefa passa a contar como sincronizada (não insiste)
                let link = link(&cal_id, &eid, ev.updated, true);
                match storage.set_task_link(t.id, &link).await {
                    Ok(()) => rep.kept_local += 1,
                    Err(e) => rep
                        .warnings
                        .push(format!("vínculo de \"{}\": {e}", t.title)),
                }
                continue;
            }
            if push {
                match bridge
                    .patch_event(&cal_id, &eid, &event_body(&t, zone))
                    .await
                {
                    Ok(updated) => {
                        let link = link(&cal_id, &eid, remote_updated(&updated), false);
                        if let Err(e) = storage.set_task_link(t.id, &link).await {
                            rep.warnings
                                .push(format!("vínculo de \"{}\": {e}", t.title));
                        }
                        rep.pushed_updated += 1;
                    }
                    Err(e) => rep
                        .warnings
                        .push(format!("atualizar evento de \"{}\": {e}", t.title)),
                }
            } else {
                let link = link(&cal_id, &eid, ev.updated, ev.read_only);
                match storage
                    .apply_remote_task(t.id, &ev.to_remote_task(link))
                    .await
                {
                    Ok(_) => rep.pulled_updated += 1,
                    Err(e) => rep
                        .warnings
                        .push(format!("atualizar tarefa \"{}\": {e}", t.title)),
                }
            }
        }

        // 4. eventos sem tarefa → tarefas novas
        let mut pending: Vec<&RemoteEvent> = remote
            .values()
            .filter(|r| !matched.contains(&r.id))
            .collect();
        pending.sort_by_key(|r| (r.start_at, r.id.clone()));
        for r in pending {
            if r.cancelled || r.declined {
                rep.skipped += 1;
                continue;
            }
            // o evento diz que pertence a uma tarefa que ainda existe (em outra
            // agenda, ou fora da janela): não duplica
            if let Some(tid) = r.task_id {
                if storage.get_task(tid).await.is_ok() {
                    rep.skipped += 1;
                    continue;
                }
            }
            if let Ok(Some(_)) = storage.find_task_by_event(&cal_id, &r.id).await {
                rep.skipped += 1;
                continue;
            }
            let link = link(&cal_id, &r.id, r.updated, r.read_only);
            let new = NewTask {
                title: will_core::tools::truncate(&r.title, crate::TITLE_MAX),
                description: will_core::tools::truncate(&r.description, crate::DESCRIPTION_MAX),
                start_at: r.start_at,
                end_at: r.end_at,
                priority: r.priority,
                created_by: "calendar".into(),
                done_at: r.done_at,
                skipped: r.skipped,
                link: Some(link),
                tags: vec![],
            };
            match storage.insert_task(&new).await {
                Ok(_) => rep.pulled_created += 1,
                Err(e) => rep.warnings.push(format!("importar \"{}\": {e}", r.title)),
            }
        }

        rep.duration_ms = started.elapsed().as_millis() as u64;
        tracing::info!(
            calendar_id = %cal_id,
            pulled_created = rep.pulled_created,
            pulled_updated = rep.pulled_updated,
            pulled_deleted = rep.pulled_deleted,
            pushed_created = rep.pushed_created,
            pushed_updated = rep.pushed_updated,
            pushed_deleted = rep.pushed_deleted,
            unchanged = rep.unchanged,
            skipped = rep.skipped,
            kept_local = rep.kept_local,
            warnings = rep.warnings.len(),
            duration_ms = rep.duration_ms,
            "sincronização de tarefas concluída"
        );
        for w in &rep.warnings {
            self.deps
                .bus
                .log("warn", crate::FAMILY, format!("sincronização: {w}"));
        }
        self.deps.bus.emit(will_core::Event::TasksChanged);
        Ok(rep)
    }

    /// Apaga o evento espelhado de uma tarefa (na exclusão). Se o Google falhar,
    /// grava uma lápide para a próxima sincronização tentar de novo.
    pub(crate) async fn unlink_remote(&self, t: &Task) {
        let (Some(cal), Some(eid)) = (&t.calendar_id, &t.event_id) else {
            return;
        };
        if t.event_read_only {
            tracing::info!(task_id = %t.id, event_id = %eid, "evento especial do Google fica na agenda (só leitura)");
            return;
        }
        match self.deps.calendar.delete_event(cal, eid).await {
            Ok(()) => {
                tracing::info!(task_id = %t.id, event_id = %eid, "evento da tarefa apagado no Google")
            }
            Err(e) => {
                tracing::warn!(task_id = %t.id, event_id = %eid, error = %e, "evento da tarefa não apagado; lápide gravada");
                self.deps.bus.log(
                    "warn",
                    crate::FAMILY,
                    format!("evento de \"{}\" não apagado no Google ({e}); tento de novo na próxima sincronização", t.title),
                );
                if let Err(e) = self.deps.storage.add_task_tombstone(cal, eid).await {
                    tracing::warn!(error = %e, "lápide não gravada");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn zone() -> Zone {
        Zone::parse("America/Sao_Paulo")
    }

    fn task(start: DateTime<Utc>, end: Option<DateTime<Utc>>, done: bool) -> Task {
        Task {
            id: TaskId::new(),
            title: "Pagar boleto".into(),
            description: "até 18h".into(),
            start_at: start,
            end_at: end,
            done_at: done.then_some(start),
            skipped: false,
            priority: TaskPriority::High,
            created_by: "user".into(),
            created_at: start,
            updated_at: start,
            notified_start_at: None,
            notified_overdue_at: None,
            calendar_id: None,
            event_id: None,
            synced_at: None,
            remote_updated_at: None,
            event_read_only: false,
            tags: vec![],
        }
    }

    #[test]
    fn body_timed_and_open_end() {
        let start = Utc.with_ymd_and_hms(2026, 11, 10, 17, 0, 0).unwrap();
        let t = task(start, None, false);
        let b = event_body(&t, zone());
        assert_eq!(b["summary"], "Pagar boleto");
        assert_eq!(b["start"]["dateTime"], "2026-11-10T14:00:00-03:00");
        assert_eq!(b["start"]["timeZone"], "America/Sao_Paulo");
        assert_eq!(b["end"]["dateTime"], "2026-11-10T15:00:00-03:00");
        assert_eq!(b["extendedProperties"]["private"][PROP_OPEN_END], "1");
        assert_eq!(b["extendedProperties"]["private"][PROP_PRIORITY], "high");
        assert_eq!(
            b["extendedProperties"]["private"][PROP_TASK_ID],
            t.id.to_string()
        );
    }

    #[test]
    fn body_done_and_all_day() {
        let start = Utc.with_ymd_and_hms(2026, 11, 10, 3, 0, 0).unwrap(); // 00:00 -03
        let end = Utc.with_ymd_and_hms(2026, 11, 12, 2, 59, 0).unwrap(); // 11/11 23:59 -03
        let t = task(start, Some(end), true);
        let b = event_body(&t, zone());
        assert_eq!(b["summary"], "✓ Pagar boleto");
        assert_eq!(b["start"]["date"], "2026-11-10");
        assert_eq!(b["end"]["date"], "2026-11-12");
        assert!(b["extendedProperties"]["private"][PROP_DONE_AT]
            .as_str()
            .unwrap()
            .starts_with("2026-11-10"));
    }

    #[test]
    fn parses_timed_event_roundtrip() {
        let ev = json!({
            "id": "abc_20261110T170000Z",
            "summary": "✓ Pagar boleto",
            "description": "até 18h",
            "start": {"dateTime": "2026-11-10T14:00:00-03:00"},
            "end": {"dateTime": "2026-11-10T15:00:00-03:00"},
            "updated": "2026-11-10T10:00:00.000Z",
            "extendedProperties": {"private": {"will_open_end": "1", "will_priority": "low", "will_task_id": "não-é-id"}},
        });
        let r = RemoteEvent::parse(&ev, zone()).unwrap();
        assert_eq!(r.title, "Pagar boleto");
        assert!(r.done_at.is_some());
        assert_eq!(r.end_at, None, "fim fictício de 1h some");
        assert_eq!(r.priority, TaskPriority::Low);
        assert_eq!(r.task_id, None);
        assert_eq!(
            r.start_at,
            Utc.with_ymd_and_hms(2026, 11, 10, 17, 0, 0).unwrap()
        );
    }

    #[test]
    fn skipped_roundtrip() {
        let start = Utc.with_ymd_and_hms(2026, 11, 10, 17, 0, 0).unwrap();
        let mut t = task(start, Some(start + Duration::hours(1)), true);
        t.skipped = true;
        let b = event_body(&t, zone());
        assert_eq!(b["summary"], "✗ Pagar boleto");
        assert_eq!(b["extendedProperties"]["private"][PROP_SKIPPED], "1");
        let ev = json!({
            "id": "s", "summary": "✗ Academia",
            "start": {"dateTime": "2026-11-10T14:00:00-03:00"},
            "end": {"dateTime": "2026-11-10T15:00:00-03:00"},
            "updated": "2026-11-10T10:00:00.000Z",
        });
        let r = RemoteEvent::parse(&ev, zone()).unwrap();
        assert_eq!(r.title, "Academia");
        assert!(r.skipped && r.done_at.is_some());
    }

    #[test]
    fn parses_all_day_event() {
        let ev = json!({
            "id": "x",
            "summary": "Feriado",
            "start": {"date": "2026-11-15"},
            "end": {"date": "2026-11-16"},
            "updated": "2026-11-10T10:00:00Z",
        });
        let r = RemoteEvent::parse(&ev, zone()).unwrap();
        assert_eq!(zone().format(r.start_at, "%d/%m %H:%M"), "15/11 00:00");
        assert_eq!(
            zone().format(r.end_at.unwrap(), "%d/%m %H:%M"),
            "15/11 23:59"
        );
        assert!(!r.cancelled && !r.declined);
    }

    #[test]
    fn skips_declined_and_cancelled() {
        let ev = json!({
            "id": "y", "summary": "Reunião", "status": "cancelled",
            "start": {"dateTime": "2026-11-10T14:00:00-03:00"},
            "end": {"dateTime": "2026-11-10T15:00:00-03:00"},
            "attendees": [{"self": true, "responseStatus": "declined"}],
        });
        let r = RemoteEvent::parse(&ev, zone()).unwrap();
        assert!(r.cancelled && r.declined);
    }
}

#[cfg(test)]
mod e2e {
    //! Rodada completa contra uma agenda falsa em memória.
    use super::*;
    use crate::{TaskInput, TaskNotifier, TasksDeps, TasksModule};
    use chrono::TimeZone;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use will_core::settings::TasksSettings;
    use will_core::EventBus;
    use will_storage::Storage;

    struct Quiet;
    #[async_trait]
    impl TaskNotifier for Quiet {
        async fn notify(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct Fake {
        events: Mutex<HashMap<String, Value>>,
        seq: AtomicUsize,
    }

    impl Fake {
        fn start_of(ev: &Value) -> DateTime<Utc> {
            when(ev.get("start"), zone(), false).unwrap()
        }
        fn put(&self, id: &str, mut ev: Value, updated: DateTime<Utc>) {
            ev["id"] = json!(id);
            ev["updated"] = json!(updated.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
            self.events.lock().insert(id.to_string(), ev);
        }
        fn summary(&self, id: &str) -> String {
            self.events.lock()[id]["summary"]
                .as_str()
                .unwrap()
                .to_string()
        }
    }

    #[async_trait]
    impl CalendarBridge for Fake {
        async fn resolve_calendar(&self, wanted: Option<&str>) -> Result<CalendarInfo, String> {
            assert!(wanted.is_none_or(|w| w == "cal1"));
            Ok(CalendarInfo {
                calendar_id: "cal1".into(),
                summary: "Teste".into(),
            })
        }
        async fn list_events(&self, _: &str, min: &str, max: &str) -> Result<Vec<Value>, String> {
            let (min, max) = (rfc3339(min).unwrap(), rfc3339(max).unwrap());
            Ok(self
                .events
                .lock()
                .values()
                .filter(|e| {
                    let s = Self::start_of(e);
                    s >= min && s < max
                })
                .cloned()
                .collect())
        }
        async fn get_event(&self, _: &str, id: &str) -> Result<Option<Value>, String> {
            Ok(self.events.lock().get(id).cloned())
        }
        async fn insert_event(&self, _: &str, body: &Value) -> Result<Value, String> {
            let id = format!("ev{}", self.seq.fetch_add(1, Ordering::SeqCst));
            self.put(&id, body.clone(), Utc::now());
            Ok(self.events.lock()[&id].clone())
        }
        async fn patch_event(&self, _: &str, id: &str, body: &Value) -> Result<Value, String> {
            let mut all = self.events.lock();
            let ev = all.get_mut(id).ok_or("404")?;
            if ev["eventType"] == "birthday" {
                return Err("Google API 400: 'birthday' event type must not have private extended properties".into());
            }
            for (k, v) in body.as_object().unwrap() {
                ev[k] = v.clone();
            }
            ev["updated"] = json!(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
            Ok(ev.clone())
        }
        async fn delete_event(&self, _: &str, id: &str) -> Result<(), String> {
            let mut all = self.events.lock();
            if all.get(id).is_some_and(|e| e["eventType"] == "birthday") {
                return Err("Google API 403: cannot delete birthday event".into());
            }
            all.remove(id);
            Ok(())
        }
    }

    fn zone() -> Zone {
        Zone::parse("America/Sao_Paulo")
    }

    async fn module(fake: Arc<Fake>) -> Arc<TasksModule> {
        let storage = Storage::open_memory().await.unwrap();
        TasksModule::new(
            TasksDeps {
                storage,
                bus: EventBus::new(16),
                notifier: Arc::new(Quiet),
                calendar: fake,
            },
            TasksSettings::default(),
        )
    }

    fn req() -> SyncRequest {
        SyncRequest {
            from: Some("2026-11-10".into()),
            to: Some("2026-12-10".into()),
            calendar_id: None,
        }
    }

    fn timed(summary: &str, day: u32, hour: u32) -> Value {
        json!({
            "summary": summary,
            "start": {"dateTime": format!("2026-11-{day:02}T{hour:02}:00:00-03:00")},
            "end": {"dateTime": format!("2026-11-{day:02}T{:02}:00:00-03:00", hour + 1)},
        })
    }

    #[tokio::test]
    async fn full_roundtrip() {
        let fake = Arc::new(Fake::default());
        let m = module(fake.clone()).await;
        let t0 = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
        // remoto: B (com hora) e C (dia inteiro); fora da janela: D
        fake.put("B", timed("Dentista", 13, 10), t0);
        fake.put(
            "C",
            json!({"summary": "Feriado", "start": {"date": "2026-11-15"}, "end": {"date": "2026-11-16"}}),
            t0,
        );
        fake.put("D", timed("Longe", 30, 9), t0);
        fake.events.lock().get_mut("D").unwrap()["start"]["dateTime"] =
            json!("2027-01-30T09:00:00-03:00");
        // local: A
        let a = m
            .create(
                TaskInput {
                    title: Some("Pagar boleto".into()),
                    start_at: Some("2026-11-12T14:00".into()),
                    priority: Some("high".into()),
                    ..Default::default()
                },
                "user",
            )
            .await
            .unwrap();

        // 1ª rodada: A sobe, B e C descem
        let r = m.sync_calendar(req()).await.unwrap();
        assert_eq!((r.pushed_created, r.pulled_created), (1, 2), "{r:?}");
        assert_eq!(fake.events.lock().len(), 4);
        let all = m.list("all").await.unwrap();
        assert_eq!(all.len(), 3);
        let a_ev = all
            .iter()
            .find(|v| v.task.id == a.task.id)
            .unwrap()
            .task
            .event_id
            .clone()
            .unwrap();
        assert_eq!(
            fake.events.lock()[&a_ev]["extendedProperties"]["private"][PROP_TASK_ID],
            a.task.id.to_string()
        );
        let c = all.iter().find(|v| v.task.title == "Feriado").unwrap();
        assert_eq!(c.start_local, "2026-11-15T00:00");
        assert_eq!(c.end_local.as_deref(), Some("2026-11-15T23:59"));

        // 2ª rodada: nada muda
        let r = m.sync_calendar(req()).await.unwrap();
        assert_eq!(r.unchanged, 3, "{r:?}");
        assert_eq!(
            r.pulled_created + r.pushed_created + r.pulled_updated + r.pushed_updated,
            0
        );

        // edição local de A → sobe
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        m.update(
            a.task.id,
            TaskInput {
                title: Some("Pagar boleto do carro".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let r = m.sync_calendar(req()).await.unwrap();
        assert_eq!(r.pushed_updated, 1, "{r:?}");
        assert_eq!(fake.summary(&a_ev), "Pagar boleto do carro");

        // edição remota de B (depois) → desce
        let later = Utc::now() + Duration::seconds(5);
        fake.put("B", timed("Dentista (remarcado)", 14, 16), later);
        let r = m.sync_calendar(req()).await.unwrap();
        assert_eq!(r.pulled_updated, 1, "{r:?}");
        let b = m
            .list("all")
            .await
            .unwrap()
            .into_iter()
            .find(|v| v.task.event_id.as_deref() == Some("B"))
            .unwrap();
        assert_eq!(b.task.title, "Dentista (remarcado)");
        assert_eq!(b.start_local, "2026-11-14T16:00");

        // C sumiu do Google → tarefa some
        fake.events.lock().remove("C");
        let r = m.sync_calendar(req()).await.unwrap();
        assert_eq!(r.pulled_deleted, 1, "{r:?}");
        assert_eq!(m.list("all").await.unwrap().len(), 2);

        // apagar A localmente → evento some na hora; nada volta
        m.delete(a.task.id).await.unwrap();
        assert!(!fake.events.lock().contains_key(&a_ev));
        let r = m.sync_calendar(req()).await.unwrap();
        assert_eq!((r.pulled_created, r.unchanged), (0, 1), "{r:?}");

        // concluir B → "✓ " na agenda; reabrir tira
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        m.set_done(b.task.id, true).await.unwrap();
        let r = m.sync_calendar(req()).await.unwrap();
        assert_eq!(r.pushed_updated, 1, "{r:?}");
        assert_eq!(fake.summary("B"), "✓ Dentista (remarcado)");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        m.set_done(b.task.id, false).await.unwrap();
        m.sync_calendar(req()).await.unwrap();
        assert_eq!(fake.summary("B"), "Dentista (remarcado)");
        // D (fora da janela) nunca entrou
        assert!(m
            .list("all")
            .await
            .unwrap()
            .iter()
            .all(|v| v.task.event_id.as_deref() != Some("D")));
    }

    #[tokio::test]
    async fn tombstone_retries_delete() {
        let fake = Arc::new(Fake::default());
        let m = module(fake.clone()).await;
        let t0 = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
        fake.put("B", timed("Dentista", 13, 10), t0);
        m.sync_calendar(req()).await.unwrap();
        let b = m.list("all").await.unwrap().remove(0);
        // simula falha na exclusão remota: lápide gravada direto
        m.deps
            .storage
            .add_task_tombstone("cal1", "B")
            .await
            .unwrap();
        m.deps.storage.delete_task(b.task.id).await.unwrap();
        let r = m.sync_calendar(req()).await.unwrap();
        assert_eq!((r.pushed_deleted, r.pulled_created), (1, 0), "{r:?}");
        assert!(fake.events.lock().is_empty());
        assert!(m
            .deps
            .storage
            .list_task_tombstones("cal1")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn special_events_are_read_only_mirrors() {
        let fake = Arc::new(Fake::default());
        let m = module(fake.clone()).await;
        let t0 = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
        fake.put(
            "T",
            json!({"summary": "Tamara", "eventType": "birthday",
                   "start": {"date": "2026-11-20"}, "end": {"date": "2026-11-21"}}),
            t0,
        );
        fake.put(
            "W",
            json!({"summary": "Escritório", "eventType": "workingLocation",
                   "start": {"date": "2026-11-12"}, "end": {"date": "2026-11-13"}}),
            t0,
        );
        // aniversário entra marcado; local de trabalho é ignorado
        let r = m.sync_calendar(req()).await.unwrap();
        assert_eq!((r.pulled_created, r.skipped), (1, 1), "{r:?}");
        let t = m.list("all").await.unwrap().remove(0);
        assert_eq!(t.task.title, "Tamara");
        assert!(t.task.event_read_only);

        // edição local não sobe (o Google recusaria), mas não vira aviso
        // nem fica insistindo nas rodadas seguintes
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        m.update(
            t.task.id,
            TaskInput {
                tags: Some(vec!["familia".into()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let r = m.sync_calendar(req()).await.unwrap();
        assert_eq!((r.kept_local, r.pushed_updated), (1, 0), "{r:?}");
        assert!(r.warnings.is_empty(), "{r:?}");
        assert_eq!(fake.summary("T"), "Tamara");
        let r = m.sync_calendar(req()).await.unwrap();
        assert_eq!((r.unchanged, r.kept_local), (1, 0), "{r:?}");

        // apagar a tarefa deixa o aniversário na agenda, sem lápide
        m.delete(t.task.id).await.unwrap();
        assert!(fake.events.lock().contains_key("T"));
        assert!(m
            .deps
            .storage
            .list_task_tombstones("cal1")
            .await
            .unwrap()
            .is_empty());
        // e a próxima rodada o importa de novo (sem vínculo local, é novo)
        let r = m.sync_calendar(req()).await.unwrap();
        assert_eq!(r.pulled_created, 1, "{r:?}");
    }

    /// Conta os bipes pedidos ao servidor.
    #[derive(Default)]
    struct Beeper(AtomicUsize);
    #[async_trait]
    impl TaskNotifier for Beeper {
        async fn notify(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn soon_beep(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn service_beeps_while_a_task_is_about_to_start() {
        let beeper = Arc::new(Beeper::default());
        let mk = |cfg: TasksSettings| {
            let beeper = beeper.clone();
            async move {
                TasksModule::new(
                    TasksDeps {
                        storage: Storage::open_memory().await.unwrap(),
                        bus: EventBus::new(16),
                        notifier: beeper,
                        calendar: Arc::new(Fake::default()),
                    },
                    cfg,
                )
            }
        };
        let soon = (Utc::now() + Duration::minutes(10)).to_rfc3339();
        let input = || TaskInput {
            title: Some("Reunião".into()),
            start_at: Some(soon.clone()),
            ..Default::default()
        };

        // dentro do corte (30 min): bipa logo e repete no intervalo (1 s)
        let m = mk(TasksSettings {
            soon_beep_seconds: 1,
            ..Default::default()
        })
        .await;
        m.create(input(), "user").await.unwrap();
        m.start();
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        m.stop();
        assert!(
            beeper.0.load(Ordering::SeqCst) >= 2,
            "bipou {}",
            beeper.0.load(Ordering::SeqCst)
        );

        // fora do corte (5 min) ou com o som desligado (0 s): silêncio
        for cfg in [
            TasksSettings {
                soon_minutes: 5,
                soon_beep_seconds: 1,
                ..Default::default()
            },
            TasksSettings {
                soon_beep_seconds: 0,
                ..Default::default()
            },
        ] {
            beeper.0.store(0, Ordering::SeqCst);
            let m = mk(cfg).await;
            m.create(input(), "user").await.unwrap();
            m.start();
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            m.stop();
            assert_eq!(beeper.0.load(Ordering::SeqCst), 0);
        }
    }
}
