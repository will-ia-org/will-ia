-- Tarefa fechada como "não realizada" (docs/31): `done_at` preenchido + skipped = 1.
-- Some da tela como a concluída, mas registra que não foi feita.
ALTER TABLE tasks ADD COLUMN skipped INTEGER NOT NULL DEFAULT 0;
