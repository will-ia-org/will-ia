//! Tarefas (`docs/31`): itens com início obrigatório (data + hora), fim
//! opcional, prioridade e conclusão, vistos num Gantt no app e manipulados
//! pelo agente (`tasks_*`). O estado é **derivado** do relógio:
//! `upcoming` (ainda não começou) → `running` (começou; sem fim ou antes do fim)
//! → `overdue` (passou do fim sem concluir); `done` quando marcada concluída e
//! `skipped` quando fechada como **não realizada** (some da tela como a
//! concluída, mas registra que não foi feita — ex.: faltou à academia).

use crate::ids::TaskId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Prioridade: alta (1), média (2, padrão), baixa (3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    High,
    #[default]
    Medium,
    Low,
}

impl TaskPriority {
    /// Número da prioridade (1 = alta, 3 = baixa), usado na ordenação e no banco.
    pub fn rank(self) -> u8 {
        match self {
            TaskPriority::High => 1,
            TaskPriority::Medium => 2,
            TaskPriority::Low => 3,
        }
    }
    pub fn from_rank(n: i64) -> Self {
        match n {
            1 => TaskPriority::High,
            3 => TaskPriority::Low,
            _ => TaskPriority::Medium,
        }
    }
    /// Aceita `high|medium|low`, `alta|media|média|baixa` ou `1|2|3`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_lowercase();
        Some(match s.as_str() {
            "high" | "alta" | "1" => TaskPriority::High,
            "medium" | "media" | "média" | "normal" | "2" => TaskPriority::Medium,
            "low" | "baixa" | "3" => TaskPriority::Low,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            TaskPriority::High => "high",
            TaskPriority::Medium => "medium",
            TaskPriority::Low => "low",
        }
    }
    pub fn label_pt(self) -> &'static str {
        match self {
            TaskPriority::High => "alta",
            TaskPriority::Medium => "média",
            TaskPriority::Low => "baixa",
        }
    }
}

/// Estado derivado do relógio e da conclusão.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Upcoming,
    Running,
    Overdue,
    Done,
    Skipped,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskState::Upcoming => "upcoming",
            TaskState::Running => "running",
            TaskState::Overdue => "overdue",
            TaskState::Done => "done",
            TaskState::Skipped => "skipped",
        }
    }
    pub fn label_pt(self) -> &'static str {
        match self {
            TaskState::Upcoming => "futura",
            TaskState::Running => "em andamento",
            TaskState::Overdue => "atrasada",
            TaskState::Done => "concluída",
            TaskState::Skipped => "não realizada",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub title: String,
    pub description: String,
    /// Início (obrigatório, com hora).
    pub start_at: DateTime<Utc>,
    /// Fim (opcional): a partir dele, sem conclusão, a tarefa fica atrasada.
    pub end_at: Option<DateTime<Utc>>,
    /// Quando foi fechada (`None` = pendente): concluída, ou não realizada se `skipped`.
    pub done_at: Option<DateTime<Utc>>,
    /// Fechada como **não realizada** (só vale com `done_at`).
    #[serde(default)]
    pub skipped: bool,
    pub priority: TaskPriority,
    /// `agent` (via MCP) ou `user` (UI/API).
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Quando o laço notou que entrou em andamento / em atraso (evita avisar duas vezes).
    pub notified_start_at: Option<DateTime<Utc>>,
    pub notified_overdue_at: Option<DateTime<Utc>>,
    /// Vínculo com o Google Calendar (sincronização, `docs/31`): agenda e
    /// evento espelhados; `synced_at` = última sincronização no relógio local
    /// (`updated_at > synced_at` ⇒ editada aqui depois); `remote_updated_at` =
    /// `updated` do evento visto nela (diferente ⇒ editado no Google depois).
    #[serde(default)]
    pub calendar_id: Option<String>,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub synced_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub remote_updated_at: Option<DateTime<Utc>>,
    /// O evento espelhado é **especial** do Google (aniversário de contato,
    /// vindo do Gmail, fora do escritório, foco): a API não aceita gravar
    /// nele, então a tarefa só **recebe** — edição e exclusão ficam só aqui.
    #[serde(default)]
    pub event_read_only: bool,
    /// Etiquetas livres, sempre normalizadas ([`normalize_tag`]): minúsculas,
    /// sem acento, espaço vira `-`. Sem duplicatas, na ordem em que entraram.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Valor estimado a pagar/receber, em **centavos** (opcional). Com os dois,
    /// é uma faixa (mínimo–máximo); com um só, é o valor da tarefa.
    #[serde(default)]
    pub value_min_cents: Option<i64>,
    #[serde(default)]
    pub value_max_cents: Option<i64>,
}

/// Maior valor aceito numa tarefa (R$ 1 trilhão), em centavos.
pub const MONEY_MAX_CENTS: i64 = 100_000_000_000_000;

/// Lê um valor digitado em reais e devolve **centavos**. Aceita `1500`,
/// `1.500`, `1.500,50`, `1500.5`, `R$ 99,90`, `1,5k`, `2M`. Vazio ou zero =
/// `None` (sem valor). Negativo ou texto que não é número = erro.
pub fn parse_money(raw: &str) -> Result<Option<i64>, String> {
    let bad = || {
        format!(
            "valor inválido \"{}\": use algo como 1500, 1.500,50 ou 1,5k",
            raw.trim()
        )
    };
    let mut s: String = raw
        .trim()
        .trim_start_matches("R$")
        .trim_start_matches("r$")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if s.is_empty() {
        return Ok(None);
    }
    let mult = match s.chars().last() {
        Some('k' | 'K') => 1_000.0,
        Some('m' | 'M') => 1_000_000.0,
        _ => 1.0,
    };
    if mult > 1.0 {
        s.pop();
    }
    if s.starts_with('-') {
        return Err("o valor não pode ser negativo".into());
    }
    let normalized = if s.contains(',') {
        // vírgula decimal: os pontos são separador de milhar
        s.replace('.', "").replace(',', ".")
    } else if s.contains('.') {
        let mut parts = s.split('.');
        let head = parts.next().unwrap_or("");
        let groups: Vec<&str> = parts.collect();
        // "1.500" / "12.345.678" = milhar; "1.5" / "1234.56" = decimal
        if !head.is_empty() && head.len() <= 3 && groups.iter().all(|g| g.len() == 3) {
            s.replace('.', "")
        } else {
            s.clone()
        }
    } else {
        s.clone()
    };
    if normalized.is_empty() || !normalized.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err(bad());
    }
    let v: f64 = normalized.parse().map_err(|_| bad())?;
    money_from_reais(v * mult)
}

/// Reais (número) → centavos. Zero = `None`; negativo ou absurdo = erro.
pub fn money_from_reais(v: f64) -> Result<Option<i64>, String> {
    if !v.is_finite() {
        return Err("valor inválido".into());
    }
    if v < 0.0 {
        return Err("o valor não pode ser negativo".into());
    }
    let cents = (v * 100.0).round();
    if cents > MONEY_MAX_CENTS as f64 {
        return Err("valor alto demais".into());
    }
    let cents = cents as i64;
    Ok((cents > 0).then_some(cents))
}

/// Número com vírgula decimal, até `decimals` casas, sem zeros sobrando
/// (trunca, não arredonda para cima: 1.999 não vira "2K").
fn trimmed_decimal(v: f64, decimals: u32) -> String {
    let f = 10f64.powi(decimals as i32);
    let t = (v * f + 1e-6).floor() / f;
    let s = format!("{t:.*}", decimals as usize);
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    s.replace('.', ",")
}

/// Valor curto para caber no Gantt (sem "R$"): abaixo de mil, por extenso
/// (`100`, `99,90`); de mil para cima, abreviado (`1K`, `1,25K`, `12,5K`,
/// `250K`, `1,2M`).
pub fn money_short(cents: i64) -> String {
    let cents = cents.max(0);
    let reais = cents as f64 / 100.0;
    if cents < 100_000 {
        return if cents.rem_euclid(100) == 0 {
            format!("{}", cents / 100)
        } else {
            format!("{},{:02}", cents / 100, cents % 100)
        };
    }
    let (v, unit) = if reais < 1_000_000.0 {
        (reais / 1_000.0, "K")
    } else if reais < 1_000_000_000.0 {
        (reais / 1_000_000.0, "M")
    } else {
        (reais / 1_000_000_000.0, "B")
    };
    let decimals = if v < 10.0 {
        2
    } else if v < 100.0 {
        1
    } else {
        0
    };
    format!("{}{unit}", trimmed_decimal(v, decimals))
}

/// Valor completo em reais: `R$ 1.234,56`.
pub fn money_full(cents: i64) -> String {
    let cents = cents.max(0);
    let int = (cents / 100).to_string();
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i).is_multiple_of(3) {
            grouped.push('.');
        }
        grouped.push(c);
    }
    format!("R$ {grouped},{:02}", cents % 100)
}

/// Faixa curta: `1K` (um valor só, ou os dois iguais) ou `1K / 2K`.
pub fn money_range_short(min: Option<i64>, max: Option<i64>) -> Option<String> {
    match (min, max) {
        (Some(a), Some(b)) if a != b => Some(format!("{} / {}", money_short(a), money_short(b))),
        (Some(a), _) | (None, Some(a)) => Some(money_short(a)),
        (None, None) => None,
    }
}

/// Faixa completa: `R$ 1.000,00` ou `R$ 1.000,00 a R$ 2.000,00`.
pub fn money_range_full(min: Option<i64>, max: Option<i64>) -> Option<String> {
    match (min, max) {
        (Some(a), Some(b)) if a != b => Some(format!("{} a {}", money_full(a), money_full(b))),
        (Some(a), _) | (None, Some(a)) => Some(money_full(a)),
        (None, None) => None,
    }
}

/// Tamanho máximo de uma tag depois de normalizada.
pub const TAG_MAX: usize = 40;

/// Normaliza uma tag digitada: minúsculas, acentos removidos (`ç`→`c`,
/// `ã`→`a`…), espaços e separadores viram `-` (sem repetir, sem sobrar nas
/// pontas), só `[a-z0-9._-]` sobrevive, até [`TAG_MAX`] caracteres. `None` se
/// não sobrar nada.
pub fn normalize_tag(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut pending_dash = false;
    for c in raw.trim().chars() {
        let c = strip_accent(c).to_ascii_lowercase();
        let keep = match c {
            'a'..='z' | '0'..='9' | '.' | '_' => Some(c),
            // qualquer separador vira um traço só
            ' ' | '-' | '/' | '\\' | ',' | ';' | ':' | '|' | '\t' | '\n' | '+' | '&' => None,
            // o resto (acento sem mapa, emoji, pontuação) some
            _ if c.is_alphanumeric() && c.is_ascii() => Some(c),
            _ => continue,
        };
        match keep {
            Some(ch) => {
                if pending_dash && !out.is_empty() {
                    out.push('-');
                }
                pending_dash = false;
                out.push(ch);
            }
            None => pending_dash = true,
        }
    }
    let out: String = out
        .trim_matches(|c| c == '-' || c == '.' || c == '_')
        .chars()
        .take(TAG_MAX)
        .collect();
    let out = out.trim_end_matches('-').to_string();
    (!out.is_empty()).then_some(out)
}

/// Normaliza uma lista: aplica [`normalize_tag`], descarta vazias e repetidas
/// (mantendo a primeira ocorrência).
pub fn normalize_tags<I, S>(raw: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for r in raw {
        if let Some(t) = normalize_tag(r.as_ref()) {
            if !out.contains(&t) {
                out.push(t);
            }
        }
    }
    out
}

/// Letras acentuadas do português (e vizinhas) → base ASCII.
fn strip_accent(c: char) -> char {
    match c {
        'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' | 'Á' | 'À' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'a',
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
        'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => 'i',
        'ó' | 'ò' | 'ô' | 'õ' | 'ö' | 'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ö' => 'o',
        'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => 'u',
        'ç' | 'Ç' => 'c',
        'ñ' | 'Ñ' => 'n',
        'ý' | 'ÿ' | 'Ý' => 'y',
        other => other,
    }
}

impl Task {
    /// Aberta, ainda não começou e começa em até `secs` ("está chegando";
    /// `secs <= 0` = nunca).
    pub fn starts_within(&self, now: DateTime<Utc>, secs: i64) -> bool {
        secs > 0
            && self.done_at.is_none()
            && now < self.start_at
            && (self.start_at - now).num_seconds() <= secs
    }

    /// Tem valor a pagar/receber (mínimo, máximo ou os dois).
    pub fn has_value(&self) -> bool {
        self.value_min_cents.is_some() || self.value_max_cents.is_some()
    }

    /// Está espelhada num evento do Google Calendar.
    pub fn is_linked(&self) -> bool {
        self.calendar_id.is_some() && self.event_id.is_some()
    }

    pub fn state_at(&self, now: DateTime<Utc>) -> TaskState {
        if self.done_at.is_some() {
            return if self.skipped {
                TaskState::Skipped
            } else {
                TaskState::Done
            };
        }
        if now < self.start_at {
            return TaskState::Upcoming;
        }
        match self.end_at {
            Some(end) if now >= end => TaskState::Overdue,
            _ => TaskState::Running,
        }
    }
    pub fn is_done(&self) -> bool {
        self.done_at.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 10, h, 0, 0).unwrap()
    }

    fn task(start: u32, end: Option<u32>) -> Task {
        Task {
            id: TaskId::new(),
            title: "x".into(),
            description: String::new(),
            start_at: t(start),
            end_at: end.map(t),
            done_at: None,
            skipped: false,
            priority: TaskPriority::Medium,
            created_by: "user".into(),
            created_at: t(0),
            updated_at: t(0),
            notified_start_at: None,
            notified_overdue_at: None,
            calendar_id: None,
            event_id: None,
            tags: vec![],
            value_min_cents: None,
            value_max_cents: None,
            synced_at: None,
            remote_updated_at: None,
            event_read_only: false,
        }
    }

    #[test]
    fn state_follows_clock() {
        let k = task(10, Some(12));
        assert_eq!(k.state_at(t(9)), TaskState::Upcoming);
        assert_eq!(k.state_at(t(10)), TaskState::Running);
        assert_eq!(k.state_at(t(11)), TaskState::Running);
        assert_eq!(k.state_at(t(12)), TaskState::Overdue);
        let open = task(10, None);
        assert_eq!(open.state_at(t(23)), TaskState::Running);
        let mut done = task(10, Some(12));
        done.done_at = Some(t(11));
        assert_eq!(done.state_at(t(20)), TaskState::Done);
        done.skipped = true;
        assert_eq!(done.state_at(t(20)), TaskState::Skipped);
    }

    #[test]
    fn priority_parse() {
        assert_eq!(TaskPriority::parse("Alta"), Some(TaskPriority::High));
        assert_eq!(TaskPriority::parse("2"), Some(TaskPriority::Medium));
        assert_eq!(TaskPriority::parse("low"), Some(TaskPriority::Low));
        assert_eq!(TaskPriority::parse("urgente"), None);
        assert_eq!(TaskPriority::from_rank(3), TaskPriority::Low);
    }

    #[test]
    fn money_parses_and_abbreviates() {
        assert_eq!(parse_money(""), Ok(None));
        assert_eq!(parse_money("0"), Ok(None));
        assert_eq!(parse_money("1500"), Ok(Some(150_000)));
        assert_eq!(parse_money("1.500"), Ok(Some(150_000)));
        assert_eq!(parse_money("R$ 1.500,50"), Ok(Some(150_050)));
        assert_eq!(parse_money("1500.5"), Ok(Some(150_050)));
        assert_eq!(parse_money("99,9"), Ok(Some(9_990)));
        assert_eq!(parse_money("1,5k"), Ok(Some(150_000)));
        assert_eq!(parse_money("2M"), Ok(Some(200_000_000)));
        assert!(parse_money("-10").is_err());
        assert!(parse_money("abc").is_err());

        assert_eq!(money_short(1_000), "10");
        assert_eq!(money_short(10_000), "100");
        assert_eq!(money_short(9_990), "99,90");
        assert_eq!(money_short(100_000), "1K");
        assert_eq!(money_short(125_000), "1,25K");
        assert_eq!(money_short(199_999), "1,99K");
        assert_eq!(money_short(1_250_000), "12,5K");
        assert_eq!(money_short(25_000_000), "250K");
        assert_eq!(money_short(120_000_000), "1,2M");
        assert_eq!(money_full(123_456), "R$ 1.234,56");
        assert_eq!(money_full(990), "R$ 9,90");
        assert_eq!(
            money_range_short(Some(100_000), Some(200_000)).as_deref(),
            Some("1K / 2K")
        );
        assert_eq!(
            money_range_short(None, Some(50_000)).as_deref(),
            Some("500")
        );
        assert_eq!(
            money_range_short(Some(5_000), Some(5_000)).as_deref(),
            Some("50")
        );
        assert_eq!(money_range_short(None, None), None);
    }

    #[test]
    fn tags_are_normalized() {
        assert_eq!(normalize_tag("  Dev Mania ").as_deref(), Some("dev-mania"));
        assert_eq!(
            normalize_tag("Ação/Reunião").as_deref(),
            Some("acao-reuniao")
        );
        assert_eq!(normalize_tag("--Olá--").as_deref(), Some("ola"));
        assert_eq!(normalize_tag("v1.2_beta").as_deref(), Some("v1.2_beta"));
        assert_eq!(normalize_tag("🚀 !!!"), None);
        assert_eq!(normalize_tag("a   b\tc"), Some("a-b-c".into()));
        assert_eq!(
            normalize_tags(["Holding", "holding", "", "Pessoal ", "HOLDING"]),
            vec!["holding".to_string(), "pessoal".to_string()]
        );
    }
}
