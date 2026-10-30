-- Debounce por trigger (docs/30): segundos sem mensagem nova (por conversa) antes
-- de disparar uma vez com todas as mensagens acumuladas. 0 = imediato.
ALTER TABLE triggers ADD COLUMN debounce_secs INTEGER NOT NULL DEFAULT 0;
