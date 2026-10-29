use crate::ids::ThreadId;
use serde::{Deserialize, Serialize};

/// De onde uma thread se origina. Somente canais criam threads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    Ui,
    Telegram {
        chat_id: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        topic_id: Option<i64>,
    },
    Realtime,
}

impl Origin {
    pub fn kind(&self) -> OriginKind {
        match self {
            Origin::Ui => OriginKind::Ui,
            Origin::Telegram { .. } => OriginKind::Telegram,
            Origin::Realtime => OriginKind::Realtime,
        }
    }

    /// Representação estável para persistência/env (`WILL_IA_ORIGIN`).
    pub fn origin_ref(&self) -> Option<String> {
        match self {
            Origin::Ui => None,
            Origin::Telegram { chat_id, topic_id } => Some(match topic_id {
                Some(t) => format!("{chat_id}:{t}"),
                None => chat_id.to_string(),
            }),
            Origin::Realtime => Some("voice".into()),
        }
    }

    pub fn from_parts(kind: &str, origin_ref: Option<&str>) -> Option<Self> {
        match kind {
            "ui" => Some(Origin::Ui),
            "realtime" => Some(Origin::Realtime),
            "telegram" => {
                let r = origin_ref?;
                let mut it = r.split(':');
                let chat_id = it.next()?.parse().ok()?;
                let topic_id = it.next().and_then(|t| t.parse().ok());
                Some(Origin::Telegram { chat_id, topic_id })
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginKind {
    Ui,
    Telegram,
    Realtime,
}

impl OriginKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OriginKind::Ui => "ui",
            OriginKind::Telegram => "telegram",
            OriginKind::Realtime => "realtime",
        }
    }
}

impl std::fmt::Display for OriginKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    /// Sem processo vivo (sessão salva, volta com --resume) ou nunca iniciada.
    Idle,
    /// Processo vivo processando um turno.
    Running,
    /// Processo vivo aguardando mensagens.
    Live,
    Error,
    Archived,
}

impl ThreadStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThreadStatus::Idle => "idle",
            ThreadStatus::Running => "running",
            ThreadStatus::Live => "live",
            ThreadStatus::Error => "error",
            ThreadStatus::Archived => "archived",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "running" => ThreadStatus::Running,
            "live" => ThreadStatus::Live,
            "error" => ThreadStatus::Error,
            "archived" => ThreadStatus::Archived,
            _ => ThreadStatus::Idle,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub id: ThreadId,
    pub origin: Origin,
    pub title: Option<String>,
    pub claude_session_id: Option<String>,
    pub status: ThreadStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_activity_at: chrono::DateTime<chrono::Utc>,
    pub total_cost_usd: f64,
    /// Processo vivo neste momento?
    #[serde(default)]
    pub alive: bool,
    /// Mensagens enviadas ainda sem `result`.
    #[serde(default)]
    pub pending: usize,
    /// Sessão salva não existe mais no disco (vai voltar do zero).
    #[serde(default)]
    pub session_expired: bool,
    /// Resumo curto do que foi conversado por último (gerado a cada N turnos;
    /// `engine.thread_summary`). `None` até o primeiro resumo.
    #[serde(default)]
    pub summary: Option<String>,
}

/// Títulos genéricos dados na criação, substituídos pelo assunto na primeira
/// mensagem (ver `derive_title`).
pub fn is_placeholder_title(title: Option<&str>) -> bool {
    match title.map(str::trim) {
        None | Some("") => true,
        Some(t) => matches!(
            t.to_lowercase().as_str(),
            "voz" | "nova conversa" | "nova thread"
        ),
    }
}

/// Deriva um título curto e legível do primeiro pedido de uma thread: primeira
/// linha/frase, sem marcação, cortado em palavra inteira (máx. ~48 caracteres).
/// Arquivo que acompanha uma mensagem do usuário (anexo do Telegram, por ora).
/// O caminho é local à máquina do serviço; a UI mostra miniatura/chip e abre o arquivo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub path: String,
    #[serde(default)]
    pub size: u64,
}

impl Attachment {
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }
}

const ATTACHMENT_NOTE_PREFIX: &str = "[O usuário enviou um arquivo ";

/// Nota que o canal acrescenta ao prompt para o cérebro saber onde está o
/// arquivo. `via` é o canal, como "pelo Telegram" ou "pelo app".
pub fn attachment_note(a: &Attachment, via: &str) -> String {
    let mime = if a.mime.is_empty() {
        "tipo desconhecido"
    } else {
        a.mime.as_str()
    };
    format!(
        "{ATTACHMENT_NOTE_PREFIX}{via}: \"{}\" ({mime}), salvo em: {}. Leia/analise conforme o pedido acima; se não houver pedido específico, descreva o conteúdo de forma útil.]",
        a.name, a.path
    )
}

/// Separa o texto "humano" do prompt das notas de anexo (formato de
/// [`attachment_note`]); também recupera anexos de turnos antigos, gravados só
/// na nota. Devolve o texto limpo e os anexos encontrados.
pub fn split_attachment_notes(prompt: &str) -> (String, Vec<Attachment>) {
    let mut text = String::new();
    let mut atts = Vec::new();
    let mut rest = prompt;
    while let Some(start) = rest.find(ATTACHMENT_NOTE_PREFIX) {
        let Some(end_rel) = rest[start..].find(']') else {
            break;
        };
        let note = &rest[start + ATTACHMENT_NOTE_PREFIX.len()..start + end_rel];
        text.push_str(&rest[..start]);
        rest = &rest[start + end_rel + 1..];
        // `pelo Telegram: "nome" (mime), salvo em: caminho. Leia/analise…`
        let Some((_via, after)) = note.split_once(": \"") else {
            continue;
        };
        let Some((name, after)) = after.split_once("\" (") else {
            continue;
        };
        let Some((mime, after)) = after.split_once("), salvo em: ") else {
            continue;
        };
        let path = after
            .split_once(". Leia/analise")
            .map(|(p, _)| p)
            .unwrap_or(after)
            .trim();
        atts.push(Attachment {
            name: name.to_string(),
            mime: if mime == "tipo desconhecido" {
                String::new()
            } else {
                mime.to_string()
            },
            path: path.to_string(),
            size: 0,
        });
    }
    text.push_str(rest);
    (text.trim().to_string(), atts)
}

pub fn derive_title(prompt: &str) -> String {
    const MAX: usize = 48;
    let first_line = prompt
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    // remove marcação leve e espaços repetidos
    let cleaned: String = first_line
        .chars()
        .filter(|c| !matches!(c, '#' | '*' | '`' | '>' | '_' | '"' | '“' | '”'))
        .collect();
    let mut words: Vec<&str> = cleaned.split_whitespace().collect();
    // corta na primeira frase, se houver pontuação forte dentro do limite
    if let Some(pos) = words.iter().position(|w| w.ends_with(['.', '?', '!', ';'])) {
        let upto: usize = words[..=pos].iter().map(|w| w.chars().count() + 1).sum();
        if upto <= MAX + 1 && pos >= 2 {
            words.truncate(pos + 1);
        }
    }
    let mut out = String::new();
    for w in words {
        let add = w.chars().count() + usize::from(!out.is_empty());
        if out.chars().count() + add > MAX {
            if !out.is_empty() {
                out.push('…');
            } else {
                out = w.chars().take(MAX - 1).collect::<String>() + "…";
            }
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    let out = out
        .trim_end_matches([',', ';', ':', '.'])
        .trim()
        .to_string();
    if out.is_empty() {
        return "Sem título".to_string();
    }
    let mut c = out.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => out,
    }
}

#[cfg(test)]
mod title_tests {
    use super::*;

    #[test]
    fn placeholder_titles() {
        assert!(is_placeholder_title(None));
        assert!(is_placeholder_title(Some("  ")));
        assert!(is_placeholder_title(Some("Voz")));
        assert!(!is_placeholder_title(Some("Revisão dos docs")));
    }

    #[test]
    fn derives_short_titles() {
        assert_eq!(derive_title("liste meus arquivos"), "Liste meus arquivos");
        assert_eq!(
            derive_title("Quero que você revise o relatório de vendas. Depois mande por e-mail."),
            "Quero que você revise o relatório de vendas"
        );
        let long = derive_title("preciso de um resumo bem detalhado de todas as reuniões da semana passada com o time de produto");
        assert!(long.chars().count() <= 49, "{long}");
        assert!(long.ends_with('…'));
        assert_eq!(derive_title("# **oi**"), "Oi");
        assert_eq!(derive_title("   \n  "), "Sem título");
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::*;

    #[test]
    fn note_roundtrip_and_legacy() {
        let a = Attachment {
            name: "foto-1.jpg".into(),
            mime: "image/jpeg".into(),
            path: "/kb/inbox/telegram/-100-1-foto-1.jpg".into(),
            size: 0,
        };
        let prompt = format!("o que é isso ?\n\n{}", attachment_note(&a, "pelo Telegram"));
        let (text, atts) = split_attachment_notes(&prompt);
        assert_eq!(text, "o que é isso ?");
        assert_eq!(atts, vec![a]);
        // sem nota: texto intacto, sem anexos
        let (text, atts) = split_attachment_notes("oi [x] tchau");
        assert_eq!(text, "oi [x] tchau");
        assert!(atts.is_empty());
        // mime desconhecido vira vazio
        let b = Attachment {
            name: "arq".into(),
            mime: String::new(),
            path: "/p/arq".into(),
            size: 0,
        };
        let (_, atts) = split_attachment_notes(&attachment_note(&b, "pelo app"));
        assert_eq!(atts[0].mime, "");
        assert_eq!(atts[0].path, "/p/arq");
    }
}
