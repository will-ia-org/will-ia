-- Custos de IA (genérico): voz (OpenAI Realtime), turnos do Claude Code e, no futuro,
-- transcrição/TTS do Telegram etc. `units_json` guarda as contagens por tipo de token;
-- `usd` é o custo (estimado quando `estimated = 1`, a partir de uma tabela de preços local).
CREATE TABLE IF NOT EXISTS ai_costs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at TEXT NOT NULL,
  source TEXT NOT NULL,          -- voice | claude | telegram_stt | telegram_tts | ...
  ref TEXT,                      -- id da sessão/thread/turno
  model TEXT,
  units_json TEXT NOT NULL DEFAULT '{}',
  usd REAL NOT NULL DEFAULT 0,
  estimated INTEGER NOT NULL DEFAULT 1,
  duration_secs INTEGER,
  note TEXT
);
CREATE INDEX IF NOT EXISTS ai_costs_at ON ai_costs(at);
CREATE INDEX IF NOT EXISTS ai_costs_source_at ON ai_costs(source, at);
