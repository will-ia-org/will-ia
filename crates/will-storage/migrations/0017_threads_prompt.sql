-- Prompt próprio de cada thread, somado ao prompt geral (Configurações → Motor)
-- no `--append-system-prompt` do `claude`.
ALTER TABLE threads ADD COLUMN prompt TEXT;
-- Impressão digital do prompt de sistema com que a sessão atual nasceu. O
-- `claude` grava o prompt na primeira mensagem; num `--resume` com texto
-- diferente, o serviço desliga esse snapshot para o texto novo valer.
ALTER TABLE threads ADD COLUMN session_prompt_hash TEXT;
