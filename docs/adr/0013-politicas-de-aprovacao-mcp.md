# ADR-0013 — Políticas de aprovação para ferramentas MCP com efeito externo

**Status:** Aceita · **Data:** 2026-10-19

## Contexto
Com WhatsApp, Gmail e Agenda, o agente passa a poder enviar mensagens e e-mails e criar/alterar compromissos em nome do usuário. Prompt injection (conteúdo de e-mails, mensagens, páginas) pode induzir ações indesejadas. Os agentes rodam headless, sem humano no terminal.

## Decisão
Cada ferramenta MCP tem uma **política** configurável: `auto` (executa), `confirmar` (aguarda aprovação humana), `desligada` (não listada). Ferramentas de leitura são `auto`; as de efeito externo (`*_send*`, `calendar_create/update/delete_event`, `gmail_modify`) nascem em `confirmar`. A aprovação é pedida por notificação nativa + card na UI e, se o Telegram estiver ativo, mensagem com botões inline ao dono; a chamada MCP bloqueia até decisão ou timeout (padrão 5 min → negada). Decisões ficam em `approvals`. O usuário pode baixar para `auto` por ferramenta, com aviso.

## Consequências
- (+) Camada de defesa independente do prompt; auditoria completa; funciona para qualquer módulo futuro.
- (−) Latência em ações externas quando o usuário não está presente; turnos podem ficar aguardando. Mitigado pelo timeout e pela opção de `auto` por ferramenta.
