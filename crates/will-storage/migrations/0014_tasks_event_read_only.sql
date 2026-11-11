-- Vínculo com evento especial do Google (aniversário, Gmail, fora do escritório,
-- foco): a agenda não aceita edição, a tarefa só recebe (docs/31, "Sincronizar").
ALTER TABLE tasks ADD COLUMN event_read_only INTEGER NOT NULL DEFAULT 0;
