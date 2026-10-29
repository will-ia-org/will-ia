//! Ferramentas `calendar_*` (Google Calendar v3). Só agendas habilitadas na UI
//! são visíveis; `calendar_id` fora da lista retorna erro.

pub mod dates;

use crate::error::GoogleError;
use crate::GoogleModule;
use chrono::{DateTime, Utc};
use dates::{parse_when, parse_window, When};
use serde_json::{json, Value};
use std::sync::Arc;
use will_core::tools::{arg_opt_bool, arg_opt_str, arg_opt_u64, arg_str, truncate};
use will_core::{
    Module, ToolContext, ToolError, ToolOutput, ToolPolicy, ToolProvider, ToolSpec, WillTool,
};
use will_storage::google::GoogleCalendar;

const FAMILY: &str = "calendar";
const MAX_RESULTS: u64 = 250;
const DESCRIPTION_MAX: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    ListCalendars,
    ListEvents,
    GetEvent,
    FreeBusy,
    CreateEvent,
    UpdateEvent,
    DeleteEvent,
    Respond,
}

impl Kind {
    const ALL: [Kind; 8] = [
        Kind::ListCalendars,
        Kind::ListEvents,
        Kind::GetEvent,
        Kind::FreeBusy,
        Kind::CreateEvent,
        Kind::UpdateEvent,
        Kind::DeleteEvent,
        Kind::Respond,
    ];
    fn name(self) -> &'static str {
        match self {
            Kind::ListCalendars => "calendar_list_calendars",
            Kind::ListEvents => "calendar_list_events",
            Kind::GetEvent => "calendar_get_event",
            Kind::FreeBusy => "calendar_free_busy",
            Kind::CreateEvent => "calendar_create_event",
            Kind::UpdateEvent => "calendar_update_event",
            Kind::DeleteEvent => "calendar_delete_event",
            Kind::Respond => "calendar_respond",
        }
    }
}

pub struct CalendarProvider {
    module: Arc<GoogleModule>,
}

impl CalendarProvider {
    pub fn new(module: Arc<GoogleModule>) -> Self {
        Self { module }
    }
}

impl ToolProvider for CalendarProvider {
    fn family(&self) -> &'static str {
        FAMILY
    }
    fn tools(&self) -> Vec<Arc<dyn WillTool>> {
        Kind::ALL
            .iter()
            .map(|k| {
                Arc::new(CalendarTool {
                    kind: *k,
                    module: self.module.clone(),
                }) as Arc<dyn WillTool>
            })
            .collect()
    }
    fn available(&self) -> bool {
        self.module.config().calendar.enabled && self.module.has_ok_account()
    }
}

pub struct CalendarTool {
    kind: Kind,
    module: Arc<GoogleModule>,
}

fn window_props() -> Value {
    json!({
        "time_min": {"type": "string", "description": "Início da janela (ISO-8601; sem fuso = fuso padrão)"},
        "time_max": {"type": "string", "description": "Fim da janela (ISO-8601)"},
        "calendar_ids": {"type": "array", "items": {"type": "string"}, "description": "Agendas (padrão: todas habilitadas)"}
    })
}

fn event_fields() -> Value {
    json!({
        "summary": {"type": "string", "description": "Título"},
        "start": {"type": "string", "description": "Início: YYYY-MM-DDTHH:MM[:SS][±hh:mm]"},
        "end": {"type": "string", "description": "Fim (mesmo formato)"},
        "date": {"type": "string", "description": "Dia inteiro (YYYY-MM-DD) — alternativa a start/end"},
        "end_date": {"type": "string", "description": "Último dia (inclusive) para eventos de dia inteiro com vários dias"},
        "description": {"type": "string"},
        "location": {"type": "string"},
        "attendees": {"type": "array", "items": {"type": "string"}, "description": "E-mails dos participantes"},
        "reminders": {"description": "Objeto {useDefault, overrides:[{method,minutes}]} ou lista de overrides"},
        "recurrence": {"type": "array", "items": {"type": "string"}, "description": "Regras RRULE (ex.: RRULE:FREQ=WEEKLY)"},
        "conference": {"type": "boolean", "description": "Criar link do Google Meet"}
    })
}

#[async_trait::async_trait]
impl WillTool for CalendarTool {
    fn spec(&self) -> ToolSpec {
        let (description, input_schema, default_policy) = match self.kind {
            Kind::ListCalendars => (
                "Lista as agendas Google habilitadas (id, nome, fuso, papel, padrão para criação).",
                json!({"type": "object", "properties": {}, "additionalProperties": false}),
                ToolPolicy::Auto,
            ),
            Kind::ListEvents => {
                let mut props = window_props();
                props["query"] = json!({"type": "string", "description": "Busca textual (título, descrição, local)"});
                props["max_results"] =
                    json!({"type": "integer", "minimum": 1, "maximum": MAX_RESULTS, "default": 50});
                (
                    "Lista eventos das agendas habilitadas na janela informada, ordenados por início (instâncias de recorrência expandidas).",
                    json!({"type": "object", "properties": props, "required": ["time_min", "time_max"]}),
                    ToolPolicy::Auto,
                )
            }
            Kind::GetEvent => (
                "Obtém um evento completo.",
                json!({"type": "object", "properties": {
                    "calendar_id": {"type": "string"}, "event_id": {"type": "string"}
                }, "required": ["calendar_id", "event_id"]}),
                ToolPolicy::Auto,
            ),
            Kind::FreeBusy => (
                "Blocos ocupados por agenda na janela (use antes de criar um compromisso).",
                json!({"type": "object", "properties": window_props(), "required": ["time_min", "time_max"]}),
                ToolPolicy::Auto,
            ),
            Kind::CreateEvent => {
                let mut props = event_fields();
                props["calendar_id"] = json!({"type": "string", "description": "Agenda (padrão: agenda padrão configurada)"});
                (
                    "Cria um evento. Informe start+end ou date (dia inteiro). Conflitos de horário são devolvidos em `conflicts` sem bloquear.",
                    json!({"type": "object", "properties": props, "required": ["summary"]}),
                    ToolPolicy::Confirm,
                )
            }
            Kind::UpdateEvent => {
                let mut props = event_fields();
                props["calendar_id"] = json!({"type": "string"});
                props["event_id"] = json!({"type": "string"});
                (
                    "Atualiza campos de um evento existente (parcial).",
                    json!({"type": "object", "properties": props, "required": ["calendar_id", "event_id"]}),
                    ToolPolicy::Confirm,
                )
            }
            Kind::DeleteEvent => (
                "Exclui um evento.",
                json!({"type": "object", "properties": {
                    "calendar_id": {"type": "string"}, "event_id": {"type": "string"}
                }, "required": ["calendar_id", "event_id"]}),
                ToolPolicy::Confirm,
            ),
            Kind::Respond => (
                "Responde a um convite recebido: aceitar, recusar ou talvez (como participante). Só use com pedido explícito do usuário.",
                json!({"type": "object", "properties": {
                    "calendar_id": {"type": "string", "description": "Agenda onde o convite está (padrão: agenda padrão)"},
                    "event_id": {"type": "string"},
                    "response": {"type": "string", "enum": ["accepted", "declined", "tentative"], "description": "aceitar | recusar | talvez"},
                    "comment": {"type": "string", "description": "Mensagem ao organizador (opcional)"},
                    "notify": {"type": "boolean", "description": "Avisar o organizador (padrão: configuração 'enviar atualizações')"}
                }, "required": ["event_id", "response"]}),
                ToolPolicy::Confirm,
            ),
        };
        ToolSpec {
            name: self.kind.name().to_string(),
            description: description.to_string(),
            input_schema,
            requires: vec![Module::Google, Module::Calendar],
            default_policy,
            family: FAMILY.to_string(),
        }
    }

    fn approval_summary(&self, args: &Value) -> String {
        let cal_name = |id: Option<&str>| -> String {
            let cache = self.module.calendars_cache.read();
            match id {
                Some(id) => cache
                    .iter()
                    .find(|c| c.calendar_id == id)
                    .map(|c| c.summary.clone())
                    .unwrap_or_else(|| id.to_string()),
                None => cache
                    .iter()
                    .find(|c| c.default_for_create && c.enabled)
                    .map(|c| c.summary.clone())
                    .unwrap_or_else(|| "agenda padrão".into()),
            }
        };
        match self.kind {
            Kind::CreateEvent => {
                let summary = arg_opt_str(args, "summary").unwrap_or("(sem título)");
                let start = arg_opt_str(args, "start")
                    .or_else(|| arg_opt_str(args, "date"))
                    .and_then(|s| parse_when(s).ok())
                    .map(|w| w.display())
                    .unwrap_or_else(|| "data inválida".into());
                format!(
                    "Criar evento '{}' em {} ({})",
                    truncate(summary, 80),
                    start,
                    cal_name(arg_opt_str(args, "calendar_id"))
                )
            }
            Kind::UpdateEvent => {
                let fields: Vec<&str> = [
                    "summary",
                    "start",
                    "end",
                    "date",
                    "description",
                    "location",
                    "attendees",
                    "recurrence",
                ]
                .into_iter()
                .filter(|f| args.get(f).is_some())
                .collect();
                format!(
                    "Atualizar evento {} em {} (campos: {})",
                    arg_opt_str(args, "event_id").unwrap_or("?"),
                    cal_name(arg_opt_str(args, "calendar_id")),
                    if fields.is_empty() {
                        "nenhum".to_string()
                    } else {
                        fields.join(", ")
                    }
                )
            }
            Kind::DeleteEvent => format!(
                "Excluir evento {} de {}",
                arg_opt_str(args, "event_id").unwrap_or("?"),
                cal_name(arg_opt_str(args, "calendar_id"))
            ),
            Kind::Respond => format!(
                "{} o convite {} em {}",
                match arg_opt_str(args, "response") {
                    Some("accepted") => "Aceitar",
                    Some("declined") => "Recusar",
                    Some("tentative") => "Responder 'talvez' a",
                    _ => "Responder a",
                },
                arg_opt_str(args, "event_id").unwrap_or("?"),
                cal_name(arg_opt_str(args, "calendar_id"))
            ),
            _ => format!("{} {}", self.kind.name(), truncate(&args.to_string(), 200)),
        }
    }

    async fn call(&self, _ctx: ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let m = &self.module;
        if !m.config().calendar.enabled {
            return Err(ToolError::Unavailable("módulo Agenda desligado".into()));
        }
        match self.kind {
            Kind::ListCalendars => list_calendars(m).await,
            Kind::ListEvents => list_events(m, &args).await,
            Kind::GetEvent => get_event(m, &args).await,
            Kind::FreeBusy => free_busy(m, &args).await,
            Kind::CreateEvent => create_event(m, &args).await,
            Kind::UpdateEvent => update_event(m, &args).await,
            Kind::DeleteEvent => delete_event(m, &args).await,
            Kind::Respond => respond(m, &args).await,
        }
    }
}

// ----- helpers -----

/// Percent-encoding para segmentos de path (ids de agenda têm `@` e `#`).
pub(crate) fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn calendar_json(c: &GoogleCalendar) -> Value {
    json!({
        "id": c.calendar_id,
        "account": c.account_id,
        "summary": c.summary,
        "timezone": c.timezone,
        "access_role": c.access_role,
        "color": c.color,
        "default_for_create": c.default_for_create,
    })
}

fn is_writable(c: &GoogleCalendar) -> bool {
    matches!(
        c.access_role.as_deref(),
        Some("owner") | Some("writer") | None
    )
}

async fn enabled(m: &GoogleModule) -> Result<Vec<GoogleCalendar>, ToolError> {
    let list = m.enabled_calendars().await?;
    if list.is_empty() {
        return Err(ToolError::Unavailable(
            "nenhuma agenda habilitada (Configurações → Agenda → marque as agendas)".into(),
        ));
    }
    Ok(list)
}

async fn find_enabled(m: &GoogleModule, calendar_id: &str) -> Result<GoogleCalendar, ToolError> {
    enabled(m)
        .await?
        .into_iter()
        .find(|c| c.calendar_id == calendar_id)
        .ok_or_else(|| {
            ToolError::InvalidArgs(format!(
                "agenda '{calendar_id}' não habilitada ou inexistente; use calendar_list_calendars"
            ))
        })
}

fn arg_str_list(args: &Value, key: &str) -> Option<Vec<String>> {
    match args.get(key)? {
        Value::Array(a) => Some(
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect(),
        ),
        Value::String(s) => Some(
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect(),
        ),
        _ => None,
    }
}

/// Seleciona agendas pela lista pedida (todas devem estar habilitadas).
async fn select_calendars(
    m: &GoogleModule,
    args: &Value,
) -> Result<Vec<GoogleCalendar>, ToolError> {
    let all = enabled(m).await?;
    match arg_str_list(args, "calendar_ids")
        .or_else(|| arg_opt_str(args, "calendar_id").map(|s| vec![s.to_string()]))
    {
        Some(ids) if !ids.is_empty() => {
            let mut out = Vec::new();
            for id in ids {
                let c = all.iter().find(|c| c.calendar_id == id).ok_or_else(|| {
                    ToolError::InvalidArgs(format!(
                        "agenda '{id}' não habilitada ou inexistente; use calendar_list_calendars"
                    ))
                })?;
                out.push(c.clone());
            }
            Ok(out)
        }
        _ => Ok(all),
    }
}

fn when_out(v: Option<&Value>) -> (Option<String>, Option<When>) {
    match v.and_then(When::from_google) {
        Some(When::Date(d)) => (Some(d.format("%Y-%m-%d").to_string()), Some(When::Date(d))),
        Some(w) => (Some(w.to_rfc3339()), Some(w)),
        None => (None, None),
    }
}

/// Evento normalizado + chave de ordenação.
pub(crate) fn normalize_event(ev: &Value, cal: &GoogleCalendar) -> (Value, DateTime<Utc>) {
    let s = |k: &str| ev.get(k).and_then(|v| v.as_str()).map(|v| v.to_string());
    let (start, start_w) = when_out(ev.get("start"));
    let (end, _) = when_out(ev.get("end"));
    let all_day = start_w.as_ref().is_some_and(|w| w.is_all_day());
    let sort = start_w
        .map(|w| w.sort_key())
        .unwrap_or_else(|| DateTime::<Utc>::MAX_UTC);
    let attendees: Vec<Value> = ev
        .get("attendees")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .map(|p| {
                    json!({
                        "email": p.get("email"),
                        "response_status": p.get("responseStatus"),
                        "optional": p.get("optional").and_then(|v| v.as_bool()).unwrap_or(false),
                        "self": p.get("self").and_then(|v| v.as_bool()).unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let recurring = ev.get("recurringEventId").is_some() || ev.get("recurrence").is_some();
    let v = json!({
        "id": s("id"),
        "calendar_id": cal.calendar_id,
        "calendar": cal.summary,
        "account": cal.account_id,
        "summary": s("summary").unwrap_or_else(|| "(sem título)".into()),
        "start": start,
        "end": end,
        "all_day": all_day,
        "location": s("location"),
        "description": s("description").map(|d| truncate(&d, DESCRIPTION_MAX)),
        "attendees": attendees,
        "html_link": s("htmlLink"),
        "status": s("status"),
        "recurring": recurring,
        "recurring_event_id": s("recurringEventId"),
        "meet_link": s("hangoutLink"),
        "organizer": ev.get("organizer").and_then(|o| o.get("email")),
        "updated": s("updated"),
    });
    (v, sort)
}

async fn fetch_raw_events(
    m: &GoogleModule,
    cal: &GoogleCalendar,
    time_min: &str,
    time_max: &str,
    query: Option<&str>,
    max: usize,
    tz: Option<&str>,
) -> Result<Vec<Value>, GoogleError> {
    let acc = m.account_ready(&cal.account_id).await?;
    let mut q: Vec<(&str, String)> = vec![
        ("singleEvents", "true".into()),
        ("orderBy", "startTime".into()),
        ("timeMin", time_min.to_string()),
        ("timeMax", time_max.to_string()),
        (
            "maxResults",
            (max.clamp(1, MAX_RESULTS as usize)).to_string(),
        ),
    ];
    if let Some(s) = query {
        q.push(("q", s.to_string()));
    }
    if let Some(tz) = tz.filter(|t| !t.is_empty()) {
        q.push(("timeZone", tz.to_string()));
    }
    let path = format!("calendar/v3/calendars/{}/events", enc(&cal.calendar_id));
    m.client
        .get_all_pages(&acc, &path, &q, "items", Some(max))
        .await
}

// ----- tools -----

async fn list_calendars(m: &GoogleModule) -> Result<ToolOutput, ToolError> {
    let list = m.enabled_calendars().await?;
    let items: Vec<Value> = list.iter().map(calendar_json).collect();
    Ok(ToolOutput::json(
        &json!({ "calendars": items, "count": items.len() }),
    ))
}

async fn list_events(m: &GoogleModule, args: &Value) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().calendar;
    let (min, max, clamped) = parse_window(
        arg_str(args, "time_min")?,
        arg_str(args, "time_max")?,
        cfg.max_window_days,
    )
    .map_err(ToolError::InvalidArgs)?;
    let max_results = arg_opt_u64(args, "max_results")
        .unwrap_or(50)
        .clamp(1, MAX_RESULTS) as usize;
    let query = arg_opt_str(args, "query").or_else(|| arg_opt_str(args, "q"));
    let cals = select_calendars(m, args).await?;
    let (tmin, tmax) = (min.to_rfc3339(), max.to_rfc3339());
    let futs = cals.iter().map(|c| {
        let (tmin, tmax, tz) = (tmin.clone(), tmax.clone(), cfg.timezone.clone());
        async move {
            let raw =
                fetch_raw_events(m, c, &tmin, &tmax, query, max_results, tz.as_deref()).await?;
            Ok::<_, GoogleError>(
                raw.iter()
                    .map(|e| normalize_event(e, c))
                    .collect::<Vec<_>>(),
            )
        }
    });
    let results = futures::future::join_all(futs).await;
    let mut events = Vec::new();
    let mut errors = Vec::new();
    for (c, r) in cals.iter().zip(results) {
        match r {
            Ok(v) => events.extend(v),
            Err(e) => errors.push(format!("{}: {}", c.summary, ToolError::from(e))),
        }
    }
    if events.is_empty() && !errors.is_empty() {
        return Err(ToolError::Failed(errors.join("; ")));
    }
    events.sort_by_key(|(_, k)| *k);
    events.truncate(max_results);
    let items: Vec<Value> = events.into_iter().map(|(v, _)| v).collect();
    Ok(ToolOutput::json(&json!({
        "events": items,
        "count": items.len(),
        "window": { "time_min": tmin, "time_max": tmax, "clamped_to_days": if clamped { Some(cfg.max_window_days) } else { None } },
        "calendars": cals.iter().map(|c| c.calendar_id.clone()).collect::<Vec<_>>(),
        "warnings": errors,
    })))
}

async fn get_event(m: &GoogleModule, args: &Value) -> Result<ToolOutput, ToolError> {
    let cal = find_enabled(m, arg_str(args, "calendar_id")?).await?;
    let event_id = arg_str(args, "event_id")?;
    let acc = m.account_ready(&cal.account_id).await?;
    let path = format!(
        "calendar/v3/calendars/{}/events/{}",
        enc(&cal.calendar_id),
        enc(event_id)
    );
    let ev = m.client.get(&acc, &path, &[]).await?;
    let (mut out, _) = normalize_event(&ev, &cal);
    out["description"] = ev.get("description").cloned().unwrap_or(Value::Null);
    out["recurrence"] = ev.get("recurrence").cloned().unwrap_or(Value::Null);
    out["reminders"] = ev.get("reminders").cloned().unwrap_or(Value::Null);
    out["creator"] = ev
        .get("creator")
        .and_then(|c| c.get("email"))
        .cloned()
        .unwrap_or(Value::Null);
    out["conference_data"] = ev.get("conferenceData").cloned().unwrap_or(Value::Null);
    out["transparency"] = ev.get("transparency").cloned().unwrap_or(Value::Null);
    out["visibility"] = ev.get("visibility").cloned().unwrap_or(Value::Null);
    Ok(ToolOutput::json(&out))
}

async fn free_busy(m: &GoogleModule, args: &Value) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().calendar;
    let (min, max, clamped) = parse_window(
        arg_str(args, "time_min")?,
        arg_str(args, "time_max")?,
        cfg.max_window_days,
    )
    .map_err(ToolError::InvalidArgs)?;
    let cals = select_calendars(m, args).await?;
    let (tmin, tmax) = (min.to_rfc3339(), max.to_rfc3339());
    // Agrupa por conta (freeBusy é por credencial).
    let mut by_account: Vec<(String, Vec<&GoogleCalendar>)> = Vec::new();
    for c in &cals {
        match by_account.iter_mut().find(|(a, _)| *a == c.account_id) {
            Some((_, v)) => v.push(c),
            None => by_account.push((c.account_id.clone(), vec![c])),
        }
    }
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for (account_id, group) in by_account {
        let acc = match m.account_ready(&account_id).await {
            Ok(a) => a,
            Err(e) => {
                errors.push(ToolError::from(e).to_string());
                continue;
            }
        };
        let mut body = json!({
            "timeMin": tmin, "timeMax": tmax,
            "items": group.iter().map(|c| json!({"id": c.calendar_id})).collect::<Vec<_>>(),
        });
        if let Some(tz) = cfg.timezone.as_deref().filter(|t| !t.is_empty()) {
            body["timeZone"] = json!(tz);
        }
        match m
            .client
            .post(&acc, "calendar/v3/freeBusy", &[], &body)
            .await
        {
            Ok(resp) => {
                for c in group {
                    let entry = resp.get("calendars").and_then(|cs| cs.get(&c.calendar_id));
                    let busy: Vec<Value> = entry
                        .and_then(|e| e.get("busy"))
                        .and_then(|b| b.as_array())
                        .map(|b| {
                            b.iter()
                                .map(|x| json!({"start": x.get("start"), "end": x.get("end")}))
                                .collect()
                        })
                        .unwrap_or_default();
                    let errs = entry.and_then(|e| e.get("errors")).cloned();
                    out.push(json!({
                        "calendar_id": c.calendar_id, "calendar": c.summary, "account": c.account_id,
                        "busy": busy, "errors": errs,
                    }));
                }
            }
            Err(e) => errors.push(ToolError::from(e).to_string()),
        }
    }
    if out.is_empty() && !errors.is_empty() {
        return Err(ToolError::Failed(errors.join("; ")));
    }
    Ok(ToolOutput::json(&json!({
        "time_min": tmin, "time_max": tmax,
        "clamped_to_days": if clamped { Some(cfg.max_window_days) } else { None },
        "calendars": out, "warnings": errors,
    })))
}

/// Interpreta start/end/date dos argumentos. `required` exige ao menos um par.
fn parse_times(args: &Value, required: bool) -> Result<Option<(When, When)>, ToolError> {
    if let Some(date) = arg_opt_str(args, "date") {
        let start = parse_when(date).map_err(ToolError::InvalidArgs)?;
        let When::Date(d) = start else {
            return Err(ToolError::InvalidArgs(
                "`date` deve ser YYYY-MM-DD (use start/end para horário)".into(),
            ));
        };
        let end_d = match arg_opt_str(args, "end_date") {
            Some(e) => match parse_when(e).map_err(ToolError::InvalidArgs)? {
                When::Date(ed) if ed >= d => ed,
                _ => {
                    return Err(ToolError::InvalidArgs(
                        "`end_date` deve ser YYYY-MM-DD e >= date".into(),
                    ))
                }
            },
            None => d,
        };
        // Google: fim exclusivo em eventos de dia inteiro.
        return Ok(Some((
            When::Date(d),
            When::Date(end_d + chrono::Duration::days(1)),
        )));
    }
    match (arg_opt_str(args, "start"), arg_opt_str(args, "end")) {
        (Some(s), e) => {
            let start = parse_when(s).map_err(|e| ToolError::InvalidArgs(format!("start: {e}")))?;
            let end = match e {
                Some(e) => {
                    parse_when(e).map_err(|e| ToolError::InvalidArgs(format!("end: {e}")))?
                }
                None => match &start {
                    When::Date(d) => When::Date(*d + chrono::Duration::days(1)),
                    When::DateTime {
                        dt,
                        explicit_offset,
                    } => When::DateTime {
                        dt: *dt + chrono::Duration::hours(1),
                        explicit_offset: *explicit_offset,
                    },
                },
            };
            if start.is_all_day() != end.is_all_day() {
                return Err(ToolError::InvalidArgs(
                    "start e end devem ser ambos data (dia inteiro) ou ambos data+hora".into(),
                ));
            }
            if end.sort_key() <= start.sort_key() {
                return Err(ToolError::InvalidArgs(
                    "end deve ser posterior a start".into(),
                ));
            }
            Ok(Some((start, end)))
        }
        (None, Some(_)) => Err(ToolError::InvalidArgs("`end` sem `start`".into())),
        (None, None) if required => Err(ToolError::InvalidArgs("informe start+end ou date".into())),
        (None, None) => Ok(None),
    }
}

/// Campos comuns de criação/edição → corpo da API.
fn event_body(args: &Value, times: Option<&(When, When)>, tz: Option<&str>) -> Value {
    let mut body = serde_json::Map::new();
    if let Some(s) = arg_opt_str(args, "summary") {
        body.insert("summary".into(), json!(s));
    }
    if let Some((start, end)) = times {
        body.insert("start".into(), start.to_google(tz));
        body.insert("end".into(), end.to_google(tz));
    }
    if let Some(v) = args.get("description").filter(|v| v.is_string()) {
        body.insert("description".into(), v.clone());
    }
    if let Some(v) = args.get("location").filter(|v| v.is_string()) {
        body.insert("location".into(), v.clone());
    }
    if let Some(list) = args.get("attendees").and_then(|a| a.as_array()) {
        let att: Vec<Value> = list
            .iter()
            .filter_map(|a| match a {
                Value::String(s) if s.contains('@') => Some(json!({"email": s.trim()})),
                Value::Object(o) if o.get("email").is_some() => Some(a.clone()),
                _ => None,
            })
            .collect();
        body.insert("attendees".into(), Value::Array(att));
    }
    match args.get("reminders") {
        Some(Value::Object(o)) => {
            body.insert("reminders".into(), Value::Object(o.clone()));
        }
        Some(Value::Array(a)) => {
            body.insert(
                "reminders".into(),
                json!({"useDefault": false, "overrides": a}),
            );
        }
        _ => {}
    }
    match args.get("recurrence") {
        Some(Value::Array(a)) => {
            body.insert("recurrence".into(), Value::Array(a.clone()));
        }
        Some(Value::String(s)) if !s.is_empty() => {
            body.insert("recurrence".into(), json!([s]));
        }
        _ => {}
    }
    if arg_opt_bool(args, "conference").unwrap_or(false) {
        body.insert(
            "conferenceData".into(),
            json!({"createRequest": {"requestId": uuid::Uuid::new_v4().to_string(), "conferenceSolutionKey": {"type": "hangoutsMeet"}}}),
        );
    }
    Value::Object(body)
}

fn send_updates_param(cfg_send: bool, has_attendees: bool) -> (&'static str, String) {
    (
        "sendUpdates",
        if cfg_send && has_attendees {
            "all"
        } else {
            "none"
        }
        .to_string(),
    )
}

async fn default_calendar(m: &GoogleModule) -> Result<GoogleCalendar, ToolError> {
    let list = enabled(m).await?;
    if let Some(c) = list.iter().find(|c| c.default_for_create) {
        return Ok(c.clone());
    }
    let writable: Vec<&GoogleCalendar> = list.iter().filter(|c| is_writable(c)).collect();
    if writable.len() == 1 {
        return Ok(writable[0].clone());
    }
    Err(ToolError::InvalidArgs(
        "nenhuma agenda padrão configurada; informe calendar_id (veja calendar_list_calendars)"
            .into(),
    ))
}

async fn create_event(m: &GoogleModule, args: &Value) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().calendar;
    if !cfg.allow_write {
        return Err(ToolError::Unavailable(
            "criação de eventos desligada (Configurações → Agenda → Permitir criar/editar)".into(),
        ));
    }
    let cal = match arg_opt_str(args, "calendar_id") {
        Some(id) => find_enabled(m, id).await?,
        None => default_calendar(m).await?,
    };
    if !is_writable(&cal) {
        return Err(ToolError::Forbidden(format!(
            "agenda '{}' é somente leitura",
            cal.summary
        )));
    }
    let summary = arg_str(args, "summary")?;
    let (start, end) = parse_times(args, true)?
        .ok_or_else(|| ToolError::InvalidArgs("informe start+end ou date".into()))?;
    let acc = m.account_ready(&cal.account_id).await?;

    // Conflitos na janela (sem bloquear).
    let conflicts: Vec<Value> = match fetch_raw_events(m, &cal, &start.to_rfc3339(), &end.to_rfc3339(), None, 50, cfg.timezone.as_deref()).await {
        Ok(raw) => raw
            .iter()
            .filter(|e| e.get("transparency").and_then(|t| t.as_str()) != Some("transparent"))
            .filter(|e| e.get("status").and_then(|t| t.as_str()) != Some("cancelled"))
            .map(|e| {
                let (n, _) = normalize_event(e, &cal);
                json!({"id": n["id"], "summary": n["summary"], "start": n["start"], "end": n["end"], "all_day": n["all_day"]})
            })
            .collect(),
        Err(e) => {
            tracing::warn!(module = "google", error = %e, "detecção de conflitos falhou");
            Vec::new()
        }
    };

    let mut body = event_body(
        args,
        Some(&(start.clone(), end.clone())),
        cfg.timezone.as_deref(),
    );
    body["summary"] = json!(summary);
    let has_attendees = body
        .get("attendees")
        .and_then(|a| a.as_array())
        .is_some_and(|a| !a.is_empty());
    let mut q = vec![send_updates_param(cfg.send_updates, has_attendees)];
    if body.get("conferenceData").is_some() {
        q.push(("conferenceDataVersion", "1".into()));
    }
    let path = format!("calendar/v3/calendars/{}/events", enc(&cal.calendar_id));
    let created = m.client.post(&acc, &path, &q, &body).await?;
    let (mut out, _) = normalize_event(&created, &cal);
    out["conflicts"] = Value::Array(conflicts);
    out["send_updates"] = json!(q[0].1);
    Ok(ToolOutput::json(&out))
}

async fn update_event(m: &GoogleModule, args: &Value) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().calendar;
    if !cfg.allow_write {
        return Err(ToolError::Unavailable(
            "edição de eventos desligada (Configurações → Agenda → Permitir criar/editar)".into(),
        ));
    }
    let cal = find_enabled(m, arg_str(args, "calendar_id")?).await?;
    let event_id = arg_str(args, "event_id")?;
    if !is_writable(&cal) {
        return Err(ToolError::Forbidden(format!(
            "agenda '{}' é somente leitura",
            cal.summary
        )));
    }
    let times = parse_times(args, false)?;
    let body = event_body(args, times.as_ref(), cfg.timezone.as_deref());
    if body.as_object().is_none_or(|o| o.is_empty()) {
        return Err(ToolError::InvalidArgs("nenhum campo para atualizar".into()));
    }
    let acc = m.account_ready(&cal.account_id).await?;
    let has_attendees = body.get("attendees").is_some();
    let mut q = vec![send_updates_param(
        cfg.send_updates,
        has_attendees || cfg.send_updates,
    )];
    if body.get("conferenceData").is_some() {
        q.push(("conferenceDataVersion", "1".into()));
    }
    let path = format!(
        "calendar/v3/calendars/{}/events/{}",
        enc(&cal.calendar_id),
        enc(event_id)
    );
    let updated = m.client.patch(&acc, &path, &q, &body).await?;
    let (out, _) = normalize_event(&updated, &cal);
    Ok(ToolOutput::json(&out))
}

async fn delete_event(m: &GoogleModule, args: &Value) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().calendar;
    if !cfg.allow_delete {
        return Err(ToolError::Unavailable(
            "exclusão de eventos desligada (Configurações → Agenda → Permitir excluir)".into(),
        ));
    }
    let cal = find_enabled(m, arg_str(args, "calendar_id")?).await?;
    let event_id = arg_str(args, "event_id")?;
    let acc = m.account_ready(&cal.account_id).await?;
    let q = vec![send_updates_param(cfg.send_updates, true)];
    let path = format!(
        "calendar/v3/calendars/{}/events/{}",
        enc(&cal.calendar_id),
        enc(event_id)
    );
    m.client.delete(&acc, &path, &q).await?;
    Ok(ToolOutput::json(
        &json!({"ok": true, "event_id": event_id, "calendar_id": cal.calendar_id}),
    ))
}

/// Aplica `response` (e `comment`) ao participante `me` na lista de `attendees`
/// do evento; `Ok(None)` se eu não estou entre os convidados.
fn respond_attendees(
    attendees: &[Value],
    me: &str,
    response: &str,
    comment: Option<&str>,
) -> Option<Vec<Value>> {
    let me_l = me.trim().to_ascii_lowercase();
    let mut found = false;
    let out: Vec<Value> = attendees
        .iter()
        .map(|a| {
            let is_me = a.get("self").and_then(|v| v.as_bool()).unwrap_or(false)
                || a.get("email")
                    .and_then(|e| e.as_str())
                    .is_some_and(|e| e.trim().to_ascii_lowercase() == me_l);
            if !is_me {
                return a.clone();
            }
            found = true;
            let mut b = a.clone();
            b["responseStatus"] = json!(response);
            match comment.map(str::trim).filter(|c| !c.is_empty()) {
                Some(c) => b["comment"] = json!(c),
                None => {
                    if let Some(o) = b.as_object_mut() {
                        o.remove("comment");
                    }
                }
            }
            b
        })
        .collect();
    found.then_some(out)
}

async fn respond(m: &GoogleModule, args: &Value) -> Result<ToolOutput, ToolError> {
    let cfg = m.config().calendar;
    if !cfg.allow_write {
        return Err(ToolError::Unavailable(
            "edição de eventos desligada (Configurações → Agenda → Permitir criar/editar)".into(),
        ));
    }
    let response = arg_str(args, "response")?;
    if !["accepted", "declined", "tentative"].contains(&response) {
        return Err(ToolError::InvalidArgs(
            "response deve ser accepted, declined ou tentative".into(),
        ));
    }
    let event_id = arg_str(args, "event_id")?;
    let cal = match arg_opt_str(args, "calendar_id") {
        Some(id) => find_enabled(m, id).await?,
        None => default_calendar(m).await?,
    };
    let acc = m.account_ready(&cal.account_id).await?;
    let path = format!(
        "calendar/v3/calendars/{}/events/{}",
        enc(&cal.calendar_id),
        enc(event_id)
    );
    let ev = m.client.get(&acc, &path, &[]).await?;
    let attendees = ev
        .get("attendees")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let organizer_is_me = ev
        .get("organizer")
        .and_then(|o| o.get("self"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let Some(updated) = respond_attendees(
        &attendees,
        &acc.email,
        response,
        arg_opt_str(args, "comment"),
    ) else {
        return Err(ToolError::Forbidden(if organizer_is_me {
            "você é o organizador deste evento, não um convidado; use calendar_update_event/calendar_delete_event".into()
        } else {
            format!(
                "{} não está entre os convidados do evento {}",
                acc.email, event_id
            )
        }));
    };
    let notify = arg_opt_bool(args, "notify").unwrap_or(cfg.send_updates);
    let q = vec![(
        "sendUpdates",
        if notify {
            "all".to_string()
        } else {
            "none".to_string()
        },
    )];
    let patched = m
        .client
        .patch(&acc, &path, &q, &json!({"attendees": updated}))
        .await?;
    let (mut out, _) = normalize_event(&patched, &cal);
    out["my_response"] = json!(response);
    out["notified_organizer"] = json!(notify);
    Ok(ToolOutput::json(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respond_attendees_marks_only_me() {
        let att = vec![
            json!({"email": "org@x.com", "organizer": true, "responseStatus": "accepted"}),
            json!({"email": "ME@x.com", "responseStatus": "needsAction", "comment": "velho"}),
        ];
        let out = respond_attendees(&att, "me@x.com", "declined", Some("não posso")).unwrap();
        assert_eq!(out[0]["responseStatus"], "accepted");
        assert_eq!(out[1]["responseStatus"], "declined");
        assert_eq!(out[1]["comment"], "não posso");
        let out = respond_attendees(&att, "me@x.com", "accepted", None).unwrap();
        assert!(out[1].get("comment").is_none());
        assert!(respond_attendees(&att, "outro@x.com", "accepted", None).is_none());
        let by_self =
            vec![json!({"email": "x@y.com", "self": true, "responseStatus": "needsAction"})];
        assert_eq!(
            respond_attendees(&by_self, "me@x.com", "tentative", None).unwrap()[0]
                ["responseStatus"],
            "tentative"
        );
    }

    fn cal() -> GoogleCalendar {
        GoogleCalendar {
            account_id: "acc".into(),
            calendar_id: "primary@x.com".into(),
            summary: "Pessoal".into(),
            timezone: Some("America/Sao_Paulo".into()),
            access_role: Some("owner".into()),
            color: None,
            enabled: true,
            default_for_create: true,
        }
    }

    #[test]
    fn normalizes_timed_and_all_day_events() {
        let ev = json!({
            "id": "e1", "summary": "Reunião", "htmlLink": "https://cal/e1", "status": "confirmed",
            "start": {"dateTime": "2025-03-10T10:00:00-03:00"}, "end": {"dateTime": "2025-03-10T11:00:00-03:00"},
            "attendees": [{"email": "a@x.com", "responseStatus": "accepted"}],
            "recurringEventId": "r1", "description": "x".repeat(5000)
        });
        let (n, key) = normalize_event(&ev, &cal());
        assert_eq!(n["calendar"], "Pessoal");
        assert_eq!(n["all_day"], false);
        assert_eq!(n["recurring"], true);
        assert_eq!(n["start"], "2025-03-10T10:00:00-03:00");
        assert_eq!(n["attendees"][0]["response_status"], "accepted");
        assert!(n["description"].as_str().unwrap().chars().count() <= DESCRIPTION_MAX);
        assert_eq!(
            key,
            "2025-03-10T13:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );

        let all =
            json!({"id": "e2", "start": {"date": "2025-03-11"}, "end": {"date": "2025-03-12"}});
        let (n2, _) = normalize_event(&all, &cal());
        assert_eq!(n2["all_day"], true);
        assert_eq!(n2["start"], "2025-03-11");
        assert_eq!(n2["summary"], "(sem título)");
    }

    #[test]
    fn parse_times_handles_all_day_and_defaults() {
        let (s, e) = parse_times(&json!({"date": "2025-03-10"}), true)
            .unwrap()
            .unwrap();
        assert_eq!(
            s,
            When::Date(chrono::NaiveDate::from_ymd_opt(2025, 3, 10).unwrap())
        );
        assert_eq!(
            e,
            When::Date(chrono::NaiveDate::from_ymd_opt(2025, 3, 11).unwrap())
        );
        let (s, e) = parse_times(&json!({"start": "2025-03-10T10:00-03:00"}), true)
            .unwrap()
            .unwrap();
        assert_eq!(e.sort_key() - s.sort_key(), chrono::Duration::hours(1));
        assert!(parse_times(
            &json!({"start": "2025-03-10T10:00", "end": "2025-03-10"}),
            true
        )
        .is_err());
        assert!(parse_times(
            &json!({"start": "2025-03-10T10:00", "end": "2025-03-10T09:00"}),
            true
        )
        .is_err());
        assert!(parse_times(&json!({}), true).is_err());
        assert!(parse_times(&json!({}), false).unwrap().is_none());
    }

    #[test]
    fn event_body_and_encoding() {
        let body = event_body(
            &json!({"summary": "x", "attendees": ["a@x.com", {"email": "b@x.com", "optional": true}, "inválido"], "reminders": [{"method": "popup", "minutes": 10}], "recurrence": "RRULE:FREQ=DAILY", "conference": true}),
            None,
            None,
        );
        assert_eq!(body["attendees"].as_array().unwrap().len(), 2);
        assert_eq!(body["reminders"]["useDefault"], false);
        assert_eq!(body["recurrence"][0], "RRULE:FREQ=DAILY");
        assert!(body["conferenceData"]["createRequest"]["requestId"].is_string());
        assert_eq!(enc("a#b@c.com"), "a%23b%40c.com");
        assert_eq!(send_updates_param(true, false).1, "none");
        assert_eq!(send_updates_param(true, true).1, "all");
    }
}
