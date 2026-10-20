CREATE TABLE IF NOT EXISTS settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  schema_version INTEGER NOT NULL,
  json TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS threads (
  id TEXT PRIMARY KEY,
  origin TEXT NOT NULL,
  origin_ref TEXT,
  title TEXT,
  claude_session_id TEXT,
  status TEXT NOT NULL DEFAULT 'idle',
  created_at TEXT NOT NULL,
  last_activity_at TEXT NOT NULL,
  total_cost_usd REAL NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS threads_origin_ref ON threads(origin, origin_ref);

CREATE TABLE IF NOT EXISTS turns (
  id TEXT PRIMARY KEY,
  thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  prompt TEXT NOT NULL,
  result_text TEXT,
  is_error INTEGER NOT NULL DEFAULT 0,
  error_kind TEXT,
  cost_usd REAL,
  duration_ms INTEGER,
  model TEXT,
  usage_json TEXT
);
CREATE INDEX IF NOT EXISTS turns_thread ON turns(thread_id, started_at);

CREATE TABLE IF NOT EXISTS turn_events (
  id INTEGER PRIMARY KEY,
  turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
  at TEXT NOT NULL,
  kind TEXT NOT NULL,
  payload TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS turn_events_turn ON turn_events(turn_id, id);

CREATE TABLE IF NOT EXISTS telegram_state (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  update_offset INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS telegram_status (
  thread_key TEXT PRIMARY KEY,
  thread_id TEXT REFERENCES threads(id) ON DELETE CASCADE,
  stack_json TEXT NOT NULL DEFAULT '[]',
  status_message_id INTEGER,
  last_user_message_id INTEGER
);

CREATE TABLE IF NOT EXISTS mcp_calls (
  id INTEGER PRIMARY KEY,
  at TEXT NOT NULL,
  thread_id TEXT,
  tool TEXT NOT NULL,
  args_redacted TEXT,
  ok INTEGER NOT NULL,
  error TEXT,
  duration_ms INTEGER
);
CREATE INDEX IF NOT EXISTS mcp_calls_at ON mcp_calls(at);

CREATE TABLE IF NOT EXISTS tool_policies (
  tool TEXT PRIMARY KEY,
  policy TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS approvals (
  id TEXT PRIMARY KEY,
  requested_at TEXT NOT NULL,
  thread_id TEXT,
  tool TEXT NOT NULL,
  summary TEXT NOT NULL,
  args_redacted TEXT,
  decided_at TEXT,
  decision TEXT,
  decided_via TEXT
);

CREATE TABLE IF NOT EXISTS mcp_json_state (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  last_generated_hash TEXT,
  last_generated_at TEXT
);

CREATE TABLE IF NOT EXISTS google_accounts (
  id TEXT PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  secret_ref TEXT NOT NULL,
  scopes TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'ok',
  connected_at TEXT NOT NULL,
  last_used_at TEXT
);

CREATE TABLE IF NOT EXISTS google_calendars (
  account_id TEXT NOT NULL REFERENCES google_accounts(id) ON DELETE CASCADE,
  calendar_id TEXT NOT NULL,
  summary TEXT NOT NULL,
  timezone TEXT,
  access_role TEXT,
  color TEXT,
  enabled INTEGER NOT NULL DEFAULT 0,
  default_for_create INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (account_id, calendar_id)
);

CREATE TABLE IF NOT EXISTS wa_contacts (
  jid TEXT PRIMARY KEY,
  phone TEXT,
  name TEXT,
  push_name TEXT,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS wa_chats (
  jid TEXT PRIMARY KEY,
  name TEXT,
  is_group INTEGER NOT NULL DEFAULT 0,
  archived INTEGER NOT NULL DEFAULT 0,
  unread_count INTEGER NOT NULL DEFAULT 0,
  last_message_at TEXT,
  last_message_preview TEXT
);
CREATE TABLE IF NOT EXISTS wa_groups (
  jid TEXT PRIMARY KEY REFERENCES wa_chats(jid) ON DELETE CASCADE,
  subject TEXT,
  description TEXT,
  participants_json TEXT,
  admins_json TEXT,
  updated_at TEXT
);
CREATE TABLE IF NOT EXISTS wa_messages (
  id TEXT PRIMARY KEY,
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
  read INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS wa_messages_chat_at ON wa_messages(chat_jid, at);
CREATE VIRTUAL TABLE IF NOT EXISTS wa_messages_fts USING fts5(text, content='wa_messages', content_rowid='rowid');
CREATE TRIGGER IF NOT EXISTS wa_messages_ai AFTER INSERT ON wa_messages BEGIN
  INSERT INTO wa_messages_fts(rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TRIGGER IF NOT EXISTS wa_messages_ad AFTER DELETE ON wa_messages BEGIN
  INSERT INTO wa_messages_fts(wa_messages_fts, rowid, text) VALUES('delete', old.rowid, old.text);
END;
CREATE TRIGGER IF NOT EXISTS wa_messages_au AFTER UPDATE ON wa_messages BEGIN
  INSERT INTO wa_messages_fts(wa_messages_fts, rowid, text) VALUES('delete', old.rowid, old.text);
  INSERT INTO wa_messages_fts(rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TABLE IF NOT EXISTS wa_outbox (
  id TEXT PRIMARY KEY,
  at TEXT NOT NULL,
  chat_jid TEXT NOT NULL,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  error TEXT,
  approval_id TEXT
);

CREATE TABLE IF NOT EXISTS events_log (
  id INTEGER PRIMARY KEY,
  at TEXT NOT NULL,
  level TEXT NOT NULL,
  module TEXT NOT NULL,
  message TEXT NOT NULL,
  fields_json TEXT
);
CREATE INDEX IF NOT EXISTS events_log_at ON events_log(at);

CREATE TABLE IF NOT EXISTS voice_transcripts (
  id INTEGER PRIMARY KEY,
  at TEXT NOT NULL,
  role TEXT NOT NULL,
  text TEXT NOT NULL
);
