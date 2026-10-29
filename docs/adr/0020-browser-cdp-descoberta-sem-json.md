# ADR-0020 — Descoberta do navegador por `DevToolsActivePort` + síntese de `/json/*` quando o Chrome não tem HTTP de descoberta

**Status:** Aceita · **Data:** 2026-10-28 · Complementa a [ADR-0018](0018-browser-cdp-proxy.md)

## Contexto

O proxy CDP (`will-browser`) só reconhecia um navegador por `GET /json/version` com 2xx. Um Chrome com depuração remota ligada **sem** `--remote-allow-origins` (perfil principal do usuário, ligado por outro app — caso real na máquina do autor, ver `docs/diagnosticos/2026-10-28-browser-cdp-chrome-sem-json-version.md`) responde **404** em todo `/json/*`, mas aceita o WebSocket CDP. Com `localhost` dual-stack, o Chrome lançado pelo módulo caía em `[::1]:9222` enquanto o probe olhava `127.0.0.1:9222`; e o erro do proxy (`502 text/plain`) quebrava o parser JSON do `agent-browser`.

## Decisão

1. **Descoberta em camadas**: HTTP `/json/version` em cada host candidato (`localhost` → `127.0.0.1` e `[::1]`); se a porta está aberta sem HTTP, `DevToolsActivePort` (perfil dedicado sempre; perfis do usuário quando `attach_existing = true`, padrão) validado por `Browser.getVersion`.
2. **Síntese**: sem HTTP no navegador, o proxy responde `/json/version|list|new|activate|close` a partir de `Browser.getVersion`/`Target.*`, por uma **única conexão de controle** reaproveitada.
3. **Porta ocupada por desconhecido → não lançar** outro navegador; erro acionável. Lançamento automático só com a porta livre.
4. **Erros em JSON** (`503`/`502` `{"error","upstream","hint"}`) nos endpoints de descoberta.

## Consequências

- Anexar ao Chrome do usuário preserva logins, mas o Chrome pode pedir "Allow remote debugging?" a cada conexão nova (uma por sessão do `agent-browser` + a de controle). Multiplexar sessões numa conexão upstream única fica como backlog.
- `attach_existing` é opt-out; nos testes automatizados é desligado para não tocar no Chrome real.
- O status ganha `upstream_authority`, `upstream_source`, `upstream_http_discovery`; a UI mostra a fonte.
