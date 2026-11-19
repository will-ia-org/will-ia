-- Até quando o usuário já leu cada thread. Respostas concluídas depois desse
-- instante contam como não lidas (badge na lista de threads do app).
ALTER TABLE threads ADD COLUMN read_at TEXT;
-- Threads que já existiam começam como lidas.
UPDATE threads SET read_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now');
