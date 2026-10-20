//! Comandos builtin, comandos da KB (`.claude/commands/*.md`) e callbacks de botões
//! inline (troca de chave `k:<id>` e aprovações `ap:<id>:y|n`).

use crate::handler::Incoming;
use crate::text::{command_arg, escape_html, thread_key};
use crate::TelegramModule;
use std::path::Path;
use std::sync::Arc;
use teloxide::payloads::setters::*;
use teloxide::prelude::Requester;
use teloxide::types::{
    CallbackQuery, ChatId, InlineKeyboardButton, InlineKeyboardMarkup, MessageId, ParseMode,
    ThreadId,
};
use will_core::{ApprovalDecision, ApprovalId, ClaudeTokenInfo, Origin, ThreadSummary};

pub(crate) const BUILTIN_COMMANDS: &[(&str, &str)] = &[
    ("novo", "Zera a conversa desta thread e começa do zero"),
    (
        "reiniciar",
        "Reinicia o Claude desta thread (mantém o contexto)",
    ),
    ("status", "Status do bot e de todas as threads"),
    (
        "topico",
        "Cria um tópico novo no fórum (ex.: /topico Carteira)",
    ),
    (
        "apagar",
        "Apaga ESTE tópico e a thread dele (sessão + histórico)",
    ),
    (
        "limpar",
        "Remove do bot as threads dormentes (tópicos já apagados)",
    ),
    ("chave", "Escolhe qual conta/chave do Claude usar"),
    ("help", "Lista os comandos"),
];

const BUILTIN_NAMES: &[&str] = &[
    "start",
    "novo",
    "reiniciar",
    "status",
    "topico",
    "apagar",
    "limpar",
    "chave",
    "help",
    "ajuda",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoCommand {
    /// Nome no menu do Telegram (`[a-z0-9_]`).
    pub tg_name: String,
    /// Nome real do arquivo (pode ter hífen).
    pub real_name: String,
    pub description: String,
}

/// Lê `<kb>/.claude/commands/*.md`: nome (hífen → underscore) e `description:` do frontmatter.
pub(crate) fn load_repo_commands(kb_path: &Path) -> Vec<RepoCommand> {
    let dir = kb_path.join(".claude").join("commands");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut files: Vec<String> = rd
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|f| f.ends_with(".md"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let name = f.trim_end_matches(".md").to_string();
        let tg_name = name.replace('-', "_");
        if tg_name.is_empty()
            || tg_name.len() > 32
            || !tg_name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            continue;
        }
        let description = std::fs::read_to_string(dir.join(&f))
            .ok()
            .and_then(|content| frontmatter_description(&content))
            .unwrap_or_else(|| name.clone());
        out.push(RepoCommand {
            tg_name,
            real_name: name,
            description: description.chars().take(256).collect(),
        });
    }
    out
}

pub(crate) fn frontmatter_description(content: &str) -> Option<String> {
    for line in content.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("description:") {
            let start = line.len() - rest.len();
            let v = line[start..]
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn fmt_uptime(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86400,
        (secs % 86400) / 3600,
        (secs % 3600) / 60,
        secs % 60,
    );
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn telegram_key(t: &ThreadSummary) -> Option<String> {
    match t.origin {
        Origin::Telegram { chat_id, topic_id } => Some(thread_key(chat_id, topic_id)),
        _ => None,
    }
}

fn token_keyboard(tokens: &[ClaudeTokenInfo]) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(tokens.iter().map(|t| {
        let prefix = if t.active {
            "✅ "
        } else if t.status.as_deref() == Some("error") {
            "🔴 "
        } else {
            ""
        };
        vec![InlineKeyboardButton::callback(
            format!("{prefix}{}", t.label),
            format!("k:{}", t.id),
        )]
    }))
}

fn token_text(tokens: &[ClaudeTokenInfo]) -> String {
    let active = tokens.iter().find(|t| t.active);
    let mut s = String::from("🔑 <b>Chave do Claude</b>\n");
    match active {
        Some(a) => s.push_str(&format!(
            "Ativa: <b>{}</b> <code>{}</code>",
            escape_html(&a.label),
            escape_html(&a.masked)
        )),
        None => s.push_str("⚠️ Nenhuma ativa — usando o token do ambiente."),
    }
    s.push_str(
        "\n\n<i>Toque numa conta pra trocar. Eu testo antes; se falhar, mantenho a atual.\nAs threads são derrubadas e voltam com --resume — sem perder contexto.</i>",
    );
    s
}

impl TelegramModule {
    /// Trata um builtin. `false` se o comando não é builtin (vai para o cérebro).
    pub(crate) async fn handle_builtin(self: &Arc<Self>, inc: &Incoming, cmd: &str) -> bool {
        if !BUILTIN_NAMES.contains(&cmd) {
            return false;
        }
        let arg = command_arg(&inc.text);
        match cmd {
            "start" => self.cmd_start(inc, true).await,
            "novo" => self.cmd_novo(inc).await,
            "reiniciar" => self.cmd_reiniciar(inc).await,
            "status" => self.cmd_status(inc).await,
            "topico" => self.cmd_topico(inc, &arg).await,
            "apagar" => self.cmd_apagar(inc).await,
            "limpar" => self.cmd_limpar(inc, &arg).await,
            "chave" => self.cmd_chave(inc).await,
            "help" | "ajuda" => self.cmd_help(inc).await,
            _ => return false,
        }
        true
    }

    pub(crate) async fn cmd_start(&self, inc: &Incoming, allowed: bool) {
        let mut s = format!(
            "Oi! Sou o Will IA.\nO chat ID daqui é <code>{}</code>.\nSeu user ID é <code>{}</code>.",
            inc.chat_id,
            inc.user_id.map(|u| u.to_string()).unwrap_or_else(|| "?".into())
        );
        if inc.topic_id.is_some() {
            s.push_str(&format!(
                "\nEsta thread é <code>{}</code>.",
                escape_html(&inc.key)
            ));
        }
        if allowed {
            s.push_str("\nVocê está liberado ✅ — pode mandar ver.");
        } else {
            s.push_str("\nVocê ainda NÃO está na allowlist ⛔ — adicione o chat ID e o user ID nas configurações do Telegram do Will IA.");
        }
        self.reply(inc, &s, true).await;
    }

    async fn cmd_novo(&self, inc: &Incoming) {
        let conv = self.conv(&inc.key, inc.chat_id, inc.topic_id).await;
        conv.clear().await;
        if let Some(tid) = self.thread_of(&inc.key) {
            if let Err(e) = self.host.reset_thread(tid).await {
                self.reply(inc, &format!("⚠️ não consegui zerar a thread: {e}"), false)
                    .await;
                return;
            }
        }
        self.reply(
            inc,
            "🧹 Conversa desta thread zerada. Começamos do zero.",
            false,
        )
        .await;
    }

    async fn cmd_reiniciar(&self, inc: &Incoming) {
        if let Some(tid) = self.thread_of(&inc.key) {
            if let Err(e) = self.host.restart_thread(tid).await {
                self.reply(inc, &format!("⚠️ não consegui reiniciar: {e}"), false)
                    .await;
                return;
            }
        }
        self.reply(
            inc,
            "🔄 Reiniciei o Claude desta thread (o contexto é mantido). Manda a próxima mensagem.",
            false,
        )
        .await;
    }

    async fn cmd_status(&self, inc: &Incoming) {
        let up = fmt_uptime(self.host.uptime_secs());
        let mut rows: Vec<(String, ThreadSummary)> = self
            .host
            .list_threads()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| telegram_key(&t).map(|k| (k, t)))
            .collect();
        rows.sort_by(|a, b| b.1.alive.cmp(&a.1.alive).then_with(|| a.0.cmp(&b.0)));
        let vivos = rows.iter().filter(|(_, t)| t.alive).count();
        let names = self.topic_names.lock().clone();
        let lines = if rows.is_empty() {
            "  (nenhuma thread ainda)".to_string()
        } else {
            rows.iter()
                .map(|(k, t)| {
                    let mark = if *k == inc.key { "👉" } else { "    " };
                    let dot = if t.alive {
                        if t.pending > 0 {
                            "🟡"
                        } else {
                            "🟢"
                        }
                    } else if t.session_expired {
                        "❌"
                    } else {
                        "⚪"
                    };
                    let title = names
                        .get(k)
                        .cloned()
                        .or_else(|| t.title.clone())
                        .map(|n| format!(" · {}", escape_html(&n)))
                        .unwrap_or_default();
                    let fila = if t.pending > 0 {
                        format!(" · {} turno(s)", t.pending)
                    } else {
                        String::new()
                    };
                    format!("{mark} {dot} <code>{}</code>{title}{fila}", escape_html(k))
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let text = format!(
            "🟢 <b>no ar há {up}</b>\nthreads: {vivos} viva(s) de {} conhecida(s)\n\n{lines}\n\n<i>🟢 pronta · 🟡 trabalhando · ⚪ dormindo (volta com --resume) · ❌ sessão expirada (volta do zero)</i>",
            rows.len()
        );
        self.reply(inc, &text, true).await;
    }

    async fn cmd_topico(&self, inc: &Incoming, arg: &str) {
        if arg.is_empty() {
            self.reply(
                inc,
                "Uso: /topico <nome>  — cria um tópico novo neste fórum.",
                false,
            )
            .await;
            return;
        }
        let name: String = arg.chars().take(128).collect();
        match self
            .bot
            .create_forum_topic(ChatId(inc.chat_id), name.clone())
            .await
        {
            Ok(topic) => {
                let new_topic = i64::from(topic.thread_id.0 .0);
                self.topic_names
                    .lock()
                    .insert(thread_key(inc.chat_id, Some(new_topic)), name.clone());
                let greet = format!(
                    "🧵 Tópico \"{name}\" criado — cada tópico é uma thread própria, com sessão e contexto separados. Pode mandar ver por aqui."
                );
                if let Err(e) = self
                    .sender
                    .send_text(inc.chat_id, Some(new_topic), &greet, false, None)
                    .await
                {
                    tracing::warn!("saudar tópico novo: {e}");
                }
            }
            Err(e) => {
                self.reply(
                    inc,
                    &format!(
                        "⚠️ Não consegui criar o tópico: {}\n\nIsso só funciona num supergrupo com Tópicos ligados, e com o bot admin com permissão de gerenciar tópicos.",
                        escape_html(&e.to_string())
                    ),
                    true,
                )
                .await;
            }
        }
    }

    async fn cmd_apagar(&self, inc: &Incoming) {
        let Some(topic_id) = inc.topic_id else {
            self.reply(
                inc,
                "⚠️ Este comando só vale DENTRO de um tópico — ele apaga o tópico em que foi mandado.\n\nNo chat privado ou no \"General\", use /novo pra zerar a conversa.",
                false,
            )
            .await;
            return;
        };
        if let Some(tid) = self.thread_of(&inc.key) {
            if let Err(e) = self.host.delete_thread(tid, false).await {
                tracing::warn!(key = %inc.key, "delete_thread: {e}");
            }
        }
        self.forget_key(&inc.key).await;
        self.topic_names.lock().remove(&inc.key);
        match self
            .bot
            .delete_forum_topic(ChatId(inc.chat_id), ThreadId(MessageId(topic_id as i32)))
            .await
        {
            Ok(_) => self
                .bus
                .log("info", "telegram", format!("[{}] tópico apagado", inc.key)),
            Err(e) => {
                self.reply(
                    inc,
                    &format!(
                        "🧹 Thread apagada no bot (sessão e histórico), mas não consegui apagar o tópico: {}\n\nApague ele na mão: segure o tópico na lista → Apagar.",
                        escape_html(&e.to_string())
                    ),
                    true,
                )
                .await;
            }
        }
    }

    async fn cmd_limpar(&self, inc: &Incoming, arg: &str) {
        let dormentes: Vec<(String, ThreadSummary)> = self
            .host
            .list_threads()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| telegram_key(&t).map(|k| (k, t)))
            .filter(|(k, t)| !t.alive && t.pending == 0 && *k != inc.key)
            .collect();

        if arg.is_empty() {
            let text = if dormentes.is_empty() {
                "✅ Nenhuma thread dormente — está tudo limpo.".to_string()
            } else {
                format!(
                    "🧹 <b>Threads dormentes</b> (processo parado; sessão guardada):\n\n{}\n\nPra remover uma: <code>/limpar {}</code>\nPra remover todas: <code>/limpar tudo</code>\n\n<i>Remover só apaga a sessão e o histórico no bot — não mexe no tópico do Telegram.</i>",
                    dormentes.iter().map(|(k, _)| format!("⚪ <code>{}</code>", escape_html(k))).collect::<Vec<_>>().join("\n"),
                    escape_html(&dormentes[0].0)
                )
            };
            self.reply(inc, &text, true).await;
            return;
        }

        let alvos: Vec<&(String, ThreadSummary)> = if arg.eq_ignore_ascii_case("tudo") {
            dormentes.iter().collect()
        } else {
            dormentes.iter().filter(|(k, _)| k == arg).collect()
        };
        let mut removidas = Vec::new();
        for (k, t) in alvos {
            if let Err(e) = self.host.delete_thread(t.id, false).await {
                tracing::warn!(key = %k, "delete_thread: {e}");
                continue;
            }
            self.forget_key(k).await;
            removidas.push(k.clone());
        }
        let text = if removidas.is_empty() {
            format!("⚠️ Nada removido — não achei a thread dormente <code>{}</code>. Veja as chaves em /status.", escape_html(arg))
        } else {
            format!(
                "🧹 Removi {} thread(s):\n{}",
                removidas.len(),
                removidas
                    .iter()
                    .map(|k| format!("• <code>{}</code>", escape_html(k)))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        };
        self.reply(inc, &text, true).await;
    }

    async fn cmd_chave(&self, inc: &Incoming) {
        let tokens = self.host.claude_tokens();
        if tokens.is_empty() {
            self.reply(inc, "⚠️ Nenhuma chave cadastrada. Adicione os tokens do Claude na interface do Will IA.", false).await;
            return;
        }
        if let Err(e) = self
            .sender
            .send_one(
                inc.chat_id,
                inc.topic_id,
                &token_text(&tokens),
                true,
                None,
                Some(token_keyboard(&tokens)),
            )
            .await
        {
            tracing::warn!("/chave: {e}");
        }
    }

    async fn cmd_help(&self, inc: &Incoming) {
        let mut s = String::from("<b>Comandos</b>\n");
        for (c, d) in BUILTIN_COMMANDS {
            s.push_str(&format!("/{c} — {}\n", escape_html(d)));
        }
        let aliases: Vec<String> = self.repo_alias.lock().keys().cloned().collect();
        if !aliases.is_empty() {
            let mut a = aliases;
            a.sort();
            s.push_str("\n<b>Comandos da KB</b>\n");
            for k in a {
                s.push_str(&format!("/{k}\n"));
            }
        }
        s.push_str("\nQualquer outra mensagem vai para o cérebro desta thread.");
        self.reply(inc, &s, true).await;
    }

    // ---------- callbacks ----------

    pub(crate) async fn handle_callback(self: &Arc<Self>, cq: CallbackQuery) {
        let ack = |text: &str, alert: bool| {
            let bot = self.bot.clone();
            let id = cq.id.clone();
            let text = text.to_string();
            async move {
                if let Err(e) = bot
                    .answer_callback_query(id)
                    .text(text)
                    .show_alert(alert)
                    .await
                {
                    tracing::debug!("answerCallbackQuery: {e}");
                }
            }
        };
        let Some(message) = cq.message.as_ref() else {
            ack("", false).await;
            return;
        };
        let chat_id = message.chat().id.0;
        let user_id = cq.from.id.0 as i64;
        if !self.sender.allowed_chat(chat_id) || !self.sender.allowed_user(user_id) {
            ack("⛔ sem permissão", true).await;
            return;
        }
        let data = cq.data.clone().unwrap_or_default();
        let message_id = message.id().0;
        let topic_id = message.regular_message().and_then(|m| {
            if m.is_topic_message {
                m.thread_id.map(|t| i64::from(t.0 .0))
            } else {
                None
            }
        });

        if let Some(id) = data.strip_prefix("k:") {
            ack("", false).await;
            let me = self.clone();
            let id = id.to_string();
            tokio::spawn(async move { me.switch_token(chat_id, topic_id, message_id, &id).await });
            return;
        }
        if let Some(rest) = data.strip_prefix("ap:") {
            let mut it = rest.splitn(2, ':');
            let id = it.next().and_then(|s| ApprovalId::parse(s).ok());
            let decision = match it.next() {
                Some("y") => Some(ApprovalDecision::Approved),
                Some("n") => Some(ApprovalDecision::Denied),
                _ => None,
            };
            let (Some(id), Some(decision)) = (id, decision) else {
                ack("⚠️ botão inválido", true).await;
                return;
            };
            if self.host.decide_approval(id, decision, "telegram") {
                ack(
                    if decision == ApprovalDecision::Approved {
                        "✅ aprovado"
                    } else {
                        "❌ negado"
                    },
                    false,
                )
                .await;
            } else {
                ack(
                    "⚠️ pedido não encontrado (expirou ou já foi decidido)",
                    true,
                )
                .await;
                // tira o teclado mesmo assim
                self.mark_approval_message(chat_id, message_id, None, "⌛ pedido já encerrado")
                    .await;
            }
            return;
        }
        ack("", false).await;
    }

    async fn switch_token(&self, chat_id: i64, topic_id: Option<i64>, message_id: i32, id: &str) {
        let tokens = self.host.claude_tokens();
        let Some(alvo) = tokens.iter().find(|t| t.id == id).cloned() else {
            let _ = self
                .sender
                .send_text(
                    chat_id,
                    topic_id,
                    "⚠️ chave não encontrada — mande /chave de novo",
                    false,
                    None,
                )
                .await;
            return;
        };
        if alvo.active {
            let _ = self
                .sender
                .send_text(
                    chat_id,
                    topic_id,
                    &format!("✅ {} já é a chave ativa.", alvo.label),
                    false,
                    None,
                )
                .await;
            return;
        }
        let _ = self
            .sender
            .send_text(
                chat_id,
                topic_id,
                &format!("⏳ testando {}…", alvo.label),
                false,
                None,
            )
            .await;
        self.bus.log(
            "info",
            "telegram",
            format!("/chave: testando \"{}\" ({})", alvo.label, alvo.masked),
        );

        let result = self.host.switch_claude_token(id).await;
        let tokens = self.host.claude_tokens();
        let edit = self
            .bot
            .edit_message_text(ChatId(chat_id), MessageId(message_id), token_text(&tokens))
            .parse_mode(ParseMode::Html)
            .reply_markup(token_keyboard(&tokens))
            .await;
        if let Err(e) = edit {
            tracing::debug!("editar /chave: {e}");
        }
        let text = match result {
            Ok(n) => {
                self.bus.log(
                    "info",
                    "telegram",
                    format!(
                        "/chave: agora \"{}\" — {n} thread(s) derrubada(s)",
                        alvo.label
                    ),
                );
                format!(
                    "✅ Agora usando <b>{}</b> <code>{}</code>\n{}",
                    escape_html(&alvo.label),
                    escape_html(&alvo.masked),
                    if n > 0 {
                        format!("🔄 {n} thread(s) derrubada(s) — voltam com <code>--resume</code> na próxima mensagem, sem perder contexto.")
                    } else {
                        "💤 Nenhuma thread estava viva — já vale na próxima mensagem.".to_string()
                    }
                )
            }
            Err(e) => {
                self.bus.log(
                    "warn",
                    "telegram",
                    format!("/chave: \"{}\" reprovou — {e}", alvo.label),
                );
                format!(
                    "🔴 <b>{}</b> não passou no teste — mantive a chave atual.\n\n<code>{}</code>",
                    escape_html(&alvo.label),
                    escape_html(&e)
                )
            }
        };
        let _ = self
            .sender
            .send_text(chat_id, topic_id, &text, true, None)
            .await;
    }

    /// Edita a mensagem de aprovação com a decisão e remove o teclado.
    pub(crate) async fn mark_approval_message(
        &self,
        chat_id: i64,
        message_id: i32,
        base: Option<&str>,
        suffix: &str,
    ) {
        let text = match base {
            Some(b) => format!("{b}\n\n{suffix}"),
            None => suffix.to_string(),
        };
        let res = self
            .bot
            .edit_message_text(ChatId(chat_id), MessageId(message_id), text)
            .parse_mode(ParseMode::Html)
            .await;
        if let Err(e) = res {
            if !crate::sender::is_not_modified(&e) {
                tracing::debug!("editar aprovação: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_description_parses() {
        let md = "---\ndescription: \"Resumo das notícias\"\nallowed-tools: Bash\n---\n# x";
        assert_eq!(
            frontmatter_description(md).as_deref(),
            Some("Resumo das notícias")
        );
        assert_eq!(frontmatter_description("# sem frontmatter"), None);
    }

    #[test]
    fn repo_commands_from_dir() {
        let dir = tempdir();
        let cmds = dir.join(".claude").join("commands");
        std::fs::create_dir_all(&cmds).unwrap();
        std::fs::write(
            cmds.join("will-noticias.md"),
            "---\ndescription: Notícias do dia\n---\n",
        )
        .unwrap();
        std::fs::write(cmds.join("Maiusculo.md"), "x").unwrap();
        std::fs::write(cmds.join("simples.md"), "sem desc").unwrap();
        let list = load_repo_commands(&dir);
        assert_eq!(
            list,
            vec![
                RepoCommand {
                    tg_name: "simples".into(),
                    real_name: "simples".into(),
                    description: "simples".into()
                },
                RepoCommand {
                    tg_name: "will_noticias".into(),
                    real_name: "will-noticias".into(),
                    description: "Notícias do dia".into()
                },
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn uptime_format() {
        assert_eq!(fmt_uptime(5), "5s");
        assert_eq!(fmt_uptime(65), "1m 5s");
        assert_eq!(fmt_uptime(3661), "1h 1m 1s");
        assert_eq!(fmt_uptime(90000), "1d 1h 0m");
    }

    fn tempdir() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("will-telegram-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
