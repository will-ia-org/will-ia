-- Gravações de conversa (docs/32): microfone e saída do sistema em dois canais
-- de um WAV. O áudio e a transcrição ficam em `<dados>/recordings/<id>/`; aqui
-- vai só a ficha.
CREATE TABLE recordings (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  -- quando a gravação começou (relógio de quem gravou)
  started_at TEXT NOT NULL,
  duration_secs REAL NOT NULL DEFAULT 0,
  -- com quem foi a conversa (pessoa, turma ou grupo): rótulo do canal do sistema
  with_whom TEXT NOT NULL DEFAULT '',
  subject TEXT NOT NULL DEFAULT '',
  -- a saída do sistema foi capturada (senão só há a voz de quem gravou)
  has_system INTEGER NOT NULL DEFAULT 1,
  size_bytes INTEGER NOT NULL DEFAULT 0,
  -- saved | transcribing | done | error
  status TEXT NOT NULL DEFAULT 'saved',
  error TEXT,
  created_by TEXT
);
CREATE INDEX recordings_started ON recordings(started_at DESC);
