-- Etiquetas das tarefas (docs/31): JSON de strings já normalizadas
-- (`will_core::task::normalize_tag`), ex.: ["devmania","holding"].
ALTER TABLE tasks ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';
