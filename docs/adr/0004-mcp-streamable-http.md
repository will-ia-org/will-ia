# ADR-0004 — Servidor MCP via Streamable HTTP em localhost

**Status:** Aceita · **Data:** 2026-10-19

## Contexto
Os agentes precisam de ferramentas do serviço (enviar Telegram, notificar). Alternativas: servidor MCP stdio (o binário se re-executa como filho de cada `claude`) ou HTTP único.

## Decisão
Um servidor Streamable HTTP (`rmcp` + `axum`) em `127.0.0.1`, com bearer token efêmero por processo, que identifica a thread chamadora. A entrega ao Claude Code é via `<kb>/.mcp.json` gerado (header `Bearer ${WILL_IA_MCP_TOKEN}` expandido por spawn), ver ADR-0014.

## Consequências
- (+) Um só servidor, com acesso direto ao estado do orquestrador (sem IPC extra); identidade da thread por token; scripts em `tools/` também podem usar.
- (+) Toggles de ferramentas refletidos por `tools/list_changed`.
- (−) Porta local a gerenciar; precisa autenticação (resolvido pelo bearer). stdio fica como alternativa futura se o HTTP se mostrar problemático em algum SO.
