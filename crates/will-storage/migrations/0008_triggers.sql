-- Triggers universais (docs/30): fonte de evento (whatsapp, gmail) + filtro +
-- thread/prompt. `thread_id` NULL = criar na primeira vez; sem FK (a thread
-- pode ser apagada e recriada). `filter_json` = will_core::trigger::TriggerFilter.
CREATE TABLE IF NOT EXISTS triggers (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  source TEXT NOT NULL,                -- whatsapp | gmail
  enabled INTEGER NOT NULL DEFAULT 1,
  filter_json TEXT NOT NULL DEFAULT '{}',
  prompt TEXT NOT NULL,
  thread_id TEXT,
  thread_title TEXT,
  claude_session_id TEXT,
  origin TEXT NOT NULL DEFAULT 'ui',
  origin_ref TEXT,
  cooldown_secs INTEGER NOT NULL DEFAULT 0,
  created_by TEXT NOT NULL DEFAULT 'user',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_fired_at TEXT,
  last_result TEXT,
  fire_count INTEGER NOT NULL DEFAULT 0,
  paused_reason TEXT,
  cursor TEXT
);
CREATE INDEX IF NOT EXISTS triggers_source ON triggers(source, enabled);

CREATE TABLE IF NOT EXISTS trigger_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  trigger_id TEXT NOT NULL REFERENCES triggers(id) ON DELETE CASCADE,
  at TEXT NOT NULL,
  thread_id TEXT,
  ok INTEGER NOT NULL,
  detail TEXT,
  input TEXT
);
CREATE INDEX IF NOT EXISTS trigger_runs_trig ON trigger_runs(trigger_id, at);
