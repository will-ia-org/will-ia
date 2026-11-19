-- Gravações de conversa (docs/32): pasta com data e hora, resumo e índice de
-- busca semântica.
-- Nome da pasta em `<dados>/recordings/` (`AAAA-MM-DD_HH-MM-SS_<id curto>`).
-- Nulo nas gravações anteriores, que ficam na pasta com o próprio id.
ALTER TABLE recordings ADD COLUMN folder TEXT;
-- Resumo detalhado da conversa (gerado depois da transcrição).
ALTER TABLE recordings ADD COLUMN summary TEXT;

-- Trechos da conversa com o vetor de embedding (busca por assunto).
CREATE TABLE recording_chunks (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  recording_id TEXT NOT NULL,
  -- summary (o resumo) | speech (um trecho da transcrição)
  kind TEXT NOT NULL,
  idx INTEGER NOT NULL,
  start_secs REAL NOT NULL DEFAULT 0,
  end_secs REAL NOT NULL DEFAULT 0,
  -- falas do trecho, uma por linha, com o marcador de quem falou
  text TEXT NOT NULL,
  model TEXT NOT NULL,
  -- vetor f32 little-endian
  embedding BLOB NOT NULL
);
CREATE INDEX recording_chunks_rec ON recording_chunks(recording_id);
