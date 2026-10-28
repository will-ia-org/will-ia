# Documentação do will-ia

| # | Documento | Conteúdo |
|---|---|---|
| 01 | [Visão Geral](01-visao-geral.md) | O que é, problema, princípios, fora de escopo |
| 02 | [Requisitos Funcionais](02-requisitos-funcionais.md) | RF por módulo com prioridade |
| 03 | [Requisitos Não Funcionais](03-requisitos-nao-funcionais.md) | Plataforma, desempenho, confiabilidade, segurança, usabilidade |
| 04 | [Arquitetura](04-arquitetura.md) | C4, workspace, componentes, fluxos, concorrência |
| 05 | [Motor Claude Code](05-motor-claude-code.md) | Processo vivo por thread, stream-json, steering mid-turn, supervisão, tokens |
| 06 | [Base de Conhecimento](06-base-de-conhecimento.md) | Esqueleto da pasta, CLAUDE.md da KB, subagentes, `tools/` |
| 07 | [Voz (Realtime)](07-realtime-voz.md) | WebSocket, PTT/toggle, `ask_brain`, handoff, barge-in |
| 08 | [Telegram](08-telegram.md) | Tópicos, thread key, pilha de status, comandos, portado da referência |
| 09 | [Servidor MCP](09-servidor-mcp.md) | Streamable HTTP, bearer por processo, ferramentas |
| 10 | [Configurações](10-configuracoes.md) | Seções da janela nativa (egui), esquema, aplicação a quente, onboarding |
| 11 | [Persistência](11-persistencia.md) | SQLite, keyring, esquema, retenção |
| 12 | [Segurança](12-seguranca.md) | Modelo de ameaças, regras, modo bypass |
| 13 | [Plataformas e Distribuição](13-plataformas-e-distribuicao.md) | Windows/macOS/Linux, binário por SO, empacotamento, CI/CD (planejado) |
| 14 | [Boas Práticas](14-boas-praticas.md) | Rust, UI egui, git, checklist de revisão |
| 15 | [Roadmap](15-roadmap.md) | Fases 0–4 e backlog |
| 16 | [Glossário](16-glossario.md) | Termos do projeto |
| 17 | [WhatsApp](17-whatsapp.md) | Pareamento por QR, índice local, ferramentas `whatsapp_*` (só ferramentas, não canal) |
| 18 | [Conta Google](18-conta-google.md) | OAuth Client do usuário, PKCE + loopback, escopos incrementais |
| 19 | [Google Agenda](19-google-agenda.md) | Multi-seleção de agendas, ferramentas `calendar_*` |
| 20 | [Gmail](20-gmail.md) | Labels, arquivado, busca, leitura e envio, ferramentas `gmail_*` |
| 21 | [MCPs externos](21-mcps-externos.md) | Catálogo de MCPs de terceiros, `.mcp.json` gerado na KB, segredos via `${VAR}` |
| 22 | [API local](22-api-local.md) | REST/WS interno (headless, automação, segunda instância); não serve UI |
| 23 | [Guia de teste](23-guia-de-teste.md) | Como compilar, rodar e testar cada módulo |
| 24 | [Agendamentos](24-scheduler.md) | Scheduler: cron/único por thread, laço de disparo, recriação da thread dona, ferramentas `schedule_*` |
| 25 | [Browser CDP](25-browser-cdp.md) | Proxy local do Chrome DevTools Protocol para o agent-browser: `AGENT_BROWSER_CDP`, lançamento do navegador sob demanda, log de tráfego, instalação do CLI e da skill |
| 26 | [Separação cliente/servidor](26-cliente-servidor.md) | Fase 5 (planejada): servidor residente + app cliente HTTP/WS, contas, voz por usuário, impactos e etapas |
| — | [ADRs](adr/README.md) | Decisões arquiteturais |

## Projeto de referência

O módulo Telegram e o modelo de processo vivo por thread derivam de um bot Node.js em produção (`telegram-bot.zip`, fornecido pelo autor; **não versionado** por conter segredos). Pontos herdados estão marcados nos docs 05 e 08.
