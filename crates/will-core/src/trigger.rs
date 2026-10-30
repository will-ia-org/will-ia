//! Triggers universais (`docs/30`): uma **fonte** de eventos (mensagem do
//! WhatsApp, e-mail novo no Gmail… — o agendamento por tempo continua em
//! `schedule.rs`, é o "trigger de relógio") passa por um **filtro** e, se
//! casar, **acorda uma thread** do Claude Code entregando `prompt` + todo o
//! contexto do evento como uma mensagem (dispara e esquece: ninguém espera a
//! resposta, o motor sobe o processo se estiver dormindo).
//!
//! O modelo é agnóstico à fonte: todo evento vira um [`TriggerEvent`] com
//! campos nomeados (`from`, `chat`, `subject`, `text`…) e o filtro é uma lista
//! de [`Condition`] (campo + glob, AND) mais um **escopo** opcional por fonte
//! (conta, chats escolhidos, labels/query do Gmail). Fonte nova = produzir
//! `TriggerEvent` e, se quiser, um escopo próprio; o resto (filtro, entrega,
//! histórico, UI) é compartilhado.

use crate::ids::{ThreadId, TriggerId};
use crate::thread::Origin;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// De onde vem o evento.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerSource {
    /// JSON: `"whatsapp"` (não `whats_app`), igual a [`TriggerSource::as_str`].
    #[serde(rename = "whatsapp")]
    WhatsApp,
    Gmail,
}

impl TriggerSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            TriggerSource::WhatsApp => "whatsapp",
            TriggerSource::Gmail => "gmail",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "whatsapp" | "whats_app" | "wa" => Some(TriggerSource::WhatsApp),
            "gmail" | "email" | "e-mail" | "mail" => Some(TriggerSource::Gmail),
            _ => None,
        }
    }
    /// Rótulo humano (pt-BR).
    pub fn label(&self) -> &'static str {
        match self {
            TriggerSource::WhatsApp => "WhatsApp",
            TriggerSource::Gmail => "E-mail (Gmail)",
        }
    }
    /// Campos que um evento desta fonte sempre traz (para a UI montar condições).
    pub fn fields(&self) -> &'static [&'static str] {
        match self {
            TriggerSource::WhatsApp => &[
                "from",
                "from_name",
                "from_phone",
                "chat",
                "chat_jid",
                "is_group",
                "kind",
                "text",
            ],
            TriggerSource::Gmail => &[
                "from",
                "from_name",
                "to",
                "cc",
                "subject",
                "labels",
                "snippet",
                "text",
            ],
        }
    }
}

/// Uma condição do filtro: o campo do evento precisa casar com o glob
/// (`*` = qualquer sequência, `?` = um caractere; sem distinção de maiúsculas;
/// um glob sem `*`/`?` casa como "contém"). `negate` inverte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Condition {
    pub field: String,
    pub glob: String,
    pub negate: bool,
}

/// Escopo da fonte + condições. Serializado como JSON em `triggers.filter_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TriggerFilter {
    /// Conta da fonte (id/e-mail); vazio = qualquer conta.
    pub account: String,
    /// WhatsApp: chats escolhidos (`jid`); vazio = qualquer chat.
    pub chats: Vec<ScopedChat>,
    /// WhatsApp: aceitar mensagens de grupos (padrão: não).
    pub include_groups: bool,
    /// WhatsApp: aceitar as mensagens que **eu** mandei (padrão: não).
    pub include_from_me: bool,
    /// Gmail: query da busca (`is:unread`, `in:inbox`…); vazio = `in:inbox`.
    pub gmail_query: String,
    /// Condições (AND) sobre os campos do evento.
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ScopedChat {
    pub jid: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trigger {
    pub id: TriggerId,
    pub name: String,
    pub source: TriggerSource,
    pub enabled: bool,
    pub filter: TriggerFilter,
    /// Instrução entregue à thread junto do contexto do evento.
    pub prompt: String,
    /// Thread que recebe a mensagem. `None` = criar na primeira vez (com
    /// `thread_title`) e guardar aqui.
    pub thread_id: Option<ThreadId>,
    pub thread_title: Option<String>,
    /// Sessão do Claude da thread (para `--resume` ao recriar).
    pub claude_session_id: Option<String>,
    pub origin: Origin,
    /// Segundos mínimos entre dois disparos desta trigger (0 = sem limite).
    pub cooldown_secs: u32,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub last_result: Option<String>,
    pub fire_count: i64,
    pub paused_reason: Option<String>,
    /// Posição de leitura da fonte (Gmail: `internalDate` ms do e-mail mais
    /// novo já visto; WhatsApp: não usa). JSON livre por fonte.
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TriggerRun {
    pub id: i64,
    pub trigger_id: TriggerId,
    pub at: DateTime<Utc>,
    pub thread_id: Option<ThreadId>,
    pub ok: bool,
    pub detail: Option<String>,
    /// Resumo de uma linha do evento que disparou.
    pub input: Option<String>,
}

/// Evento normalizado de qualquer fonte: campos nomeados (na ordem em que
/// devem aparecer no contexto) + corpo longo. É o que o filtro avalia e o que
/// vai inteiro para a thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TriggerEvent {
    pub source: Option<TriggerSource>,
    pub account: String,
    pub at: Option<DateTime<Utc>>,
    /// Uma linha para listas/histórico ("WhatsApp · Fulano: oi").
    pub summary: String,
    /// Campos nomeados, ex.: `("from", "5511…")`, `("subject", "Fatura")`.
    pub fields: Vec<(String, String)>,
    /// Corpo (texto da mensagem / do e-mail).
    pub body: String,
    /// Identificador na fonte (id da mensagem / do e-mail), para não repetir.
    pub ref_id: String,
    /// WhatsApp: a mensagem é minha; grupo.
    pub from_me: bool,
    pub is_group: bool,
    pub chat_jid: String,
}

impl TriggerEvent {
    pub fn field(&self, name: &str) -> Option<&str> {
        if name == "text" || name == "body" {
            if let Some((_, v)) = self.fields.iter().find(|(k, _)| k == name) {
                return Some(v);
            }
            return Some(&self.body);
        }
        self.fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Glob simples, sem distinção de maiúsculas: `*` qualquer sequência, `?` um
/// caractere. Casa em **qualquer trecho** do texto (`*@xpto.com` acha
/// `Fulano <joao@xpto.com>`); `^` no início e `$` no fim ancoram. Padrão sem
/// curingas casa por "contém".
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let raw = pattern.trim().to_lowercase();
    if raw.is_empty() {
        return true;
    }
    let anchored_start = raw.starts_with('^');
    let anchored_end = raw.ends_with('$') && !raw.ends_with("\\$");
    let core = raw
        .trim_start_matches('^')
        .trim_end_matches('$')
        .to_string();
    let mut pat = String::new();
    if !anchored_start {
        pat.push('*');
    }
    pat.push_str(&core);
    if !anchored_end {
        pat.push('*');
    }
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    // casamento iterativo com retrocesso para `*`
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark): (Option<usize>, usize) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

impl TriggerFilter {
    /// O evento passa pelo escopo e por todas as condições?
    pub fn matches(&self, ev: &TriggerEvent) -> bool {
        if !self.account.trim().is_empty()
            && !ev.account.is_empty()
            && !ev.account.eq_ignore_ascii_case(self.account.trim())
        {
            return false;
        }
        if ev.source == Some(TriggerSource::WhatsApp) {
            if ev.from_me && !self.include_from_me {
                return false;
            }
            if ev.is_group && !self.include_groups && self.chats.is_empty() {
                return false;
            }
            if !self.chats.is_empty() && !self.chats.iter().any(|c| c.jid == ev.chat_jid) {
                return false;
            }
        }
        self.conditions.iter().all(|c| {
            if c.field.trim().is_empty() || c.glob.trim().is_empty() {
                return true;
            }
            let hit = ev
                .field(c.field.trim())
                .map(|v| glob_match(&c.glob, v))
                .unwrap_or(false);
            hit != c.negate
        })
    }
}

/// Mensagem entregue à thread: cabeçalho (gerado pelo serviço, para o agente
/// saber que é um disparo), o **prompt configurado** e o **contexto inteiro**
/// do evento (todos os campos + corpo), para o agente localizar e agir.
pub fn fired_message(t: &Trigger, ev: &TriggerEvent, fired_at_local: &str) -> String {
    let source = t.source.label();
    let mut out = format!(
        "[Trigger \"{}\" ({source}) disparada em {fired_at_local}; id {}. Esta mensagem foi gerada por uma regra configurada previamente, não pelo usuário ao vivo: execute o que o prompt pede usando o contexto abaixo. Para ajustar a regra, use a página Triggers do app.]\n\n## Prompt da trigger\n{}\n\n## Contexto do evento\n",
        t.name,
        t.id,
        t.prompt.trim()
    );
    out.push_str(&format!("- fonte: {source}\n"));
    if !ev.account.is_empty() {
        out.push_str(&format!("- conta: {}\n", ev.account));
    }
    if let Some(at) = ev.at {
        out.push_str(&format!("- quando: {}\n", at.to_rfc3339()));
    }
    for (k, v) in &ev.fields {
        if k == "text" || k == "body" || v.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("- {k}: {v}\n"));
    }
    if !ev.ref_id.is_empty() {
        out.push_str(&format!("- id na fonte: {}\n", ev.ref_id));
    }
    if !ev.body.trim().is_empty() {
        out.push_str("\n## Conteúdo\n");
        out.push_str(ev.body.trim());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_json_matches_as_str() {
        for src in [TriggerSource::WhatsApp, TriggerSource::Gmail] {
            let j = serde_json::to_string(&src).unwrap();
            assert_eq!(j, format!("\"{}\"", src.as_str()));
            assert_eq!(serde_json::from_str::<TriggerSource>(&j).unwrap(), src);
            assert_eq!(TriggerSource::parse(src.as_str()), Some(src));
        }
        assert_eq!(TriggerSource::parse("whats_app"), Some(TriggerSource::WhatsApp));
    }

    #[test]
    fn glob_basics() {
        assert!(glob_match("*@xpto.com", "Fulano <joao@XPTO.com>"));
        assert!(!glob_match("*@xpto.com", "joao@outra.com>"));
        assert!(glob_match("fatura", "Sua FATURA chegou"));
        assert!(glob_match("fat?ra*", "fatura de outubro"));
        assert!(glob_match("", "qualquer coisa"));
        assert!(!glob_match("abc", "xyz"));
        assert!(glob_match("^fatura", "Fatura de outubro"));
        assert!(!glob_match("^fatura", "Sua fatura"));
        assert!(glob_match("outubro$", "fatura de outubro"));
        assert!(!glob_match("outubro$", "outubro chegou"));
    }

    fn ev() -> TriggerEvent {
        TriggerEvent {
            source: Some(TriggerSource::WhatsApp),
            account: "acc1".into(),
            fields: vec![
                ("from".into(), "5511999@s.whatsapp.net".into()),
                ("from_name".into(), "Pedro".into()),
                ("chat".into(), "Pedro".into()),
            ],
            body: "bom dia, manda o boleto".into(),
            chat_jid: "5511999@s.whatsapp.net".into(),
            ..Default::default()
        }
    }

    #[test]
    fn filter_scope_and_conditions() {
        let mut f = TriggerFilter::default();
        assert!(f.matches(&ev()));
        f.chats.push(ScopedChat {
            jid: "outro@s.whatsapp.net".into(),
            name: "Outro".into(),
        });
        assert!(!f.matches(&ev()));
        f.chats[0].jid = "5511999@s.whatsapp.net".into();
        assert!(f.matches(&ev()));
        f.conditions.push(Condition {
            field: "text".into(),
            glob: "*boleto*".into(),
            negate: false,
        });
        assert!(f.matches(&ev()));
        f.conditions[0].negate = true;
        assert!(!f.matches(&ev()));
        // grupo sem permissão
        let mut g = ev();
        g.is_group = true;
        let f2 = TriggerFilter::default();
        assert!(!f2.matches(&g));
        // mensagem minha
        let mut m = ev();
        m.from_me = true;
        assert!(!f2.matches(&m));
    }

    #[test]
    fn message_has_prompt_and_context() {
        let t = Trigger {
            id: TriggerId::new(),
            name: "Pedro".into(),
            source: TriggerSource::WhatsApp,
            enabled: true,
            filter: TriggerFilter::default(),
            prompt: "Responda com educação.".into(),
            thread_id: None,
            thread_title: None,
            claude_session_id: None,
            origin: Origin::Ui,
            cooldown_secs: 0,
            created_by: "user".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            last_fired_at: None,
            last_result: None,
            fire_count: 0,
            paused_reason: None,
            cursor: None,
        };
        let m = fired_message(&t, &ev(), "30/10 09:00");
        assert!(m.contains("Responda com educação."));
        assert!(m.contains("- from_name: Pedro"));
        assert!(m.contains("bom dia, manda o boleto"));
        assert!(m.contains("WhatsApp"));
    }
}
