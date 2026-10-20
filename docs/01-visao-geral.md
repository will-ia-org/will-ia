# 01 — Visão Geral

## O que é o will-ia

O **will-ia** é um assistente pessoal de IA que roda como aplicação de bandeja (tray) em Windows, Linux e macOS. Ele não possui inteligência própria: ele **orquestra** dois motores externos e os conecta a canais de entrada/saída.

| Papel | Tecnologia | Função |
|---|---|---|
| **Cérebro** | Claude Code CLI em modo headless (`claude -p`) | Pensa, pesquisa, lê e escreve arquivos na base de conhecimento, executa scripts. Cada conversa é uma *thread* com contexto próprio. |
| **Voz** | OpenAI Realtime API (WebSocket) | Conversa por voz em tempo real com o usuário. Delega perguntas difíceis ao cérebro via function calling. |
| **Mensageria** | Bot do Telegram em supergrupo com tópicos | Cada tópico é uma thread do cérebro. Permite conversar com o assistente de qualquer lugar. |
| **Ferramentas** | Servidor MCP local (Streamable HTTP) | Expõe capacidades do próprio serviço (ex.: enviar mensagem no Telegram) para os agentes Claude Code. |

## Problema que resolve

Hoje, usar o Claude Code como "segundo cérebro" exige estar na frente de um terminal. O will-ia transforma isso em um serviço sempre ativo que:

1. Mantém uma **base de conhecimento** (pasta de Markdown, documentos e scripts) que cresce ao longo do tempo, editada pelos próprios agentes.
2. Pode ser acionado por **voz** (atalho global ou push-to-talk) com latência de conversa natural.
3. Pode ser acionado por **Telegram**, de qualquer dispositivo, com threads paralelas isoladas por tópico.
4. Permite que os agentes **ajam no mundo** através de ferramentas MCP que o serviço expõe.

## Princípios de design

- **O serviço é um orquestrador, não um LLM.** Toda a inteligência está no Claude Code e no Realtime. O código Rust cuida de processos, canais, estado e segurança.
- **A base de conhecimento é a fonte de verdade.** Comportamento dos agentes, scripts e memória de longo prazo vivem na pasta da base de conhecimento, não no binário. O serviço só aponta para ela.
- **Threads são a unidade de contexto.** Toda interação (UI, voz, Telegram) acontece dentro de uma thread, que mapeia 1:1 para uma sessão do Claude Code.
- **Local-first e seguro por padrão.** MCP só escuta em localhost, segredos ficam no keyring do SO, Telegram exige allowlist, permissões do agente são configuráveis.
- **Multiplataforma de verdade.** Um único código-base Rust (UI nativa com egui/eframe, sem WebView nem Node) e um binário por SO nos três sistemas.

## Fora de escopo (por enquanto)

- Versionamento automático (git) da base de conhecimento.
- Múltiplos usuários/perfis no mesmo serviço.
- Interface web remota (a UI é um app desktop local; a API HTTP em `127.0.0.1` é contrato interno/headless, não serve páginas).
- Sincronização da base de conhecimento entre máquinas.
- Rotação automática de tokens do Claude Code (há suporte a vários tokens, mas apenas um ativo por vez).

## Documentos relacionados

- [02 — Requisitos Funcionais](02-requisitos-funcionais.md)
- [03 — Requisitos Não Funcionais](03-requisitos-nao-funcionais.md)
- [04 — Arquitetura](04-arquitetura.md)
- [15 — Roadmap](15-roadmap.md)
