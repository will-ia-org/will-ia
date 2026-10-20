# ADR-0005 — SQLite para estado e keyring para segredos

**Status:** Aceita · **Data:** 2026-10-19

## Contexto
Estado: threads, turnos, custos, mapeamentos Telegram, offset, pilhas, logs de MCP. Segredos: tokens Claude, OpenAI, Telegram. A referência usava `state.json` + `tokens.json` (git-crypt).

## Decisão
SQLite (`sqlx`, WAL) no diretório de dados do usuário para todo estado; `keyring` do SO para segredos, com `secret_ref` no banco. Settings como JSON versionado em uma linha.

## Consequências
- (+) Consultas (histórico, custos), transações, migrations; segredos fora do disco em texto plano.
- (−) Linux sem Secret Service exige fallback (arquivo 0600 com aviso). Debounce em memória para escritas frequentes (pilha/offset).
