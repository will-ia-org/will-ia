//! Helpers puros de texto: thread key, split em 4096, normalização de comandos,
//! escape/conversão para HTML do Telegram, sanitização de nomes de arquivo.

pub const TELEGRAM_MAX: usize = 4096;

/// Thread key: `<chat_id>` no privado/General, `<chat_id>:<topic_id>` em tópico.
pub fn thread_key(chat_id: i64, topic_id: Option<i64>) -> String {
    match topic_id {
        Some(t) => format!("{chat_id}:{t}"),
        None => chat_id.to_string(),
    }
}

/// Inverso de [`thread_key`].
pub fn parse_thread_key(key: &str) -> Option<(i64, Option<i64>)> {
    let mut it = key.split(':');
    let chat_id: i64 = it.next()?.trim().parse().ok()?;
    let topic_id = match it.next() {
        Some(t) => Some(t.trim().parse().ok()?),
        None => None,
    };
    Some((chat_id, topic_id))
}

/// Escapa o necessário para `parse_mode=HTML`.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Quebra texto em pedaços de no máximo `max` chars preferindo `\n`; se a melhor
/// quebra ficar antes da metade, corta no limite.
pub fn split_message(text: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut out = Vec::new();
    let mut rest: Vec<char> = text.chars().collect();
    while rest.len() > max {
        let window = &rest[..=max.min(rest.len() - 1)];
        // último '\n' dentro de [0, max]
        let mut cut = window.iter().rposition(|c| *c == '\n').unwrap_or(0);
        if cut == 0 || cut * 2 < max {
            cut = max;
        }
        let head: String = rest[..cut].iter().collect();
        out.push(head);
        let mut tail = rest.split_off(cut);
        if tail.first() == Some(&'\n') {
            tail.remove(0);
        }
        rest = tail;
    }
    if !rest.is_empty() {
        out.push(rest.into_iter().collect());
    }
    out
}

/// Remove acentos (decomposição manual das letras latinas comuns).
pub fn strip_accents(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'Á' | 'À' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'A',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'É' | 'È' | 'Ê' | 'Ë' => 'E',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'Í' | 'Ì' | 'Î' | 'Ï' => 'I',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ö' => 'O',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'Ú' | 'Ù' | 'Û' | 'Ü' => 'U',
            'ç' => 'c',
            'Ç' => 'C',
            'ñ' => 'n',
            'Ñ' => 'N',
            other => other,
        })
        .collect()
}

/// Nome do comando a partir do texto: tira `/`, `@bot`, acentos e caixa.
/// `/tópico Carteira` → `topico`; `/status@meu_bot` → `status`.
pub fn command_name(text: &str) -> String {
    let first = text.split_whitespace().next().unwrap_or("");
    let first = first.strip_prefix('/').unwrap_or(first);
    let first = first.split('@').next().unwrap_or("");
    strip_accents(first).to_lowercase()
}

/// Argumento do comando (tudo depois do primeiro token).
pub fn command_arg(text: &str) -> String {
    let trimmed = text.trim_start();
    match trimmed.split_whitespace().next() {
        Some(first) => trimmed[first.len()..].trim().to_string(),
        None => String::new(),
    }
}

/// Uma linha, sem excesso, curta (máx. `max` chars, com `…`).
pub fn clip(text: &str, max: usize) -> String {
    let collapsed: Vec<&str> = text.split_whitespace().collect();
    let t = collapsed.join(" ");
    if t.chars().count() > max {
        let mut out: String = t.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        t
    }
}

/// Sanitiza nome de arquivo: `[^\w.-]` → `_`, mantém no máximo 80 chars (do fim).
pub fn sanitize_filename(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('.').to_string();
    let count = s.chars().count();
    if count > 80 {
        s.chars().skip(count - 80).collect()
    } else {
        s
    }
}

/// Conversão mínima Markdown → HTML do Telegram: escapa `& < >`, ```blocos``` → `<pre>`,
/// `código` → `<code>`, **negrito** → `<b>`, *itálico*/_itálico_ (palavra inteira) → `<i>`,
/// e remove marcadores de título `#`.
pub fn markdown_to_html(md: &str) -> String {
    let mut out = String::with_capacity(md.len() + 32);
    let mut rest = md;
    loop {
        match rest.find("```") {
            None => {
                out.push_str(&inline_md(rest));
                break;
            }
            Some(start) => {
                out.push_str(&inline_md(&rest[..start]));
                let after = &rest[start + 3..];
                match after.find("```") {
                    None => {
                        // bloco não fechado: trata o resto como código
                        let body = strip_lang_line(after);
                        out.push_str("<pre>");
                        out.push_str(&escape_html(body));
                        out.push_str("</pre>");
                        break;
                    }
                    Some(end) => {
                        let body = strip_lang_line(&after[..end]);
                        out.push_str("<pre>");
                        out.push_str(&escape_html(body.trim_end_matches('\n')));
                        out.push_str("</pre>");
                        rest = &after[end + 3..];
                    }
                }
            }
        }
    }
    out
}

fn strip_lang_line(body: &str) -> &str {
    // "```rust\n..." → remove a primeira linha se for só um identificador
    if let Some(nl) = body.find('\n') {
        let first = &body[..nl];
        if !first.is_empty()
            && first.len() <= 20
            && first
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+' || c == '#')
        {
            return &body[nl + 1..];
        }
    }
    body.strip_prefix('\n').unwrap_or(body)
}

fn inline_md(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // títulos: "## Foo" → "<b>Foo</b>"
        let trimmed = line.trim_start();
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            out.push_str("<b>");
            out.push_str(&inline_spans(trimmed[hashes + 1..].trim()));
            out.push_str("</b>");
            continue;
        }
        out.push_str(&inline_spans(line));
    }
    out
}

/// Processa `código`, **negrito**, *itálico* e _itálico_ numa linha.
fn inline_spans(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    // primeiro separa os trechos de `código` (dentro deles nada mais é interpretado)
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        match after.find('`') {
            Some(end) if end > 0 => {
                out.push_str(&emphasis(&rest[..start]));
                out.push_str("<code>");
                out.push_str(&escape_html(&after[..end]));
                out.push_str("</code>");
                rest = &after[end + 1..];
            }
            _ => {
                // crase solta: escapa o resto
                out.push_str(&emphasis(&rest[..start]));
                out.push_str(&escape_html(&rest[start..]));
                rest = "";
                break;
            }
        }
    }
    out.push_str(&emphasis(rest));
    out
}

fn emphasis(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let escaped = escape_html(text);
    let bold = replace_pairs(&escaped, "**", "<b>", "</b>");
    let bold = replace_pairs(&bold, "__", "<b>", "</b>");
    let ital = replace_word_pairs(&bold, '*', "<i>", "</i>");
    replace_word_pairs(&ital, '_', "<i>", "</i>")
}

/// `**x**` → `<b>x</b>` (sem atravessar quebra de linha; conteúdo não vazio).
fn replace_pairs(s: &str, marker: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find(marker) {
        let after = &rest[start + marker.len()..];
        match after.find(marker) {
            Some(end) if end > 0 && !after[..end].contains('\n') => {
                out.push_str(&rest[..start]);
                out.push_str(open);
                out.push_str(&after[..end]);
                out.push_str(close);
                rest = &after[end + marker.len()..];
            }
            _ => {
                out.push_str(&rest[..start + marker.len()]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `*x*` / `_x_` → `<i>x</i>` quando o marcador abre no início de palavra e fecha no fim.
fn replace_word_pairs(s: &str, marker: char, open: &str, close: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let at_word_start =
            i == 0 || chars[i - 1].is_whitespace() || "([{\"'".contains(chars[i - 1]);
        if c == marker
            && at_word_start
            && i + 1 < chars.len()
            && !chars[i + 1].is_whitespace()
            && chars[i + 1] != marker
        {
            // procura o fechamento
            let mut j = i + 1;
            let mut found = None;
            while j < chars.len() && chars[j] != '\n' {
                if chars[j] == marker
                    && !chars[j - 1].is_whitespace()
                    && (j + 1 == chars.len()
                        || !(chars[j + 1].is_alphanumeric() || chars[j + 1] == marker))
                {
                    found = Some(j);
                    break;
                }
                j += 1;
            }
            if let Some(j) = found {
                out.push_str(open);
                out.extend(&chars[i + 1..j]);
                out.push_str(close);
                i = j + 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_key_roundtrip() {
        assert_eq!(thread_key(-100123, None), "-100123");
        assert_eq!(thread_key(-100123, Some(42)), "-100123:42");
        assert_eq!(parse_thread_key("-100123"), Some((-100123, None)));
        assert_eq!(parse_thread_key("-100123:42"), Some((-100123, Some(42))));
        assert_eq!(parse_thread_key("abc"), None);
    }

    #[test]
    fn split_prefers_newline() {
        let text = format!("{}\n{}", "a".repeat(3000), "b".repeat(3000));
        let parts = split_message(&text, 4096);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].len(), 3000);
        assert_eq!(parts[1].len(), 3000);
        assert!(parts[1].starts_with('b'));
    }

    #[test]
    fn split_cuts_at_limit_when_break_is_too_early() {
        let text = format!("{}\n{}", "a".repeat(100), "b".repeat(5000));
        let parts = split_message(&text, 4096);
        assert_eq!(parts[0].chars().count(), 4096);
        assert!(parts.iter().all(|p| p.chars().count() <= 4096));
        assert_eq!(parts.concat().len(), text.len());
    }

    #[test]
    fn split_short_text_is_single_chunk() {
        assert_eq!(split_message("oi", 4096), vec!["oi".to_string()]);
        assert!(split_message("", 4096).is_empty());
    }

    #[test]
    fn split_counts_chars_not_bytes() {
        let text = "é".repeat(5000);
        let parts = split_message(&text, 4096);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].chars().count(), 4096);
    }

    #[test]
    fn command_normalization() {
        assert_eq!(command_name("/tópico Carteira"), "topico");
        assert_eq!(command_name("/TOPICO x"), "topico");
        assert_eq!(command_name("/status@meu_bot"), "status");
        assert_eq!(command_name("/novo"), "novo");
        assert_eq!(command_name("/reiniciar@Bot extra"), "reiniciar");
        assert_eq!(command_arg("/tópico  Minha Carteira "), "Minha Carteira");
        assert_eq!(command_arg("/novo"), "");
    }

    #[test]
    fn clip_collapses_and_truncates() {
        assert_eq!(clip("  a   b\n\nc ", 280), "a b c");
        let long = "x".repeat(300);
        let c = clip(&long, 280);
        assert_eq!(c.chars().count(), 280);
        assert!(c.ends_with('…'));
    }

    #[test]
    fn sanitize_names() {
        assert_eq!(
            sanitize_filename("meu arquivo (1).pdf"),
            "meu_arquivo__1_.pdf"
        );
        assert_eq!(sanitize_filename("../../etc/passwd"), "_.._etc_passwd");
        let long = format!("{}.txt", "a".repeat(100));
        assert_eq!(sanitize_filename(&long).chars().count(), 80);
    }

    #[test]
    fn markdown_basic() {
        assert_eq!(markdown_to_html("a < b & c > d"), "a &lt; b &amp; c &gt; d");
        assert_eq!(
            markdown_to_html("**negrito** e `cod<e>`"),
            "<b>negrito</b> e <code>cod&lt;e&gt;</code>"
        );
        assert_eq!(
            markdown_to_html("```rust\nlet x = 1;\n```"),
            "<pre>let x = 1;</pre>"
        );
        assert_eq!(
            markdown_to_html("```\na<b\n```\nfim"),
            "<pre>a&lt;b</pre>\nfim"
        );
        assert_eq!(markdown_to_html("## Título"), "<b>Título</b>");
        assert_eq!(
            markdown_to_html("isso é *itálico* aqui"),
            "isso é <i>itálico</i> aqui"
        );
        assert_eq!(
            markdown_to_html("snake_case_name fica"),
            "snake_case_name fica"
        );
        assert_eq!(markdown_to_html("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(markdown_to_html("crase ` solta"), "crase ` solta");
    }
}
