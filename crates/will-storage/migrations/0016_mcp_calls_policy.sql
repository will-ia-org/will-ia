-- Como cada chamada de ferramenta foi liberada (docs/09): auto, auto_listed
-- (todos os destinatários na lista de confiança), confirm ou off.
ALTER TABLE mcp_calls ADD COLUMN policy TEXT;
