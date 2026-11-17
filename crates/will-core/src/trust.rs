//! Destinatários confiáveis (`docs/09`): a política `auto_listed` executa
//! sozinha só quando **todos** os destinatários da chamada estão na lista do
//! canal; o resto cai em confirmar.
//!
//! Cada ferramenta com destinatário declara o canal ([`TrustChannel`]) e
//! resolve, por chamada, para quem ela vai ([`Recipient`], com a chave já
//! canônica). O servidor MCP só compara chaves: não conhece WhatsApp nem
//! e-mail. As chaves canônicas saem daqui ([`wa_key`], [`email_key`]) para a
//! ferramenta, a API e o app normalizarem do mesmo jeito.

use serde::{Deserialize, Serialize};

/// Canal de uma lista de confiança.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustChannel {
    WhatsApp,
    Email,
}

impl TrustChannel {
    pub const ALL: [TrustChannel; 2] = [TrustChannel::WhatsApp, TrustChannel::Email];

    pub fn as_str(&self) -> &'static str {
        match self {
            TrustChannel::WhatsApp => "whatsapp",
            TrustChannel::Email => "email",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "whatsapp" => Some(TrustChannel::WhatsApp),
            "email" => Some(TrustChannel::Email),
            _ => None,
        }
    }

    /// Chave canônica de um valor digitado para este canal.
    pub fn key(&self, raw: &str) -> Option<String> {
        match self {
            TrustChannel::WhatsApp => wa_key(raw),
            TrustChannel::Email => email_key(raw),
        }
    }
}

/// Um destinatário (de uma chamada ou de uma lista): chave canônica para
/// comparar e rótulo legível para mostrar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Recipient {
    pub key: String,
    pub label: String,
}

impl Recipient {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
        }
    }
}

/// Listas de confiança por canal (`settings.mcp.trusted`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TrustedRecipients {
    pub whatsapp: Vec<Recipient>,
    pub email: Vec<Recipient>,
}

impl TrustedRecipients {
    pub fn list(&self, ch: TrustChannel) -> &[Recipient] {
        match ch {
            TrustChannel::WhatsApp => &self.whatsapp,
            TrustChannel::Email => &self.email,
        }
    }

    fn list_mut(&mut self, ch: TrustChannel) -> &mut Vec<Recipient> {
        match ch {
            TrustChannel::WhatsApp => &mut self.whatsapp,
            TrustChannel::Email => &mut self.email,
        }
    }

    /// A chave (já canônica) está liberada neste canal? No e-mail vale o
    /// endereço exato ou o domínio inteiro (`@empresa.com`).
    pub fn allows(&self, ch: TrustChannel, key: &str) -> bool {
        let list = self.list(ch);
        if list.iter().any(|e| e.key == key) {
            return true;
        }
        match ch {
            TrustChannel::Email => key
                .rsplit_once('@')
                .map(|(_, domain)| format!("@{domain}"))
                .is_some_and(|d| list.iter().any(|e| e.key == d)),
            TrustChannel::WhatsApp => false,
        }
    }

    /// Os que **não** estão liberados, na ordem recebida.
    pub fn unlisted(&self, ch: TrustChannel, recipients: &[Recipient]) -> Vec<Recipient> {
        recipients
            .iter()
            .filter(|r| !self.allows(ch, &r.key))
            .cloned()
            .collect()
    }

    /// Inclui (ou renomeia) uma entrada; `false` se a chave já estava com o
    /// mesmo rótulo.
    pub fn add(&mut self, ch: TrustChannel, entry: Recipient) -> bool {
        let list = self.list_mut(ch);
        match list.iter_mut().find(|e| e.key == entry.key) {
            Some(e) if e.label == entry.label || entry.label.trim().is_empty() => false,
            Some(e) => {
                e.label = entry.label;
                true
            }
            None => {
                list.push(entry);
                true
            }
        }
    }

    pub fn remove(&mut self, ch: TrustChannel, key: &str) -> bool {
        let list = self.list_mut(ch);
        let before = list.len();
        list.retain(|e| e.key != key);
        list.len() != before
    }
}

/// Servidor dos JIDs de grupo do WhatsApp.
const WA_GROUP_SUFFIX: &str = "@g.us";

/// Chave canônica de telefone do WhatsApp: só dígitos (8 a 15) e, no Brasil,
/// sem o nono dígito do celular — o WhatsApp entrega o mesmo número com ou
/// sem ele (`5511987654321` e `551187654321` são o mesmo contato).
pub fn wa_phone_key(raw: &str) -> Option<String> {
    let d: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if !(8..=15).contains(&d.len()) {
        return None;
    }
    let b = d.as_bytes();
    if d.len() == 13 && d.starts_with("55") && b[4] == b'9' && (b'6'..=b'9').contains(&b[5]) {
        return Some(format!("{}{}", &d[..4], &d[5..]));
    }
    Some(d)
}

/// Chave canônica de um destino do WhatsApp: JID de grupo (`…@g.us`, em
/// minúsculas) ou telefone ([`wa_phone_key`]; aceita `+55 (11) 9…`, só dígitos
/// ou JID de usuário `55…@s.whatsapp.net`). JID que não leva a um telefone
/// (ex.: `…@lid`) não tem chave: quem chama resolve o telefone antes.
pub fn wa_key(raw: &str) -> Option<String> {
    let s = raw.trim();
    match s.rsplit_once('@') {
        Some((user, _)) if s.to_ascii_lowercase().ends_with(WA_GROUP_SUFFIX) => {
            let user = user.trim();
            let ok = !user.is_empty()
                && user
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            ok.then(|| format!("{}{WA_GROUP_SUFFIX}", user.to_ascii_lowercase()))
        }
        Some((user, "s.whatsapp.net" | "c.us")) => {
            wa_phone_key(user.split(':').next().unwrap_or(user))
        }
        Some(_) => None,
        None => wa_phone_key(s),
    }
}

/// A chave é de grupo do WhatsApp?
pub fn wa_key_is_group(key: &str) -> bool {
    key.ends_with(WA_GROUP_SUFFIX)
}

/// Chave canônica de e-mail: endereço (`fulano@empresa.com`, também de
/// `Nome <fulano@empresa.com>`) ou domínio inteiro (`@empresa.com`), em
/// minúsculas. `None` se não parece nenhum dos dois.
pub fn email_key(raw: &str) -> Option<String> {
    let s = raw.trim();
    let s = match (s.rfind('<'), s.rfind('>')) {
        (Some(a), Some(b)) if a < b => &s[a + 1..b],
        _ => s,
    };
    let s = s.trim().trim_matches('"').to_lowercase();
    let (local, domain) = s.rsplit_once('@')?;
    let domain_ok = domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    let local_ok = !local.contains(|c: char| c.is_whitespace() || c == '@' || c == ',');
    (domain_ok && local_ok).then_some(s)
}

/// A chave de e-mail é um domínio inteiro (`@empresa.com`)?
pub fn email_key_is_domain(key: &str) -> bool {
    key.starts_with('@')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whatsapp_keys_are_canonical() {
        assert_eq!(
            wa_key("+55 (11) 98765-4321").as_deref(),
            Some("551187654321")
        );
        assert_eq!(wa_key("551187654321").as_deref(), Some("551187654321"));
        assert_eq!(
            wa_key("5511987654321@s.whatsapp.net").as_deref(),
            Some("551187654321")
        );
        assert_eq!(
            wa_key("5511987654321:12@s.whatsapp.net").as_deref(),
            Some("551187654321")
        );
        // fixo brasileiro e número de fora ficam como estão
        assert_eq!(wa_key("551133334444").as_deref(), Some("551133334444"));
        assert_eq!(wa_key("+1 415 555 2671").as_deref(), Some("14155552671"));
        assert_eq!(
            wa_key(" 120363041234567890@G.US ").as_deref(),
            Some("120363041234567890@g.us")
        );
        assert_eq!(wa_key("123456@lid"), None);
        assert_eq!(wa_key("Fulano"), None);
        assert_eq!(wa_key("123"), None);
        assert!(wa_key_is_group("1203@g.us"));
        assert!(!wa_key_is_group("551187654321"));
    }

    #[test]
    fn email_keys_are_canonical() {
        assert_eq!(
            email_key(" Fulano@Empresa.COM ").as_deref(),
            Some("fulano@empresa.com")
        );
        assert_eq!(
            email_key("Fulano de Tal <fulano@empresa.com>").as_deref(),
            Some("fulano@empresa.com")
        );
        assert_eq!(email_key("@Empresa.com").as_deref(), Some("@empresa.com"));
        assert_eq!(email_key("fulano"), None);
        assert_eq!(email_key("fulano@empresa"), None);
        assert_eq!(email_key("a b@empresa.com"), None);
        assert!(email_key_is_domain("@empresa.com"));
    }

    #[test]
    fn list_allows_exact_and_domain() {
        let mut t = TrustedRecipients::default();
        assert!(t.add(TrustChannel::Email, Recipient::new("@empresa.com", "")));
        assert!(t.add(TrustChannel::Email, Recipient::new("x@fora.com", "X")));
        assert!(!t.add(TrustChannel::Email, Recipient::new("x@fora.com", "X")));
        assert!(t.add(
            TrustChannel::WhatsApp,
            Recipient::new("551187654321", "Ana")
        ));
        assert!(t.allows(TrustChannel::Email, "qualquer@empresa.com"));
        assert!(t.allows(TrustChannel::Email, "x@fora.com"));
        assert!(!t.allows(TrustChannel::Email, "y@fora.com"));
        // subdomínio não herda o domínio
        assert!(!t.allows(TrustChannel::Email, "a@sub.empresa.com"));
        assert!(t.allows(TrustChannel::WhatsApp, "551187654321"));
        assert!(!t.allows(TrustChannel::WhatsApp, "1203@g.us"));
        let got = t.unlisted(
            TrustChannel::Email,
            &[
                Recipient::new("a@empresa.com", ""),
                Recipient::new("y@fora.com", ""),
            ],
        );
        assert_eq!(got, vec![Recipient::new("y@fora.com", "")]);
        assert!(t.remove(TrustChannel::Email, "x@fora.com"));
        assert!(!t.remove(TrustChannel::Email, "x@fora.com"));
    }
}
