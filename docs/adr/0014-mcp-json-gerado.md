# ADR-0014 — `.mcp.json` da KB gerado pelo serviço como união de MCPs internos e externos

**Status:** Aceita · **Data:** 2026-10-19 · Complementa a ADR-0004

## Contexto
Além do servidor MCP do próprio will-ia, o usuário quer catalogar MCPs de terceiros (Trello, GitHub…) e disponibilizá-los aos agentes. Alternativas: (a) passar tudo via `--mcp-config` inline por spawn; (b) o serviço fazer proxy dos externos; (c) gerar o `.mcp.json` de projeto na pasta da KB com a união.

## Decisão
(c). O serviço é dono do `<kb>/.mcp.json`: escreve a união (`will_ia` + externos habilitados), usa expansão `${VAR}` para o bearer por thread e para todos os segredos, injeta as variáveis no ambiente de cada `claude -p`, e passa o mesmo arquivo em `--mcp-config` (+ `enableAllProjectMcpServers` em `settings.local.json`) para não depender de aprovação interativa. Edições manuais são importadas, não sobrescritas. Sem proxy: o Claude Code fala direto com os externos.

## Consequências
- (+) Um só arquivo, no formato nativo, visível ao usuário e útil também em sessões interativas; segredos fora do disco; identidade por thread mantida.
- (+) Catálogo testável na UI; importação do `~/.claude.json`.
- (−) Políticas de aprovação não se aplicam a externos (só allowlist por ferramenta do Claude Code); proxy opcional fica no backlog.
- (−) Regeneração precisa de cuidado com edições manuais (hash + importação).
