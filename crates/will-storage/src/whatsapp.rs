//! Índice local do WhatsApp (chats, mensagens, contatos, grupos, outbox),
//! particionado por conta pareada (`account_id`).

use crate::{fmt, now, parse_dt, Result, Storage};
use sqlx::Row;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WaChat {
    pub account_id: String,
    pub jid: String,
    pub name: Option<String>,
    pub is_group: bool,
    pub archived: bool,
    pub unread_count: i64,
    pub last_message_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_message_preview: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WaMessage {
    pub account_id: String,
    pub id: String,
    pub chat_jid: String,
    pub sender_jid: Option<String>,
    pub sender_name: Option<String>,
    pub from_me: bool,
    pub at: chrono::DateTime<chrono::Utc>,
    pub kind: String,
    pub text: Option<String>,
    pub media_ref: Option<String>,
    pub media_path: Option<String>,
    pub quoted_id: Option<String>,
    pub read: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WaContact {
    pub account_id: String,
    pub jid: String,
    pub phone: Option<String>,
    pub name: Option<String>,
    pub push_name: Option<String>,
}

fn row_msg(r: &sqlx::sqlite::SqliteRow) -> WaMessage {
    let at: String = r.get("at");
    WaMessage {
        account_id: r.get("account_id"),
        id: r.get("id"),
        chat_jid: r.get("chat_jid"),
        sender_jid: r.get("sender_jid"),
        sender_name: r.get("sender_name"),
        from_me: r.get::<i64, _>("from_me") != 0,
        at: parse_dt(&at),
        kind: r.get("kind"),
        text: r.get("text"),
        media_ref: r.get("media_ref"),
        media_path: r.get("media_path"),
        quoted_id: r.get("quoted_id"),
        read: r.get::<i64, _>("read") != 0,
    }
}

fn row_chat(r: &sqlx::sqlite::SqliteRow) -> WaChat {
    let last: Option<String> = r.get("last_message_at");
    WaChat {
        account_id: r.get("account_id"),
        jid: r.get("jid"),
        name: r.get("name"),
        is_group: r.get::<i64, _>("is_group") != 0,
        archived: r.get::<i64, _>("archived") != 0,
        unread_count: r.get("unread_count"),
        last_message_at: last.as_deref().map(parse_dt),
        last_message_preview: r.get("last_message_preview"),
    }
}

fn row_contact(r: &sqlx::sqlite::SqliteRow) -> WaContact {
    WaContact {
        account_id: r.get("account_id"),
        jid: r.get("jid"),
        phone: r.get("phone"),
        name: r.get("name"),
        push_name: r.get("push_name"),
    }
}

impl Storage {
    pub async fn wa_upsert_contact(&self, c: &WaContact) -> Result<()> {
        sqlx::query(
            "INSERT INTO wa_contacts (account_id, jid, phone, name, push_name, updated_at) VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(account_id, jid) DO UPDATE SET phone = COALESCE(excluded.phone, wa_contacts.phone), name = COALESCE(excluded.name, wa_contacts.name), push_name = COALESCE(excluded.push_name, wa_contacts.push_name), updated_at = excluded.updated_at",
        )
        .bind(&c.account_id)
        .bind(&c.jid)
        .bind(&c.phone)
        .bind(&c.name)
        .bind(&c.push_name)
        .bind(fmt(now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn wa_search_contacts(
        &self,
        account_id: &str,
        query: Option<&str>,
        limit: i64,
    ) -> Result<Vec<WaContact>> {
        let q = format!("%{}%", query.unwrap_or("").trim());
        let rows = sqlx::query(
            "SELECT * FROM wa_contacts WHERE account_id = ? AND (name LIKE ? OR push_name LIKE ? OR phone LIKE ? OR jid LIKE ?) ORDER BY name LIMIT ?",
        )
        .bind(account_id)
        .bind(&q)
        .bind(&q)
        .bind(&q)
        .bind(&q)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(row_contact).collect())
    }

    pub async fn wa_upsert_chat(
        &self,
        account_id: &str,
        jid: &str,
        name: Option<&str>,
        is_group: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO wa_chats (account_id, jid, name, is_group) VALUES (?, ?, ?, ?) ON CONFLICT(account_id, jid) DO UPDATE SET name = COALESCE(excluded.name, wa_chats.name), is_group = excluded.is_group",
        )
        .bind(account_id)
        .bind(jid)
        .bind(name)
        .bind(is_group as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn wa_set_chat_archived(
        &self,
        account_id: &str,
        jid: &str,
        archived: bool,
    ) -> Result<()> {
        sqlx::query("UPDATE wa_chats SET archived = ? WHERE account_id = ? AND jid = ?")
            .bind(archived as i64)
            .bind(account_id)
            .bind(jid)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn wa_insert_message(&self, m: &WaMessage) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO wa_messages (account_id, id, chat_jid, sender_jid, sender_name, from_me, at, kind, text, media_ref, media_path, quoted_id, read) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&m.account_id)
        .bind(&m.id)
        .bind(&m.chat_jid)
        .bind(&m.sender_jid)
        .bind(&m.sender_name)
        .bind(m.from_me as i64)
        .bind(fmt(m.at))
        .bind(&m.kind)
        .bind(&m.text)
        .bind(&m.media_ref)
        .bind(&m.media_path)
        .bind(&m.quoted_id)
        .bind(m.read as i64)
        .execute(&self.pool)
        .await?;
        let preview = m.text.clone().unwrap_or_else(|| format!("[{}]", m.kind));
        let unread_inc = if m.from_me || m.read { 0 } else { 1 };
        sqlx::query(
            "UPDATE wa_chats SET last_message_at = ?, last_message_preview = ?, unread_count = unread_count + ? WHERE account_id = ? AND jid = ? AND (last_message_at IS NULL OR last_message_at <= ?)",
        )
        .bind(fmt(m.at))
        .bind(will_core::tools::truncate(&preview, 120))
        .bind(unread_inc)
        .bind(&m.account_id)
        .bind(&m.chat_jid)
        .bind(fmt(m.at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn wa_set_media_path(&self, account_id: &str, id: &str, path: &str) -> Result<()> {
        sqlx::query("UPDATE wa_messages SET media_path = ? WHERE account_id = ? AND id = ?")
            .bind(path)
            .bind(account_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn wa_mark_read(&self, account_id: &str, chat_jid: &str) -> Result<()> {
        sqlx::query(
            "UPDATE wa_messages SET read = 1 WHERE account_id = ? AND chat_jid = ? AND read = 0",
        )
        .bind(account_id)
        .bind(chat_jid)
        .execute(&self.pool)
        .await?;
        sqlx::query("UPDATE wa_chats SET unread_count = 0 WHERE account_id = ? AND jid = ?")
            .bind(account_id)
            .bind(chat_jid)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn wa_list_chats(
        &self,
        account_id: &str,
        filter: &str,
        limit: i64,
    ) -> Result<Vec<WaChat>> {
        let sql = match filter {
            "unread" => "SELECT * FROM wa_chats WHERE account_id = ? AND unread_count > 0 ORDER BY last_message_at DESC LIMIT ?",
            "groups" => "SELECT * FROM wa_chats WHERE account_id = ? AND is_group = 1 ORDER BY last_message_at DESC LIMIT ?",
            "archived" => "SELECT * FROM wa_chats WHERE account_id = ? AND archived = 1 ORDER BY last_message_at DESC LIMIT ?",
            _ => "SELECT * FROM wa_chats WHERE account_id = ? AND archived = 0 ORDER BY last_message_at DESC LIMIT ?",
        };
        let rows = sqlx::query(sql)
            .bind(account_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(row_chat).collect())
    }

    pub async fn wa_get_chat(&self, account_id: &str, jid: &str) -> Result<Option<WaChat>> {
        let r = sqlx::query("SELECT * FROM wa_chats WHERE account_id = ? AND jid = ?")
            .bind(account_id)
            .bind(jid)
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.as_ref().map(row_chat))
    }

    pub async fn wa_find_chat_by_name(&self, account_id: &str, name: &str) -> Result<Vec<WaChat>> {
        let q = format!("%{}%", name.trim());
        let rows = sqlx::query("SELECT * FROM wa_chats WHERE account_id = ? AND name LIKE ? ORDER BY last_message_at DESC LIMIT 10")
            .bind(account_id)
            .bind(q)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(row_chat).collect())
    }

    pub async fn wa_get_messages(
        &self,
        account_id: &str,
        chat_jid: &str,
        limit: i64,
        before: Option<&str>,
        since: Option<&str>,
    ) -> Result<Vec<WaMessage>> {
        let rows = match (before, since) {
            (Some(b), _) => {
                sqlx::query("SELECT * FROM wa_messages WHERE account_id = ? AND chat_jid = ? AND at < ? ORDER BY at DESC LIMIT ?")
                    .bind(account_id)
                    .bind(chat_jid)
                    .bind(b)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
            (None, Some(s)) => {
                sqlx::query("SELECT * FROM wa_messages WHERE account_id = ? AND chat_jid = ? AND at >= ? ORDER BY at DESC LIMIT ?")
                    .bind(account_id)
                    .bind(chat_jid)
                    .bind(s)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
            _ => {
                sqlx::query("SELECT * FROM wa_messages WHERE account_id = ? AND chat_jid = ? ORDER BY at DESC LIMIT ?")
                    .bind(account_id)
                    .bind(chat_jid)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        let mut v: Vec<WaMessage> = rows.iter().map(row_msg).collect();
        v.reverse();
        Ok(v)
    }

    /// Troca o texto de uma mensagem (edição) mantendo o FTS em dia (gatilho `au`).
    pub async fn wa_set_text(&self, account_id: &str, id: &str, text: &str) -> Result<bool> {
        let r = sqlx::query("UPDATE wa_messages SET text = ? WHERE account_id = ? AND id = ?")
            .bind(text)
            .bind(account_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Remove uma mensagem do índice (apagada no WhatsApp).
    pub async fn wa_delete_message(&self, account_id: &str, id: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM wa_messages WHERE account_id = ? AND id = ?")
            .bind(account_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Estado de um envio nosso: `(status, error, at, chat_jid, kind)`.
    #[allow(clippy::type_complexity)]
    pub async fn wa_get_outbox(
        &self,
        account_id: &str,
        id: &str,
    ) -> Result<Option<(String, Option<String>, String, String, String)>> {
        let r = sqlx::query(
            "SELECT status, error, at, chat_jid, kind FROM wa_outbox WHERE account_id = ? AND id = ?",
        )
        .bind(account_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(|row| {
            (
                row.get::<String, _>("status"),
                row.get::<Option<String>, _>("error"),
                row.get::<String, _>("at"),
                row.get::<String, _>("chat_jid"),
                row.get::<String, _>("kind"),
            )
        }))
    }

    pub async fn wa_get_message(&self, account_id: &str, id: &str) -> Result<Option<WaMessage>> {
        let r = sqlx::query("SELECT * FROM wa_messages WHERE account_id = ? AND id = ?")
            .bind(account_id)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.as_ref().map(row_msg))
    }

    pub async fn wa_search_messages(
        &self,
        account_id: &str,
        query: &str,
        chat_jid: Option<&str>,
        since: Option<&str>,
        limit: i64,
    ) -> Result<Vec<WaMessage>> {
        // FTS5: aspas para busca por frase/termos simples.
        let q = query.replace('"', " ");
        let rows = sqlx::query(
            "SELECT m.* FROM wa_messages m JOIN wa_messages_fts f ON f.rowid = m.rowid WHERE wa_messages_fts MATCH ? AND m.account_id = ? AND (? IS NULL OR m.chat_jid = ?) AND (? IS NULL OR m.at >= ?) ORDER BY m.at DESC LIMIT ?",
        )
        .bind(&q)
        .bind(account_id)
        .bind(chat_jid)
        .bind(chat_jid)
        .bind(since)
        .bind(since)
        .bind(limit)
        .fetch_all(&self.pool)
        .await;
        let rows = match rows {
            Ok(r) => r,
            Err(_) => {
                // fallback LIKE quando a sintaxe FTS falha
                let like = format!("%{}%", query);
                sqlx::query("SELECT * FROM wa_messages WHERE text LIKE ? AND account_id = ? AND (? IS NULL OR chat_jid = ?) AND (? IS NULL OR at >= ?) ORDER BY at DESC LIMIT ?")
                    .bind(like)
                    .bind(account_id)
                    .bind(chat_jid)
                    .bind(chat_jid)
                    .bind(since)
                    .bind(since)
                    .bind(limit)
                    .fetch_all(&self.pool)
                    .await?
            }
        };
        Ok(rows.iter().map(row_msg).collect())
    }

    pub async fn wa_upsert_group(
        &self,
        account_id: &str,
        jid: &str,
        subject: Option<&str>,
        description: Option<&str>,
        participants: &serde_json::Value,
        admins: &serde_json::Value,
    ) -> Result<()> {
        self.wa_upsert_chat(account_id, jid, subject, true).await?;
        sqlx::query(
            "INSERT INTO wa_groups (account_id, jid, subject, description, participants_json, admins_json, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?) ON CONFLICT(account_id, jid) DO UPDATE SET subject = excluded.subject, description = excluded.description, participants_json = excluded.participants_json, admins_json = excluded.admins_json, updated_at = excluded.updated_at",
        )
        .bind(account_id)
        .bind(jid)
        .bind(subject)
        .bind(description)
        .bind(participants.to_string())
        .bind(admins.to_string())
        .bind(fmt(now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn wa_get_group(
        &self,
        account_id: &str,
        jid: &str,
    ) -> Result<Option<serde_json::Value>> {
        let r = sqlx::query("SELECT * FROM wa_groups WHERE account_id = ? AND jid = ?")
            .bind(account_id)
            .bind(jid)
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.map(|r| {
            let p: Option<String> = r.get("participants_json");
            let a: Option<String> = r.get("admins_json");
            serde_json::json!({
                "jid": r.get::<String,_>("jid"),
                "subject": r.get::<Option<String>,_>("subject"),
                "description": r.get::<Option<String>,_>("description"),
                "participants": p.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
                "admins": a.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()),
            })
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn wa_record_outbox(
        &self,
        account_id: &str,
        id: &str,
        chat_jid: &str,
        kind: &str,
        status: &str,
        error: Option<&str>,
        approval_id: Option<&str>,
    ) -> Result<()> {
        sqlx::query("INSERT OR REPLACE INTO wa_outbox (account_id, id, at, chat_jid, kind, status, error, approval_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(account_id)
            .bind(id)
            .bind(fmt(now()))
            .bind(chat_jid)
            .bind(kind)
            .bind(status)
            .bind(error)
            .bind(approval_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Atualiza o status de um envio (sent → delivered → read, ou error).
    pub async fn wa_set_outbox_status(
        &self,
        account_id: &str,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<bool> {
        let r = sqlx::query("UPDATE wa_outbox SET status = ?, error = COALESCE(?, error) WHERE account_id = ? AND id = ?")
            .bind(status)
            .bind(error)
            .bind(account_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Poda global (todas as contas): remove mensagens mais antigas que `days`.
    pub async fn wa_prune(&self, days: i64) -> Result<u64> {
        let cutoff = fmt(now() - chrono::Duration::days(days));
        let r = sqlx::query("DELETE FROM wa_messages WHERE at < ?")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    pub async fn wa_sent_count_since(
        &self,
        account_id: &str,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64> {
        let r = sqlx::query("SELECT COUNT(*) AS c FROM wa_outbox WHERE account_id = ? AND at >= ? AND status != 'error'")
            .bind(account_id)
            .bind(fmt(since))
            .fetch_one(&self.pool)
            .await?;
        Ok(r.get::<i64, _>("c"))
    }

    /// Apaga todo o índice de uma conta.
    pub async fn wa_clear_index(&self, account_id: &str) -> Result<()> {
        for t in [
            "wa_messages",
            "wa_groups",
            "wa_chats",
            "wa_contacts",
            "wa_outbox",
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {t} WHERE account_id = ?"
            )))
            .bind(account_id)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// `{chats, messages, contacts}` de uma conta.
    pub async fn wa_stats(&self, account_id: &str) -> Result<serde_json::Value> {
        let chats = sqlx::query("SELECT COUNT(*) AS c FROM wa_chats WHERE account_id = ?")
            .bind(account_id)
            .fetch_one(&self.pool)
            .await?
            .get::<i64, _>("c");
        let msgs = sqlx::query("SELECT COUNT(*) AS c FROM wa_messages WHERE account_id = ?")
            .bind(account_id)
            .fetch_one(&self.pool)
            .await?
            .get::<i64, _>("c");
        let contacts = sqlx::query("SELECT COUNT(*) AS c FROM wa_contacts WHERE account_id = ?")
            .bind(account_id)
            .fetch_one(&self.pool)
            .await?
            .get::<i64, _>("c");
        let (last, recent) = self.wa_recency(account_id).await?;
        Ok(
            serde_json::json!({"chats": chats, "messages": msgs, "contacts": contacts, "last_message_at": last, "messages_24h": recent}),
        )
    }

    /// Religa nomes entre identidades: contato LID ganha o nome da agenda do
    /// contato de telefone com o mesmo número; chat 1:1 ganha o nome (ou push
    /// name) do seu contato. Devolve quantos chats/contatos mudaram.
    pub async fn wa_relink_names(&self, account_id: &str) -> Result<(u64, u64)> {
        let c = sqlx::query(
            "UPDATE wa_contacts SET name = (SELECT pn.name FROM wa_contacts pn WHERE pn.account_id = wa_contacts.account_id AND pn.phone = wa_contacts.phone AND pn.jid NOT LIKE '%@lid' AND pn.name IS NOT NULL AND pn.name != '' LIMIT 1)              WHERE account_id = ? AND jid LIKE '%@lid' AND (name IS NULL OR name = '') AND phone IS NOT NULL              AND EXISTS (SELECT 1 FROM wa_contacts pn WHERE pn.account_id = wa_contacts.account_id AND pn.phone = wa_contacts.phone AND pn.jid NOT LIKE '%@lid' AND pn.name IS NOT NULL AND pn.name != '')",
        )
        .bind(account_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        let ch = sqlx::query(
            "UPDATE wa_chats SET name = (SELECT COALESCE(NULLIF(c.name, ''), NULLIF(c.push_name, '')) FROM wa_contacts c WHERE c.account_id = wa_chats.account_id AND c.jid = wa_chats.jid)              WHERE account_id = ? AND is_group = 0 AND EXISTS (SELECT 1 FROM wa_contacts c WHERE c.account_id = wa_chats.account_id AND c.jid = wa_chats.jid AND COALESCE(NULLIF(c.name, ''), NULLIF(c.push_name, '')) IS NOT NULL)              AND (name IS NULL OR name = '' OR name = (SELECT c.push_name FROM wa_contacts c WHERE c.account_id = wa_chats.account_id AND c.jid = wa_chats.jid AND c.name IS NOT NULL AND c.name != ''))",
        )
        .bind(account_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok((ch, c))
    }

    /// Última mensagem indexada (fora de `status@broadcast`) e quantas chegaram
    /// nas últimas 24 h: é o que mostra se o índice está acompanhando o celular.
    pub async fn wa_recency(&self, account_id: &str) -> Result<(Option<String>, i64)> {
        let r = sqlx::query(
            "SELECT MAX(at) AS last, SUM(CASE WHEN at >= ? THEN 1 ELSE 0 END) AS recent FROM wa_messages WHERE account_id = ? AND chat_jid != 'status@broadcast'",
        )
        .bind(fmt(now() - chrono::Duration::hours(24)))
        .bind(account_id)
        .fetch_one(&self.pool)
        .await?;
        Ok((
            r.get::<Option<String>, _>("last"),
            r.get::<Option<i64>, _>("recent").unwrap_or(0),
        ))
    }

    /// Estatísticas por conta: `[(account_id, {chats, messages, contacts})]`,
    /// incluindo contas que só existem no índice (ex.: removidas sem limpar).
    pub async fn wa_stats_all(&self) -> Result<Vec<(String, serde_json::Value)>> {
        let rows = sqlx::query(
            "SELECT account_id, SUM(chats) AS chats, SUM(messages) AS messages, SUM(contacts) AS contacts FROM ( \
               SELECT account_id, COUNT(*) AS chats, 0 AS messages, 0 AS contacts FROM wa_chats GROUP BY account_id \
               UNION ALL SELECT account_id, 0, COUNT(*), 0 FROM wa_messages GROUP BY account_id \
               UNION ALL SELECT account_id, 0, 0, COUNT(*) FROM wa_contacts GROUP BY account_id \
             ) GROUP BY account_id ORDER BY account_id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows.iter() {
            let id: String = r.get("account_id");
            let (last, recent) = self.wa_recency(&id).await?;
            out.push((
                id,
                serde_json::json!({"chats": r.get::<i64, _>("chats"), "messages": r.get::<i64, _>("messages"), "contacts": r.get::<i64, _>("contacts"), "last_message_at": last, "messages_24h": recent}),
            ));
        }
        Ok(out)
    }
}

/// Lê o mapa LID → telefone do store da biblioteca `whatsapp-rust`
/// (`<data_dir>/whatsapp/<conta>/store.db`, tabela `lid_pn_mapping`), somente
/// leitura. Sem esse mapa os contatos novos ficam sem telefone e sem nome.
pub async fn read_lid_mappings(store_db: &std::path::Path) -> Result<Vec<(String, String)>> {
    if !store_db.is_file() {
        return Ok(vec![]);
    }
    // Sem `read_only`: o store usa WAL e o modo somente-leitura falha sem o `-shm`.
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(store_db)
        .create_if_missing(false)
        .busy_timeout(std::time::Duration::from_secs(3));
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await?;
    let rows = sqlx::query("SELECT lid, phone_number FROM lid_pn_mapping")
        .fetch_all(&pool)
        .await
        .unwrap_or_default();
    pool.close().await;
    Ok(rows
        .iter()
        .map(|r| {
            (
                r.get::<String, _>("lid"),
                r.get::<String, _>("phone_number"),
            )
        })
        .filter(|(l, p)| !l.is_empty() && !p.is_empty())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(account: &str, id: &str, chat: &str, text: &str) -> WaMessage {
        WaMessage {
            account_id: account.into(),
            id: id.into(),
            chat_jid: chat.into(),
            sender_jid: Some(chat.into()),
            sender_name: None,
            from_me: false,
            at: now(),
            kind: "text".into(),
            text: Some(text.into()),
            media_ref: None,
            media_path: None,
            quoted_id: None,
            read: false,
        }
    }

    #[tokio::test]
    async fn index_is_partitioned_by_account() {
        let st = Storage::open_memory().await.unwrap();
        let chat = "5511@s.whatsapp.net";
        st.wa_upsert_chat("a", chat, Some("Ana"), false)
            .await
            .unwrap();
        st.wa_upsert_chat("b", chat, Some("Ana (b)"), false)
            .await
            .unwrap();
        st.wa_insert_message(&msg("a", "m1", chat, "orçamento reforma"))
            .await
            .unwrap();
        st.wa_insert_message(&msg("b", "m1", chat, "outra coisa"))
            .await
            .unwrap();
        st.wa_insert_message(&msg("b", "m2", chat, "orçamento viagem"))
            .await
            .unwrap();

        assert_eq!(st.wa_list_chats("a", "all", 10).await.unwrap().len(), 1);
        assert_eq!(
            st.wa_get_messages("b", chat, 10, None, None)
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            st.wa_get_message("a", "m1")
                .await
                .unwrap()
                .unwrap()
                .text
                .as_deref(),
            Some("orçamento reforma")
        );
        assert_eq!(
            st.wa_search_messages("a", "orçamento", None, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            st.wa_search_messages("b", "orçamento", None, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            st.wa_get_chat("b", chat)
                .await
                .unwrap()
                .unwrap()
                .unread_count,
            2
        );

        st.wa_mark_read("b", chat).await.unwrap();
        assert_eq!(
            st.wa_get_chat("a", chat)
                .await
                .unwrap()
                .unwrap()
                .unread_count,
            1
        );

        let all = st.wa_stats_all().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0, "a");
        assert_eq!(all[0].1["messages"], 1);
        assert_eq!(all[1].1["messages"], 2);

        st.wa_clear_index("a").await.unwrap();
        assert_eq!(st.wa_stats("a").await.unwrap()["messages"], 0);
        assert_eq!(st.wa_stats("b").await.unwrap()["messages"], 2);
        // FTS continua coerente após a limpeza
        assert_eq!(
            st.wa_search_messages("b", "viagem", None, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
    }
}
