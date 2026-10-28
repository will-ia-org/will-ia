-- Agendamentos do scheduler (docs/24): cron recorrente ou data única, ligados à
-- thread que os criou. `origin`/`origin_ref`/`claude_session_id` permitem recriar
-- a thread se ela for apagada. `runs` guarda um histórico curto por agendamento.
CREATE TABLE IF NOT EXISTS schedules (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  message TEXT NOT NULL,
  trigger_kind TEXT NOT NULL,          -- cron | once
  cron_expr TEXT,
  run_at TEXT,
  timezone TEXT,
  thread_id TEXT NOT NULL,             -- sem FK: a thread pode ser apagada e recriada
  claude_session_id TEXT,
  origin TEXT NOT NULL,
  origin_ref TEXT,
  enabled INTEGER NOT NULL DEFAULT 1,
  created_by TEXT NOT NULL DEFAULT 'agent',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  next_run_at TEXT,
  last_run_at TEXT,
  last_result TEXT,
  run_count INTEGER NOT NULL DEFAULT 0,
  paused_reason TEXT
);
CREATE INDEX IF NOT EXISTS schedules_next ON schedules(enabled, next_run_at);
CREATE INDEX IF NOT EXISTS schedules_thread ON schedules(thread_id);

CREATE TABLE IF NOT EXISTS schedule_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  schedule_id TEXT NOT NULL REFERENCES schedules(id) ON DELETE CASCADE,
  at TEXT NOT NULL,
  thread_id TEXT,
  ok INTEGER NOT NULL,
  detail TEXT
);
CREATE INDEX IF NOT EXISTS schedule_runs_sched ON schedule_runs(schedule_id, at);
