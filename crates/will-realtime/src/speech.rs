//! Limpeza do texto do cérebro antes de ir para a fala.
//!
//! Remove blocos de código, tabelas, marcadores de markdown e caminhos/URLs
//! longos, colapsa espaços e limita o tamanho.

/// Tamanho a partir do qual uma URL/caminho vira a palavra "link".
const LONG_PATH_CHARS: usize = 40;

/// Prepara texto do cérebro para ser falado pelo modelo Realtime.
pub fn clean_for_speech(text: &str, limit_chars: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut in_fence = false;

    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if line.is_empty() {
            continue;
        }
        // Tabelas e réguas horizontais.
        if line.starts_with('|') || is_rule(line) {
            continue;
        }
        let line = strip_line_prefix(line);
        let line = strip_inline_markdown(line);
        let words: Vec<String> = line.split_whitespace().map(replace_long_path).collect();
        if !words.is_empty() {
            out.push(words.join(" "));
        }
    }

    let joined = out.join(" ");
    let collapsed = joined.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&collapsed, limit_chars)
}

fn is_rule(line: &str) -> bool {
    line.len() >= 3
        && line
            .chars()
            .all(|c| c == '-' || c == '*' || c == '_' || c == '=')
}

/// Remove cabeçalhos (`#`), marcadores de lista (`-`, `*`, `+`, `1.`) e citações (`>`).
fn strip_line_prefix(line: &str) -> &str {
    let mut s = line;
    loop {
        let before = s;
        s = s.trim_start_matches('#').trim_start();
        s = s.trim_start_matches('>').trim_start();
        if let Some(rest) = s
            .strip_prefix("- ")
            .or_else(|| s.strip_prefix("* "))
            .or_else(|| s.strip_prefix("+ "))
        {
            s = rest.trim_start();
        }
        if let Some(rest) = s
            .strip_prefix("- [ ] ")
            .or_else(|| s.strip_prefix("- [x] "))
        {
            s = rest.trim_start();
        }
        // "1. " / "12) "
        let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && digits <= 3 {
            let rest = &s[digits..];
            if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
                s = r.trim_start();
            }
        }
        if s == before {
            return s;
        }
    }
}

/// Remove ênfases, crases e links `[texto](url)` / imagens `![alt](url)`.
fn strip_inline_markdown(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // ![alt](url) ou [texto](url) → alt/texto
        if c == '[' || (c == '!' && chars.get(i + 1) == Some(&'[')) {
            let start = if c == '!' { i + 1 } else { i };
            if let Some(close) = find_from(&chars, start + 1, ']') {
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(end) = find_from(&chars, close + 2, ')') {
                        out.extend(&chars[start + 1..close]);
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        if matches!(c, '*' | '_' | '`' | '~') {
            // `_` dentro de palavra (snake_case) é mantido.
            let prev_alnum = i > 0 && chars[i - 1].is_alphanumeric();
            let next_alnum = chars.get(i + 1).is_some_and(|n| n.is_alphanumeric());
            if c == '_' && prev_alnum && next_alnum {
                out.push(c);
            }
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn find_from(chars: &[char], from: usize, needle: char) -> Option<usize> {
    chars
        .iter()
        .skip(from)
        .position(|&c| c == needle)
        .map(|p| p + from)
}

fn replace_long_path(word: &str) -> String {
    let is_punct = |c: char| {
        matches!(
            c,
            ',' | '.' | ';' | ':' | '(' | ')' | '<' | '>' | '"' | '\'' | '!' | '?'
        )
    };
    let trimmed = word.trim_matches(is_punct);
    let is_url = trimmed.contains("://") || trimmed.starts_with("www.");
    let is_path = trimmed.matches(['/', '\\']).count() >= 2
        || trimmed.starts_with('/')
        || trimmed.starts_with("~/")
        || trimmed.starts_with("./");
    if (is_url || is_path) && trimmed.chars().count() > LONG_PATH_CHARS {
        // Preserva a pontuação em volta ("veja: http://…," → "veja: link,").
        let start = word.find(trimmed).unwrap_or(0);
        let end = start + trimmed.len();
        format!("{}link{}", &word[..start], &word[end..])
    } else {
        word.to_string()
    }
}

fn truncate_chars(s: &str, limit: usize) -> String {
    if limit == 0 || s.chars().count() <= limit {
        return s.to_string();
    }
    let mut cut: String = s.chars().take(limit).collect();
    // Evita cortar no meio de uma palavra quando possível.
    if let Some(pos) = cut.rfind(' ') {
        if pos > limit / 2 {
            cut.truncate(pos);
        }
    }
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_code_blocks_and_tables() {
        let text =
            "Resultado:\n```rust\nfn main() {}\n```\n| a | b |\n|---|---|\n| 1 | 2 |\nPronto.";
        assert_eq!(clean_for_speech(text, 1500), "Resultado: Pronto.");
    }

    #[test]
    fn strips_markdown_markers() {
        let text = "# Título\n- **Item** um\n- _Item_ dois com `código`\n1. [Doc](https://exemplo.com/a) ok";
        assert_eq!(
            clean_for_speech(text, 1500),
            "Título Item um Item dois com código Doc ok"
        );
    }

    #[test]
    fn replaces_long_urls_and_paths() {
        let long_url = format!("https://exemplo.com/{}", "a".repeat(50));
        let long_path = format!("/home/will/{}/arquivo.txt", "pasta/".repeat(10));
        let text = format!("Veja {long_url} e {long_path}, mas /etc/hosts fica.");
        assert_eq!(
            clean_for_speech(&text, 1500),
            "Veja link e link, mas /etc/hosts fica."
        );
    }

    #[test]
    fn collapses_whitespace_and_truncates() {
        let text = "a   b\n\n\n c\t d";
        assert_eq!(clean_for_speech(text, 1500), "a b c d");
        let long = "palavra ".repeat(100);
        let out = clean_for_speech(&long, 50);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 51);
    }

    #[test]
    fn keeps_snake_case() {
        assert_eq!(
            clean_for_speech("use ask_brain agora", 100),
            "use ask_brain agora"
        );
    }
}
