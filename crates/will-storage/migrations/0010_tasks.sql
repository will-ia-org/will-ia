-- Tarefas (docs/31): início obrigatório com hora, fim opcional, prioridade
-- (1 alta, 2 média, 3 baixa) e conclusão. Estado (futura/em andamento/atrasada)
-- é derivado do relógio; `notified_*` evita avisar duas vezes no Telegram.
CREATE TABLE IF NOT EXISTS tasks (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  description TEXT NOT NULL DEFAULT '',
  start_at TEXT NOT NULL,
  end_at TEXT,
  done_at TEXT,
  priority INTEGER NOT NULL DEFAULT 2,
  created_by TEXT NOT NULL DEFAULT 'user',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  notified_start_at TEXT,
  notified_overdue_at TEXT
);
CREATE INDEX IF NOT EXISTS tasks_start ON tasks(done_at, start_at);
