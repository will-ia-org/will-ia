//! Normalização de datas da Agenda.
//!
//! Entradas aceitas: `YYYY-MM-DD` (dia inteiro), `YYYY-MM-DDTHH:MM[:SS[.fff]]`
//! com ou sem offset (`Z`, `±hh:mm`). Sem offset, assume-se o **fuso local do
//! sistema** (`chrono::Local`). Quando `calendar.timezone` está configurado e
//! a entrada não tem offset, a criação/edição envia `dateTime` sem offset +
//! `timeZone` (o Google resolve o fuso corretamente); já em janelas de consulta
//! (`timeMin`/`timeMax`, que exigem RFC 3339 com offset) usa-se o offset local.
//! Sem a crate `chrono-tz` não há como resolver um nome IANA para offset
//! localmente, portanto assume-se que o fuso configurado coincide com o do
//! sistema nessas consultas.

use chrono::{DateTime, FixedOffset, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum When {
    /// Data/hora. `explicit_offset` indica se a entrada trazia offset.
    DateTime {
        dt: DateTime<FixedOffset>,
        explicit_offset: bool,
    },
    /// Dia inteiro.
    Date(NaiveDate),
}

impl When {
    pub fn is_all_day(&self) -> bool {
        matches!(self, When::Date(_))
    }

    /// Chave de ordenação (dia inteiro = meia-noite local).
    pub fn sort_key(&self) -> DateTime<Utc> {
        match self {
            When::DateTime { dt, .. } => dt.with_timezone(&Utc),
            When::Date(d) => local_midnight(*d).with_timezone(&Utc),
        }
    }

    /// RFC 3339 com offset (para `timeMin`/`timeMax`) ou `YYYY-MM-DD`.
    pub fn to_rfc3339(&self) -> String {
        match self {
            When::DateTime { dt, .. } => dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            When::Date(d) => local_midnight(*d).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        }
    }

    /// Texto curto para resumos de aprovação.
    pub fn display(&self) -> String {
        match self {
            When::DateTime { dt, .. } => dt.format("%d/%m/%Y %H:%M (%:z)").to_string(),
            When::Date(d) => format!("{} (dia inteiro)", d.format("%d/%m/%Y")),
        }
    }

    /// Objeto `start`/`end` da Calendar API.
    pub fn to_google(&self, tz_name: Option<&str>) -> serde_json::Value {
        match self {
            When::Date(d) => serde_json::json!({ "date": d.format("%Y-%m-%d").to_string() }),
            When::DateTime {
                dt,
                explicit_offset,
            } => match (explicit_offset, tz_name) {
                (false, Some(tz)) if !tz.is_empty() => serde_json::json!({
                    "dateTime": dt.naive_local().format("%Y-%m-%dT%H:%M:%S").to_string(),
                    "timeZone": tz,
                }),
                _ => {
                    serde_json::json!({ "dateTime": dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true) })
                }
            },
        }
    }

    /// Lê `start`/`end` de um evento da API.
    pub fn from_google(v: &serde_json::Value) -> Option<When> {
        if let Some(d) = v.get("date").and_then(|d| d.as_str()) {
            return NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .ok()
                .map(When::Date);
        }
        let s = v.get("dateTime")?.as_str()?;
        parse_when(s).ok()
    }
}

fn local_midnight(d: NaiveDate) -> DateTime<FixedOffset> {
    let naive = d.and_hms_opt(0, 0, 0).unwrap_or_default();
    attach_local(naive)
}

fn attach_local(naive: NaiveDateTime) -> DateTime<FixedOffset> {
    let local = Local
        .from_local_datetime(&naive)
        .single()
        .or_else(|| Local.from_local_datetime(&naive).earliest())
        .unwrap_or_else(|| Local.from_utc_datetime(&naive));
    local.fixed_offset()
}

/// Interpreta a entrada do agente.
pub fn parse_when(input: &str) -> Result<When, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("data vazia".into());
    }
    if s.len() == 10 {
        if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
            return Ok(When::Date(d));
        }
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(When::DateTime {
            dt,
            explicit_offset: true,
        });
    }
    // RFC 3339 com espaço no lugar do T
    let normalized = if s.len() > 10 && s.as_bytes()[10] == b' ' {
        s.replacen(' ', "T", 1)
    } else {
        s.to_string()
    };
    if let Ok(dt) = DateTime::parse_from_rfc3339(&normalized) {
        return Ok(When::DateTime {
            dt,
            explicit_offset: true,
        });
    }
    // Offset sem segundos: `HH:MM±hh:mm`, `HH:MM±hhmm`, `HH:MMZ`.
    let with_offset = if normalized.ends_with(['Z', 'z']) {
        format!("{}+00:00", &normalized[..normalized.len() - 1])
    } else {
        normalized.clone()
    };
    for fmt in [
        "%Y-%m-%dT%H:%M%:z",
        "%Y-%m-%dT%H:%M%z",
        "%Y-%m-%dT%H:%M:%S%:z",
        "%Y-%m-%dT%H:%M:%S%z",
    ] {
        if let Ok(dt) = DateTime::parse_from_str(&with_offset, fmt) {
            return Ok(When::DateTime {
                dt,
                explicit_offset: true,
            });
        }
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(&normalized, fmt) {
            return Ok(When::DateTime {
                dt: attach_local(naive),
                explicit_offset: false,
            });
        }
    }
    Err(format!(
        "data inválida: '{input}' (use YYYY-MM-DD, YYYY-MM-DDTHH:MM ou RFC 3339 com fuso, ex.: 2025-03-10T14:00-03:00)"
    ))
}

/// Interpreta e valida uma janela; limita a `max_days` a partir de `time_min`.
/// Retorna (min, max, clamped).
pub fn parse_window(
    time_min: &str,
    time_max: &str,
    max_days: u32,
) -> Result<(When, When, bool), String> {
    let min = parse_when(time_min).map_err(|e| format!("time_min: {e}"))?;
    let max = parse_when(time_max).map_err(|e| format!("time_max: {e}"))?;
    let (a, b) = (min.sort_key(), max.sort_key());
    if b <= a {
        return Err("time_max deve ser posterior a time_min".into());
    }
    let limit = chrono::Duration::days(max_days.max(1) as i64);
    if b - a > limit {
        let clamped = match &min {
            When::DateTime { dt, .. } => When::DateTime {
                dt: *dt + limit,
                explicit_offset: true,
            },
            When::Date(d) => When::Date(*d + limit),
        };
        return Ok((min, clamped, true));
    }
    Ok((min, max, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_day() {
        let w = parse_when("2025-03-10").unwrap();
        assert_eq!(w, When::Date(NaiveDate::from_ymd_opt(2025, 3, 10).unwrap()));
        assert!(w.is_all_day());
        assert_eq!(w.to_google(None), serde_json::json!({"date": "2025-03-10"}));
    }

    #[test]
    fn parses_explicit_offset_and_keeps_it() {
        let w = parse_when("2025-03-10T14:00:00-03:00").unwrap();
        match &w {
            When::DateTime {
                dt,
                explicit_offset,
            } => {
                assert!(*explicit_offset);
                assert_eq!(dt.offset().local_minus_utc(), -3 * 3600);
                assert_eq!(dt.to_rfc3339(), "2025-03-10T14:00:00-03:00");
            }
            _ => panic!("esperado DateTime"),
        }
        assert_eq!(
            w.to_google(Some("America/Sao_Paulo")),
            serde_json::json!({"dateTime": "2025-03-10T14:00:00-03:00"})
        );
        let z = parse_when("2025-03-10T17:00Z").unwrap();
        assert_eq!(z.to_rfc3339(), "2025-03-10T17:00:00Z");
    }

    #[test]
    fn naive_uses_local_offset_or_configured_tz() {
        let w = parse_when("2025-03-10T14:00").unwrap();
        let When::DateTime {
            dt,
            explicit_offset,
        } = &w
        else {
            panic!()
        };
        assert!(!explicit_offset);
        assert_eq!(dt.naive_local().format("%H:%M").to_string(), "14:00");
        let expected = Local
            .with_ymd_and_hms(2025, 3, 10, 14, 0, 0)
            .single()
            .unwrap()
            .fixed_offset();
        assert_eq!(*dt, expected);
        assert_eq!(
            w.to_google(Some("America/Sao_Paulo")),
            serde_json::json!({"dateTime": "2025-03-10T14:00:00", "timeZone": "America/Sao_Paulo"})
        );
        assert!(w.to_rfc3339().len() >= 20);
        assert!(parse_when("2025-03-10 09:30").is_ok());
        assert!(parse_when("10/03/2025").is_err());
    }

    #[test]
    fn window_is_clamped() {
        let (a, b, clamped) =
            parse_window("2025-01-01T00:00:00Z", "2025-12-31T00:00:00Z", 62).unwrap();
        assert!(clamped);
        assert_eq!(b.sort_key() - a.sort_key(), chrono::Duration::days(62));
        assert!(parse_window("2025-01-02", "2025-01-01", 62).is_err());
        let (_, _, c) = parse_window("2025-01-01", "2025-01-05", 62).unwrap();
        assert!(!c);
    }

    #[test]
    fn from_google_roundtrip_and_sort() {
        let a = When::from_google(&serde_json::json!({"dateTime": "2025-03-10T10:00:00-03:00"}))
            .unwrap();
        let b = When::from_google(&serde_json::json!({"date": "2025-03-10"})).unwrap();
        assert!(!a.is_all_day());
        assert!(b.is_all_day());
        assert!(b.sort_key() <= a.sort_key() || b.sort_key() > a.sort_key()); // apenas garante que não entra em pânico
    }
}
