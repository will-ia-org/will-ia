//! Texto para leitura em voz alta **literal** (botão "ler" das respostas):
//! tira só a *marcação* do Markdown (asteriscos, cerquilhas, crases, colchetes
//! de link, barras de tabela) e mantém todas as palavras, na ordem, sem
//! resumir. Também parte o texto em pedaços que cabem no teto da API de TTS,
//! cortando em parágrafo/frase para a voz não quebrar no meio de uma palavra.

/// Teto de caracteres por chamada de TTS da OpenAI (4096); fica uma folga.
pub const TTS_CHUNK_CHARS: usize = 3500;

/// Markdown → texto falável, palavra por palavra. Blocos de código perdem só
/// as cercas (o conteúdo é lido como está); links viram o texto do link;
/// imagens viram o texto alternativo; linhas de tabela viram células
/// separadas por vírgula; réguas horizontais e tags HTML somem.
pub fn speakable_literal(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    let mut in_fence = false;
    let mut blank_run = 0usize;
    for raw in md.lines() {
        let line = raw.trim_end();
        let t = line.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        let text = if in_fence {
            Some(line.to_string())
        } else {
            block_text(t)
        };
        // `None` = linha só de marcação (régua, separador de tabela): some sem
        // virar linha em branco.
        let Some(text) = text else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            blank_run += 1;
            if blank_run == 1 && !out.is_empty() {
                out.push('\n');
            }
            continue;
        }
        blank_run = 0;
        out.push_str(text);
        out.push('\n');
    }
    out.trim().to_string()
}

/// Uma linha fora de bloco de código: tira o prefixo de bloco (título, lista,
/// citação) e a marcação inline.
fn block_text(t: &str) -> Option<String> {
    // régua horizontal
    if t.len() >= 3
        && (t.chars().all(|c| c == '-' || c == ' ')
            || t.chars().all(|c| c == '*' || c == ' ')
            || t.chars().all(|c| c == '_' || c == ' '))
    {
        return None;
    }
    let mut s = t;
    // citação
    while let Some(r) = s.strip_prefix('>') {
        s = r.trim_start();
    }
    // título
    if s.starts_with('#') {
        let n = s.chars().take_while(|c| *c == '#').count();
        if s[n..].starts_with(' ') || s[n..].is_empty() {
            s = s[n..].trim_start();
        }
    }
    // lista (marcador, tarefa, numerada)
    for m in ["- [ ] ", "- [x] ", "- [X] ", "- ", "* ", "+ "] {
        if let Some(r) = s.strip_prefix(m) {
            s = r;
            break;
        }
    }
    if let Some(r) = numbered_rest(s) {
        s = r;
    }
    // tabela: separador some; linha vira "célula, célula"
    if s.starts_with('|') {
        let cells: Vec<&str> = s
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .collect();
        if cells
            .iter()
            .all(|c| c.chars().all(|ch| ch == '-' || ch == ':'))
        {
            return None;
        }
        return Some(
            cells
                .iter()
                .map(|c| inline_text(c))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    Some(inline_text(s))
}

/// `1. texto` / `12) texto` → `texto`.
fn numbered_rest(s: &str) -> Option<&str> {
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = &s[digits..];
    rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))
}

/// Marcação inline: links/imagens → texto, tags HTML somem, `*`, `` ` ``,
/// `~~` e `_` de ênfase somem.
fn inline_text(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < n {
        let c = chars[i];
        // imagem ![alt](url) → alt ; link [texto](url) → texto
        if (c == '[' || (c == '!' && i + 1 < n && chars[i + 1] == '[')) && !in_escape(&chars, i) {
            let start = if c == '!' { i + 1 } else { i };
            if let Some((text_end, link_end)) = link_span(&chars, start) {
                let inner: String = chars[start + 1..text_end].iter().collect();
                out.push_str(&inline_text(&inner));
                i = link_end + 1;
                continue;
            }
        }
        // tag HTML conhecida (<br>, </b>, <a href=...>); `Vec<String>` fica
        if c == '<' {
            if let Some(end) = chars[i + 1..].iter().position(|&x| x == '>') {
                let tag: String = chars[i + 1..i + 1 + end].iter().collect();
                let name = tag
                    .trim_start_matches('/')
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if is_html_tag(&name) {
                    i += end + 2;
                    continue;
                }
            }
        }
        match c {
            '`' => {}
            // `*` solto entre espaços é um asterisco de verdade (2 * 3); colado
            // em texto é ênfase
            '*' => {
                let prev_space = i == 0 || chars[i - 1].is_whitespace();
                let next_space = i + 1 >= n || chars[i + 1].is_whitespace();
                if prev_space && next_space {
                    out.push('*');
                }
            }
            '~' if i + 1 < n && chars[i + 1] == '~' => {
                i += 1;
            }
            '_' => {
                // só é ênfase quando encosta em espaço/pontuação de um lado
                let prev_word = i > 0 && chars[i - 1].is_alphanumeric();
                let next_word = i + 1 < n && chars[i + 1].is_alphanumeric();
                if prev_word && next_word {
                    out.push('_');
                }
            }
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

/// Tags HTML que o Markdown aceita embutidas (o que vale ser apagado da fala).
fn is_html_tag(name: &str) -> bool {
    matches!(
        name,
        "a" | "b"
            | "i"
            | "u"
            | "s"
            | "em"
            | "strong"
            | "p"
            | "div"
            | "span"
            | "br"
            | "hr"
            | "code"
            | "pre"
            | "sub"
            | "sup"
            | "small"
            | "big"
            | "ul"
            | "ol"
            | "li"
            | "blockquote"
            | "img"
            | "table"
            | "thead"
            | "tbody"
            | "tr"
            | "td"
            | "th"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "del"
            | "ins"
            | "mark"
            | "kbd"
    )
}

fn in_escape(chars: &[char], i: usize) -> bool {
    i > 0 && chars[i - 1] == '\\'
}

/// A partir de `[` em `start`, devolve (índice do `]`, índice do `)` final)
/// quando é `[texto](destino)`.
fn link_span(chars: &[char], start: usize) -> Option<(usize, usize)> {
    let mut depth = 0i32;
    let mut text_end = None;
    for (k, &ch) in chars.iter().enumerate().skip(start) {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    text_end = Some(k);
                    break;
                }
            }
            '\n' => return None,
            _ => {}
        }
    }
    let te = text_end?;
    if chars.get(te + 1) != Some(&'(') {
        return None;
    }
    let close = chars[te + 2..].iter().position(|&x| x == ')')?;
    Some((te, te + 2 + close))
}

/// Parte o texto em pedaços de até `max` caracteres, preferindo cortar em
/// parágrafo, depois em frase, depois em espaço; só no limite corta no meio.
pub fn tts_chunks(text: &str, max: usize) -> Vec<String> {
    let max = max.max(50);
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let push_unit = |unit: &str, chunks: &mut Vec<String>, cur: &mut String| {
        let unit = unit.trim();
        if unit.is_empty() {
            return;
        }
        let sep = if cur.is_empty() { 0 } else { 1 };
        if cur.chars().count() + sep + unit.chars().count() <= max {
            if sep == 1 {
                cur.push('\n');
            }
            cur.push_str(unit);
        } else {
            if !cur.is_empty() {
                chunks.push(std::mem::take(cur));
            }
            if unit.chars().count() <= max {
                cur.push_str(unit);
            } else {
                for piece in split_long(unit, max) {
                    chunks.push(piece);
                }
            }
        }
    };
    for para in text.split("\n\n") {
        push_unit(para, &mut chunks, &mut cur);
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// Parágrafo maior que `max`: corta em frases; frase maior ainda, em espaços;
/// no limite, por contagem de caracteres.
fn split_long(unit: &str, max: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for sent in sentences(unit) {
        let sent = sent.trim();
        if sent.is_empty() {
            continue;
        }
        let sep = if cur.is_empty() { 0 } else { 1 };
        if cur.chars().count() + sep + sent.chars().count() <= max {
            if sep == 1 {
                cur.push(' ');
            }
            cur.push_str(sent);
            continue;
        }
        if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if sent.chars().count() <= max {
            cur.push_str(sent);
            continue;
        }
        // frase enorme: por palavras, depois por caracteres
        for word in sent.split_whitespace() {
            let sep = if cur.is_empty() { 0 } else { 1 };
            if cur.chars().count() + sep + word.chars().count() <= max {
                if sep == 1 {
                    cur.push(' ');
                }
                cur.push_str(word);
            } else {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let mut w: Vec<char> = word.chars().collect();
                while w.len() > max {
                    let rest = w.split_off(max);
                    out.push(w.iter().collect());
                    w = rest;
                }
                cur = w.iter().collect();
            }
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Frases: corta depois de `.`, `!`, `?`, `;` ou quebra de linha.
fn sentences(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        let end_here = c == '\n'
            || (matches!(c, '.' | '!' | '?' | ';')
                && iter.peek().is_none_or(|(_, nx)| nx.is_whitespace()));
        if end_here {
            let e = i + c.len_utf8();
            out.push(&s[start..e]);
            start = e;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_markup_keeps_words() {
        let md = "# Título\n\nOla **mundo** com _ênfase_ e `codigo` e [link](https://x.y).\n\n- item um\n- item dois\n1. primeiro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n\n```rust\nlet x = 1;\n```\n\n> citação\n\n![foto](a.png) snake_case fica.";
        let s = speakable_literal(md);
        assert_eq!(
            s,
            "Título\n\nOla mundo com ênfase e codigo e link.\n\nitem um\nitem dois\nprimeiro\n\na, b\n1, 2\n\nlet x = 1;\n\ncitação\n\nfoto snake_case fica."
        );
    }

    #[test]
    fn html_tags_and_strike() {
        assert_eq!(inline_text("a<br>b ~~x~~ <b>c</b> 2 < 3"), "ab x c 2 < 3");
        // conteúdo literal que parece marcação fica
        assert_eq!(
            inline_text("Vec<String> e 2 * 3 = 6, mas **isso** não"),
            "Vec<String> e 2 * 3 = 6, mas isso não"
        );
    }

    #[test]
    fn chunks_prefer_paragraphs_and_sentences() {
        let p1 = "a".repeat(60);
        let p2 = "b".repeat(60);
        let text = format!("{p1}\n\n{p2}");
        let c = tts_chunks(&text, 100);
        assert_eq!(c, vec![p1.clone(), p2.clone()]);
        // parágrafo longo: corta em frases
        let long = format!(
            "{}. {}. {}.",
            "x".repeat(40),
            "y".repeat(40),
            "z".repeat(40)
        );
        let c = tts_chunks(&long, 90);
        assert_eq!(c.len(), 2);
        assert!(c[0].starts_with("xxx") && c[0].ends_with("yyy."));
        assert!(c[1].starts_with("zzz"));
        // palavra gigante: corta por caracteres
        let giant = "w".repeat(230);
        let c = tts_chunks(&giant, 100);
        assert_eq!(c.iter().map(|x| x.chars().count()).sum::<usize>(), 230);
        assert!(c.iter().all(|x| x.chars().count() <= 100));
        assert!(tts_chunks("   ", 100).is_empty());
    }
}
