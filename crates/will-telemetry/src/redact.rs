//! Redator de segredos em texto livre (tokens Anthropic/OpenAI/Telegram/Axiom,
//! `Bearer …`). Mantém um prefixo curto para o segredo continuar identificável.

use std::sync::OnceLock;

pub fn redact(s: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"(sk-ant-[A-Za-z0-9_\-]{6})[A-Za-z0-9_\-]+|(sk-[A-Za-z0-9]{4})[A-Za-z0-9\-_]{16,}|(xa(?:at|pt)-[A-Za-z0-9]{4})[A-Za-z0-9\-_]{16,}|(\d{8,}:[A-Za-z0-9_\-]{4})[A-Za-z0-9_\-]{20,}|(Bearer\s+[A-Za-z0-9_\-\.]{4})[A-Za-z0-9_\-\.]+|(ya29\.[A-Za-z0-9_\-]{4})[A-Za-z0-9_\-\.]+|(1//[A-Za-z0-9_\-]{4})[A-Za-z0-9_\-]{20,}",
        )
        .unwrap_or_else(|e| panic!("regex do redator: {e}"))
    });
    if !re.is_match(s) {
        return s.to_string();
    }
    re.replace_all(s, |c: &regex::Captures| {
        let keep = (1..=7)
            .filter_map(|i| c.get(i))
            .map(|m| m.as_str())
            .next()
            .unwrap_or("");
        format!("{keep}…[redigido]")
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn redacts_known_shapes() {
        let s = "token sk-ant-abcdef0123456789XYZ e Bearer abcd.efgh.ijkl e xaat-abcd1234567890abcdefghij";
        let r = redact(s);
        assert!(!r.contains("0123456789XYZ"));
        assert!(r.contains("sk-ant-abcdef…[redigido]"));
        assert!(r.contains("Bearer abcd…[redigido]"));
        assert!(r.contains("xaat-abcd…[redigido]"));
    }

    #[test]
    fn leaves_plain_text() {
        assert_eq!(redact("nada aqui"), "nada aqui");
    }
}
