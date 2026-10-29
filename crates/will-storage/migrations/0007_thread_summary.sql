-- Resumo automático da conversa (docs/10, "Resumo das threads"): a cada N
-- turnos concluídos o serviço pede a um modelo de chat um resumo curto do que
-- foi conversado por último e guarda aqui. `summary_turns` é a contagem de
-- turnos concluídos no momento do último resumo (ou da última tentativa).
ALTER TABLE threads ADD COLUMN summary TEXT;
ALTER TABLE threads ADD COLUMN summary_turns INTEGER NOT NULL DEFAULT 0;
