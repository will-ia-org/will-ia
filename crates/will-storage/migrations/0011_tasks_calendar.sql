-- Sincronização Tarefas ↔ Google Calendar (docs/31): cada tarefa pode estar
-- espelhada num evento (`calendar_id` + `event_id`); `synced_at` é a última
-- sincronização no relógio local (edição local = `updated_at > synced_at`) e
-- `remote_updated_at` é o `updated` do evento visto nela (edição remota = diferente).
-- `task_tombstones` guarda eventos de tarefas apagadas cuja exclusão no Google
-- falhou na hora, para a próxima sincronização tentar de novo (e não reimportar).
ALTER TABLE tasks ADD COLUMN calendar_id TEXT;
ALTER TABLE tasks ADD COLUMN event_id TEXT;
ALTER TABLE tasks ADD COLUMN synced_at TEXT;
ALTER TABLE tasks ADD COLUMN remote_updated_at TEXT;
CREATE INDEX IF NOT EXISTS tasks_event ON tasks(calendar_id, event_id);
CREATE TABLE IF NOT EXISTS task_tombstones (
  calendar_id TEXT NOT NULL,
  event_id TEXT NOT NULL,
  deleted_at TEXT NOT NULL,
  PRIMARY KEY (calendar_id, event_id)
);
