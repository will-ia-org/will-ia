-- Índice do WhatsApp por conta pareada (múltiplas contas).
-- Recria as tabelas wa_* com `account_id` e chaves compostas. Dados do índice
-- anterior (conta única) são descartados: o índice é reconstruído pelo
-- history sync ao reconectar.

DROP TRIGGER IF EXISTS wa_messages_ai;
DROP TRIGGER IF EXISTS wa_messages_ad;
DROP TRIGGER IF EXISTS wa_messages_au;
DROP TABLE IF EXISTS wa_messages_fts;
DROP TABLE IF EXISTS wa_outbox;
DROP TABLE IF EXISTS wa_messages;
DROP TABLE IF EXISTS wa_groups;
DROP TABLE IF EXISTS wa_chats;
DROP TABLE IF EXISTS wa_contacts;

CREATE TABLE wa_contacts (
  account_id TEXT NOT NULL,
  jid TEXT NOT NULL,
  phone TEXT,
  name TEXT,
  push_name TEXT,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (account_id, jid)
);
CREATE TABLE wa_chats (
  account_id TEXT NOT NULL,
  jid TEXT NOT NULL,
  name TEXT,
  is_group INTEGER NOT NULL DEFAULT 0,
  archived INTEGER NOT NULL DEFAULT 0,
  unread_count INTEGER NOT NULL DEFAULT 0,
  last_message_at TEXT,
  last_message_preview TEXT,
  PRIMARY KEY (account_id, jid)
);
CREATE TABLE wa_groups (
  account_id TEXT NOT NULL,
  jid TEXT NOT NULL,
  subject TEXT,
  description TEXT,
  participants_json TEXT,
  admins_json TEXT,
  updated_at TEXT,
  PRIMARY KEY (account_id, jid),
  FOREIGN KEY (account_id, jid) REFERENCES wa_chats(account_id, jid) ON DELETE CASCADE
);
CREATE TABLE wa_messages (
  account_id TEXT NOT NULL,
  id TEXT NOT NULL,
  chat_jid TEXT NOT NULL,
  sender_jid TEXT,
  sender_name TEXT,
  from_me INTEGER NOT NULL DEFAULT 0,
  at TEXT NOT NULL,
  kind TEXT NOT NULL,
  text TEXT,
  media_ref TEXT,
  media_path TEXT,
  quoted_id TEXT,
  read INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (account_id, id)
);
CREATE INDEX wa_messages_account_chat_at ON wa_messages(account_id, chat_jid, at);
CREATE VIRTUAL TABLE wa_messages_fts USING fts5(text, content='wa_messages', content_rowid='rowid');
CREATE TRIGGER wa_messages_ai AFTER INSERT ON wa_messages BEGIN
  INSERT INTO wa_messages_fts(rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TRIGGER wa_messages_ad AFTER DELETE ON wa_messages BEGIN
  INSERT INTO wa_messages_fts(wa_messages_fts, rowid, text) VALUES('delete', old.rowid, old.text);
END;
CREATE TRIGGER wa_messages_au AFTER UPDATE ON wa_messages BEGIN
  INSERT INTO wa_messages_fts(wa_messages_fts, rowid, text) VALUES('delete', old.rowid, old.text);
  INSERT INTO wa_messages_fts(rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TABLE wa_outbox (
  account_id TEXT NOT NULL,
  id TEXT NOT NULL,
  at TEXT NOT NULL,
  chat_jid TEXT NOT NULL,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  error TEXT,
  approval_id TEXT,
  PRIMARY KEY (account_id, id)
);
