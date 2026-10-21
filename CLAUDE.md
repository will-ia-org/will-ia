# CLAUDE.md — memória do projeto will-ia

Este arquivo orienta o assistente de código que trabalha **neste repositório** (o serviço). Não confundir com o `CLAUDE.md` da **base de conhecimento** do usuário, que é gerado pelo serviço e vive em outra pasta (ver `docs/06`).

## O que é o projeto

Serviço de bandeja multiplataforma (Windows/Linux/macOS) em **Rust + egui/eframe** (app desktop nativo, binário único `will-ia`; ADR-0016) que orquestra:
- **Claude Code CLI** em modo headless como cérebro (`claude -p --input-format stream-json --output-format stream-json`), **um processo vivo por thread**, rodando sobre a pasta "base de conhecimento" do usuário;
- **OpenAI Realtime** (WebSocket) para voz, delegando ao cérebro via função `ask_brain`;
- **Bot do Telegram** em supergrupo com tópicos (1 tópico = 1 thread);
- **Servidor MCP** local (Streamable HTTP, `127.0.0.1`) com ferramentas para os agentes: `telegram_*`, `whatsapp_*`, `calendar_*`, `gmail_*`, `notify_user`, cada uma com política `auto` / `confirmar` / `desligada`;
- **WhatsApp pessoal** do dono pareado por QR (`whatsapp-rust`), **só ferramentas**, não é canal de entrada;
- **MCPs externos** catalogados na UI; o serviço gera `<kb>/.mcp.json` = `will_ia` + externos, segredos como `${WILL_MCP_*}` injetados por spawn (nunca proxy);
- **Conta Google** (OAuth Client do próprio usuário, PKCE + loopback) para **Agenda** (multi-seleção de agendas, ler/criar) e **Gmail** (labels, arquivado, busca, leitura, envio).

Documentação completa em `docs/` (índice em `docs/README.md`). **Leia o doc do módulo antes de mexer nele.**

## Regras inegociáveis

1. **Nunca chamar a API da Anthropic diretamente.** Toda inteligência passa pelo binário `claude`. Não adicionar `anthropic`/SDKs da Anthropic ao projeto.
2. **Mensagens vão para o stdin do processo na hora**, inclusive durante um turno em andamento (steering). Nenhum caminho de código pode "esperar o turno acabar" para entregar uma mensagem do usuário.
3. **Segredos** (tokens Claude, OpenAI, Telegram) só no keyring, tipados como `SecretString`; nunca em settings JSON, logs, mensagens de erro ou chat. Logs mostram no máximo `…últimos 4`.
4. **MCP só em loopback** com bearer efêmero por processo. Ferramentas só enviam para destinos da allowlist.
5. **Telegram deny-by-default**: chat **e** user allowlist; fora disso só `/start` responde.
6. O serviço **não toca na base de conhecimento** além de criar o esqueleto inicial e não roda git nela.
7. Falha em um módulo nunca derruba o app: módulo vira `Degraded` com motivo.
8. Processos filhos têm kill em cascata (grupo de processos em Unix, Job Object no Windows) e são encerrados no shutdown.
9. **Ferramentas com efeito externo** (`*_send*`, `gmail_modify`, `calendar_create/update/delete_event`) nascem com política `confirmar`; o resumo mostrado na aprovação é gerado pelo serviço, nunca pelo agente.
10. **WhatsApp e Google não criam threads.** Mensagens/e-mails recebidos são dados para ferramentas, não gatilhos.
11. **Conteúdo lido** (e-mails, mensagens, páginas) é dado não confiável; nada dele vira instrução para o serviço.
12. **`.mcp.json` da KB é gerado pelo serviço**: nunca gravar segredos literais nele; usar `${VAR}` + env por spawn; edições manuais são importadas, não sobrescritas.
13. Google: nunca embutir `client_id`/`client_secret` nossos; o usuário fornece. Sem crates `google-*` gerados: REST direto com `reqwest`.

## Arquitetura em uma tela

```
crates/will-desktop (egui: janela, bandeja, áudio cpal, hotkey; modos `will-ia` | `--show` | `serve`) ─► crates/will-server (AppState, Host=ThreadHost+BrainHost, API axum + WS local, .mcp.json, módulos)
                                              └─► crates/will-core (tipos, Event/EventBus, Settings, WillTool/ToolProvider, ApprovalManager, ThreadHost/BrainHost)
                                                         ├── will-engine   (runner por thread: spawn, stdin aberto, parser stream-json, fila de pendências, retire/teto/idle, sessão expirada, troca de token)
                                                         ├── will-telegram (long polling, thread key, pilha de status, comandos, anexos)
                                                         ├── will-realtime (WS OpenAI, cpal, PTT/toggle, ask_brain handoff)
                                                         ├── will-mcp      (rmcp + axum, ToolRegistry, políticas/aprovações, bearer → thread)
                                                         ├── will-whatsapp (whatsapp-rust atrás de WhatsAppBackend, índice SQLite+FTS, tools whatsapp_*)
                                                         ├── will-google   (oauth2 PKCE/loopback, GoogleClient reqwest, calendar/ e gmail/, tools calendar_* gmail_*)
                                                         ├── will-storage  (sqlx SQLite WAL, keyring, repos)
                                                         └── will-platform (autostart, hotkey, notificações, paths por SO)
```

Dependências: módulos → `will-core` + `will-storage`; `will-core` não depende de nada interno. Módulos de ferramentas implementam `ToolProvider` e são descobertos pelo `will-mcp`. UI (egui, modo imediato sobre snapshot de `AppState` + ações assíncronas) não tem lógica de negócio. A API HTTP local (`docs/22`) é contrato interno/headless; não serve UI.

## Conceitos-chave (ver `docs/16-glossario.md`)

- **Thread** = processo `claude` (vivo ou dormente) + `claude_session_id` + origem (`ui`/`telegram`/`realtime`).
- **Pendência** = mensagem escrita no stdin ainda sem `result`; cada `result` fecha a cabeça da fila da thread e sabe para onde responder.
- **Retire** = matar processo mantendo sessão (volta com `--resume`). **Reset** = matar e esquecer sessão.
- **Sessão expirada** = stderr/`result.errors` com `No conversation found with session ID` → engolir o result, esquecer sessão, respawnar e reenviar pendências (uma vez).
- **Thread key** (Telegram) = `<chat_id>` ou `<chat_id>:<message_thread_id>`.
- **Política de ferramenta** = `auto` / `confirmar` / `desligada`; **Aprovação** = pedido bloqueante com timeout (5 min → negada), decidido na UI, notificação ou Telegram.
- **Módulo de ferramentas** (WhatsApp, Google) ≠ **canal** (UI, Telegram, voz): só canais criam threads.
- Gmail: **arquivado** = sem label `INBOX`; **não lida** = com `UNREAD`. As ferramentas já devolvem esses booleanos.
- Google OAuth: orientar o usuário a **publicar o app** no Google Cloud; em modo teste o refresh token expira em 7 dias.

## Flags do `claude` que usamos (CLI 2.1.303)

`-p --input-format stream-json --output-format stream-json --verbose --replay-user-messages [--include-partial-messages] --append-system-prompt … --session-id <uuid> | --resume <id> --mcp-config <kb>/.mcp.json [--strict-mcp-config] --permission-prompts none` e, conforme configuração, `--permission-mode acceptEdits --allowedTools …` **ou** `--permission-mode bypassPermissions --dangerously-skip-permissions`. Env: `CLAUDE_CODE_OAUTH_TOKEN`, `WILL_IA_THREAD_ID`, `WILL_IA_ORIGIN`, `WILL_IA_MCP_URL`, `WILL_IA_MCP_TOKEN`, `WILL_MCP_<NOME>_<CHAVE>`. Detalhes e tabela de eventos em `docs/05`.

## Projeto de referência

O módulo Telegram e o modelo de runner vêm de um bot Node.js em produção (`telegram-bot.zip`, na raiz, **ignorado pelo git** porque contém token de bot e tokens Claude). Ao implementar `will-telegram`/`will-engine`, consulte `docs/08` e `docs/05`, que destilam o que importa; se precisar do código, extraia o zip em um diretório temporário fora do repo e **nunca copie valores de `tokens.json`, `allowlist.txt` ou o token hardcoded em `bot.mjs`**.

## Convenções

- Rust estável, `cargo fmt`, `cargo clippy -D warnings`, `unsafe_code = "forbid"` fora de `will-platform`.
- Erros com `thiserror` por crate; `anyhow` só no binário; sem `unwrap()` fora de testes.
- `tracing` com spans `thread_id`/`turn_id`/`module`; nunca `println!`.
- Async: tokio; sem `Mutex` através de `.await`; atores + `broadcast` para eventos; `CancellationToken` em toda task.
- IDs `uuid` v7; tempo `chrono::Utc` no banco.
- Commits: Conventional Commits com escopo do crate (`feat(engine): …`).
- Idioma: código e identificadores em **inglês**; docs, comentários explicativos e textos de UI em **pt-BR**.
- Testes: `fake-claude` para o engine, `wiremock` para Telegram/OpenAI, fixtures NDJSON reais anonimizadas para o parser.

## Comandos

```bash
export PATH=$HOME/.cargo/bin:$PATH
# Linux sem libasound2-dev instalado (sem sudo): export PKG_CONFIG_PATH=$HOME/.local/will-ia-sysroot/usr/lib/x86_64-linux-gnu/pkgconfig
cargo build --release -p will-desktop                          # binário target/release/will-ia (app egui + serviço)
target/release/will-ia [--show]                                # serviço + bandeja (+ janela); 2ª execução só mostra a janela da ativa
target/release/will-ia serve [--port N] [--data-dir D] [--file-secrets]   # headless: só serviço + API local
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
scripts/smoke.sh                                               # ponta a ponta com fake-claude (modo serve)
```

## Estado atual e próximos passos

- **2026-10-19:** núcleo, módulos e API compilam em Rust estável 1.99; `cargo test` e clippy `-D warnings` passam; `scripts/smoke.sh` valida ponta a ponta com `fake-claude`.
- **2026-10-20 (ADR-0016):** UI nativa egui/eframe em `crates/will-desktop` **implementada** (Início, Threads/chat com streaming, Aprovações, Logs e todas as seções de configurações); UI React (`ui/`) e casca Tauri (`apps/will-desktop`) removidas. Binário único `will-ia` (GUI + serviço em processo; `serve` = headless; segunda execução reabre a janela via `POST /api/ui/show`). Áudio da voz via `cpal` no app (WS `/api/voice/audio` só no headless); indicador flutuante de voz; bandeja `ksni` (Linux; tenta subir `xapp-sn-watcher` se faltar) / `tray-icon`; `global-hotkey`; `rfd`. Fechar a janela só oculta; "Sair" encerra. WhatsApp com várias contas (QR desenhado na janela, sem aviso de risco). **Modo portátil** (pendrive): `will-ia.db` + `data/` ao lado do binário, segredos em arquivo, KB relativa (`docs/13`, `packaging/portable/`).
- **2026-10-20 (tarde):** UI redesenhada (tipografia, cards, conteúdo centralizado, barra de título própria com janela sem moldura, `Escala da interface`), captura de tela por `will-ia screenshot --nav <pagina>` (`POST /api/ui/screenshot`) para iterar no layout; voz: host PulseAudio do cpal (fones BT/USB), buffers de 30/80 ms, jitter buffer de 250 ms, fila de 10 min, microfone mudo enquanto o assistente fala (`barge_in` opcional), `vad_threshold`, sinais sonoros, teste de mic/tom; ferramentas de voz `list_threads` + `ask_brain{thread}`; **custos**: tabela `ai_costs` (migration 0003), estimativa da voz por tabela de preços, custo dos turnos do Claude, página Custos e `/api/costs`.
- **2026-10-21:** ajuda em ícone "?" com modal (`help.rs`, `ui::section_help`/`heading_help`); i18n pt-BR/en sem crates (`i18n::tr`/`trf`, literal pt-BR é a chave; idioma de Geral aplicado por quadro); semântica das bolinhas de módulo (verde = pronto mesmo ocioso, amarelo = atenção com motivo, cinza = desligado) com recálculo ao mudar conta/sessão/config e sincronização dos eventos de status para o `AppState`; chat com "há X min" + ícone "i" e mensagens enviadas no meio do turno agrupadas ao balão inicial (`merged_into`, sem texto "(incorporada…)"); anexos do Telegram gravados no turno (`turns.attachments_json`, migration 0004; `Attachment`/`attachment_note`/`split_attachment_notes` no core) e exibidos no chat como miniatura/chip, Aprovações em cards + tabela, Logs estilo terminal. Detalhes em `docs/10`.
- **Validado nesta máquina:** compila com todas as features, clippy `-D warnings`, `cargo test --workspace`, `scripts/smoke.sh` (headless), GUI sobe oculta no display do usuário (`--hidden`) e a segunda instância reabre a janela. **Não validado:** uso interativo da GUI, Windows/macOS (bandeja `tray-icon`, atalho), serviços reais.
- **Não validado com serviços reais** nesta máquina: Telegram, OpenAI Realtime, Google, WhatsApp (há mocks/testes unitários). Backend real do WhatsApp compila por padrão (`whatsapp-real`).
- **Próximos:** teste interativo da GUI pelo usuário (Linux) e correções; workflows `ci.yml`/`release.yml` (matriz Linux x64, Windows x64, macOS x64 e arm64; `docs/13`) e bundles por SO; testes contra serviços reais; backlog em `docs/15`.
- Ambiente desta máquina: Rust via rustup em `~/.cargo/bin` (exportar no PATH); sem sudo; headers da ALSA via sysroot local em `~/.local/will-ia-sysroot` (`PKG_CONFIG_PATH`).

## Ao alterar comportamento

1. Atualize o requisito em `docs/02`/`03`.
2. Decisão arquitetural nova → ADR em `docs/adr/` (não edite ADR aceita; substitua).
3. Mantenha este arquivo curto: aponte para os docs em vez de duplicar.
