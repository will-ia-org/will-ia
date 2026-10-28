//! Expressões cron e fusos: validação, próxima ocorrência e descrição.

use chrono::{DateTime, Local, TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;
use std::str::FromStr;

/// Fuso de avaliação: nome IANA ou o fuso do sistema.
#[derive(Debug, Clone, Copy)]
pub enum Zone {
    Named(Tz),
    Local,
}

impl Zone {
    /// `None`/vazio = fuso do sistema; senão precisa ser um nome IANA válido.
    pub fn parse(name: Option<&str>) -> Result<Zone, String> {
        match name.map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(Zone::Local),
            Some(n) => n.parse::<Tz>().map(Zone::Named).map_err(|_| {
                format!("fuso horário desconhecido: {n} (use um nome IANA, ex.: America/Sao_Paulo)")
            }),
        }
    }
    pub fn name(&self) -> String {
        match self {
            Zone::Named(tz) => tz.name().to_string(),
            Zone::Local => {
                let off = Local::now().format("%:z").to_string();
                format!("fuso do sistema (UTC{off})")
            }
        }
    }
    /// Formata um instante nesse fuso, para cabeçalhos e listas.
    pub fn format(&self, at: DateTime<Utc>) -> String {
        match self {
            Zone::Named(tz) => at.with_timezone(tz).format("%d/%m/%Y %H:%M %Z").to_string(),
            Zone::Local => at
                .with_timezone(&Local)
                .format("%d/%m/%Y %H:%M %Z")
                .to_string(),
        }
    }
}

/// Valida a expressão (5 campos; segundos opcionais como 6º campo à frente).
pub fn parse_cron(expr: &str) -> Result<Cron, String> {
    let expr = expr.trim();
    if expr.is_empty() {
        return Err("expressão cron vazia".into());
    }
    Cron::from_str(expr).map_err(|e| format!("expressão cron inválida \"{expr}\": {e}"))
}

/// Próxima ocorrência estritamente depois de `after`, no fuso dado.
pub fn next_after(cron: &Cron, zone: Zone, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match zone {
        Zone::Named(tz) => cron
            .find_next_occurrence(&after.with_timezone(&tz), false)
            .ok()
            .map(|d| d.with_timezone(&Utc)),
        Zone::Local => cron
            .find_next_occurrence(&after.with_timezone(&Local), false)
            .ok()
            .map(|d| d.with_timezone(&Utc)),
    }
}

/// As próximas `n` ocorrências (prévia para a UI e validação de intervalo).
pub fn next_n(cron: &Cron, zone: Zone, from: DateTime<Utc>, n: usize) -> Vec<DateTime<Utc>> {
    let mut out = Vec::with_capacity(n);
    let mut cur = from;
    for _ in 0..n {
        match next_after(cron, zone, cur) {
            Some(d) => {
                out.push(d);
                cur = d;
            }
            None => break,
        }
    }
    out
}

/// Descrição em inglês gerada pela biblioteca (ex.: "At 08:00, every day").
pub fn describe(cron: &Cron) -> String {
    cron.describe()
}

/// Rejeita expressões que disparam com intervalo menor que `min_minutes`
/// (olhando as 4 primeiras ocorrências).
pub fn check_min_interval(cron: &Cron, zone: Zone, min_minutes: u32) -> Result<(), String> {
    if min_minutes == 0 {
        return Ok(());
    }
    let occ = next_n(cron, zone, Utc::now(), 4);
    for w in occ.windows(2) {
        let gap = w[1] - w[0];
        if gap < chrono::Duration::minutes(min_minutes as i64) {
            return Err(format!(
                "intervalo mínimo entre disparos é de {min_minutes} min (a expressão dispara a cada {} min)",
                gap.num_minutes().max(0)
            ));
        }
    }
    Ok(())
}

/// Interpreta uma data/hora do usuário/agente: ISO-8601 com fuso, ou
/// `YYYY-MM-DDTHH:MM[:SS]` / `YYYY-MM-DD HH:MM` sem fuso (assume `zone`).
pub fn parse_run_at(s: &str, zone: Zone) -> Result<DateTime<Utc>, String> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Ok(d.with_timezone(&Utc));
    }
    let naive = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M"))
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S"))
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M"))
        .map_err(|_| {
            format!(
                "data/hora inválida \"{s}\" (use YYYY-MM-DDTHH:MM, opcionalmente com fuso ±hh:mm)"
            )
        })?;
    let resolved: Option<DateTime<Utc>> = match zone {
        Zone::Named(tz) => tz
            .from_local_datetime(&naive)
            .single()
            .map(|d| d.with_timezone(&Utc)),
        Zone::Local => Local
            .from_local_datetime(&naive)
            .single()
            .map(|d| d.with_timezone(&Utc)),
    };
    resolved.ok_or_else(|| format!("data/hora ambígua ou inexistente no fuso: {s}"))
}

/// "em 2 h 05 min", "em 3 dias", "agora" ou "atrasado 4 min".
pub fn humanize_until(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let d = at - now;
    let secs = d.num_seconds();
    let (prefix, abs) = if secs >= 0 {
        ("em", secs)
    } else {
        ("atrasado", -secs)
    };
    if abs < 60 {
        return if secs >= 0 {
            "agora".into()
        } else {
            "atrasado".into()
        };
    }
    let days = abs / 86_400;
    let hours = (abs % 86_400) / 3600;
    let mins = (abs % 3600) / 60;
    let body = if days > 0 {
        if hours > 0 {
            format!("{days} d {hours} h")
        } else {
            format!("{days} d")
        }
    } else if hours > 0 {
        format!("{hours} h {mins:02} min")
    } else {
        format!("{mins} min")
    };
    format!("{prefix} {body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_next_in_named_zone() {
        let cron = parse_cron("0 8 * * *").unwrap();
        let zone = Zone::parse(Some("America/Sao_Paulo")).unwrap();
        let after = Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap(); // 09:00 em SP
        let next = next_after(&cron, zone, after).unwrap();
        // 08:00 de 29/10 em SP (UTC-3) = 11:00Z
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 10, 29, 11, 0, 0).unwrap());
    }

    #[test]
    fn rejects_bad_cron_and_zone() {
        assert!(parse_cron("a b c").is_err());
        assert!(Zone::parse(Some("Marte/Olympus")).is_err());
        assert!(Zone::parse(None).is_ok());
    }

    #[test]
    fn min_interval() {
        let zone = Zone::parse(Some("UTC")).unwrap();
        assert!(check_min_interval(&parse_cron("* * * * *").unwrap(), zone, 5).is_err());
        assert!(check_min_interval(&parse_cron("*/10 * * * *").unwrap(), zone, 5).is_ok());
    }

    #[test]
    fn run_at_formats() {
        let zone = Zone::parse(Some("America/Sao_Paulo")).unwrap();
        let a = parse_run_at("2026-10-29T10:00", zone).unwrap();
        assert_eq!(a, Utc.with_ymd_and_hms(2026, 10, 29, 13, 0, 0).unwrap());
        let b = parse_run_at("2026-10-29T10:00:00-03:00", zone).unwrap();
        assert_eq!(a, b);
        assert!(parse_run_at("amanhã", zone).is_err());
    }

    #[test]
    fn humanize() {
        let now = Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap();
        assert_eq!(
            humanize_until(now + chrono::Duration::minutes(5), now),
            "em 5 min"
        );
        assert_eq!(
            humanize_until(now + chrono::Duration::hours(26), now),
            "em 1 d 2 h"
        );
        assert_eq!(
            humanize_until(now - chrono::Duration::minutes(3), now),
            "atrasado 3 min"
        );
    }
}
