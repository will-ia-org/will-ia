# will-ia

Assistente pessoal de IA que roda como **aplicação de bandeja** em Windows, Linux e macOS. Ele orquestra o **Claude Code CLI** (cérebro, rodando sobre uma pasta local de conhecimento), a **OpenAI Realtime API** (voz) e um **bot do Telegram** (mensageria), e expõe um **servidor MCP** local com ferramentas para os agentes agirem no mundo do usuário: Telegram, **WhatsApp pessoal**, **Google Agenda** e **Gmail**.

> Status: **v0.1, pré-release**. Núcleo, API local, motor, MCP, Telegram, Google, WhatsApp (várias contas) e voz compilam e têm testes; a **UI nativa (egui/eframe)** está implementada (ADR-0016) e substitui a antiga UI React + Tauri, já removidas. Documentação completa em [`docs/`](docs/README.md); guia de teste em [`docs/23-guia-de-teste.md`](docs/23-guia-de-teste.md).

## Como funciona

```
 Voz (Realtime, áudio cpal) ─┐                                   ┌─► claude -p (thread A) ─┐
 Telegram ───────────────────┼─► will-ia (Rust: app egui + tray ─┼─► claude -p (thread B) ─┼─► Base de Conhecimento (pasta local)
 Janela nativa (egui) ───────┘        + serviço em processo) ▲   └─► claude -p (thread N) ─┘
                            └──── MCP (localhost) ◄── telegram_* · whatsapp_* · calendar_* · gmail_* · notify_user
                                       │  (políticas: auto / confirmar / desligada)
                                       ├── WhatsApp pareado por QR (só ferramentas, não é canal de entrada)
                                       └── Conta Google (OAuth do usuário) → Agenda (multi-seleção) + Gmail (ler, buscar, enviar)
```

- **Threads**: cada conversa é um processo `claude -p` **vivo**, com stdin aberto em `stream-json`. Você pode mandar mais mensagens **enquanto ele processa**; elas entram como contexto do turno em andamento. Se o processo morrer (ocioso, reinício, troca de token), a thread volta com `--resume`.
- **Base de conhecimento**: a pasta onde o Claude Code roda. `CLAUDE.md` e `.claude/agents/` modelam o comportamento; `knowledge/` guarda a memória; `tools/` guarda scripts Node/Python que os agentes criam para si.
- **Voz**: atalho global (toggle ou push-to-talk) abre a conversa com o Realtime; o áudio sai e entra pelo próprio app (`cpal`) e um indicador flutuante mostra ouvindo / pensando / falando. Ele responde o trivial e delega o resto ao cérebro via função `ask_brain`, sem parar de conversar; quando o resultado chega, ele fala.
- **Telegram**: supergrupo com Tópicos; **cada tópico é uma thread**. Pilha de status ao vivo, resposta final como reply, comandos (`/novo`, `/reiniciar`, `/topico`, `/apagar`, `/status`, `/chave`).
- **MCP**: servidor Streamable HTTP em `127.0.0.1`, autenticado por token efêmero por processo. Primeira ferramenta: enviar mensagem no Telegram.
- **WhatsApp**: o dono pareia o próprio número por QR; os agentes leem chats, não lidas e grupos, buscam, resumem e enviam (com aprovação). Não é um canal de entrada.
- **Google Agenda e Gmail**: o usuário conecta sua conta com um OAuth Client próprio, escolhe quais agendas ficam visíveis; os agentes consultam e criam compromissos, leem e-mails por label/busca, veem o que está arquivado e enviam (com aprovação).
- **MCPs externos**: catálogo de servidores de terceiros (Trello, GitHub, Notion…) na UI; o serviço gera o `.mcp.json` da base de conhecimento com a união do nosso servidor e dos externos, com segredos como `${VAR}` injetadas por processo.
- **Aprovações**: ferramentas com efeito externo pedem confirmação do dono na UI, por notificação ou por botões no Telegram, com timeout.
- **Tokens**: vários tokens de subscription do Claude Code cadastrados, **um ativo**; a troca testa o token e não derruba o serviço.

## Stack

| Camada | Tecnologia |
|---|---|
| Linguagem | Rust (workspace de crates) |
| App desktop (janela, bandeja, atalho) | `egui`/`eframe` (nativo, sem WebView), `tray-icon` (Windows/macOS) e `ksni` (Linux), `global-hotkey`, `rfd` |
| Motor | Claude Code CLI `-p --input-format stream-json --output-format stream-json` |
| Voz | OpenAI Realtime API via WebSocket, áudio com `cpal` no app (PCM16 24 kHz) |
| Telegram | `teloxide` (cliente), long polling, tópicos de fórum |
| MCP | `rmcp` + `axum`, Streamable HTTP, políticas de aprovação |
| WhatsApp | `whatsapp-rust` (protocolo multi-device), índice local SQLite + FTS |
| Google | `oauth2` (PKCE + loopback), REST direto com `reqwest` (Calendar v3, Gmail v1) |
| Estado | SQLite (`sqlx`) + keyring do SO |

## Estrutura do repositório

```
will-ia/
├── README.md            # este arquivo
├── CLAUDE.md            # memória do projeto para o assistente de código
├── docs/                # requisitos, arquitetura, módulos, ADRs, API, guia de teste
├── crates/
│   ├── will-core        # tipos, eventos, settings, contratos (ThreadHost, WillTool, aprovações)
│   ├── will-storage     # SQLite (sqlx) + keyring/arquivo de segredos
│   ├── will-engine      # runner por thread do `claude -p` (stream-json, steering mid-turn, resume)
│   ├── will-mcp         # servidor MCP `will_ia` (Streamable HTTP, bearer por processo, políticas)
│   ├── will-telegram    # bot com tópicos, pilha de status, comandos, aprovações
│   ├── will-google      # OAuth PKCE + Agenda + Gmail (REST)
│   ├── will-whatsapp    # pareamento, índice local, ferramentas (whatsapp-rust)
│   ├── will-realtime    # sessão OpenAI Realtime + handoff ask_brain + pipeline de áudio
│   ├── will-platform    # autostart, notificações, abrir pasta/URL, which
│   ├── will-server      # orquestrador, API/WS local (headless/automação), módulos
│   └── will-desktop     # binário `will-ia`: app egui/eframe, bandeja, hotkey, áudio cpal, indicador de voz, modo `serve`
├── packaging/           # linux/ (.desktop), systemd/ (user unit), macos/, windows/ — a fazer
├── tools/fake-claude/   # simulador do `claude -p` para testes
└── scripts/smoke.sh     # teste ponta a ponta da API (modo headless)
```

## Pré-requisitos (uso)

- [Claude Code CLI](https://docs.anthropic.com/claude-code) instalado e um token de subscription (`claude setup-token`).
- Opcional: chave da OpenAI (voz), bot do Telegram (@BotFather), OAuth Client no Google Cloud (Agenda/Gmail), telefone com WhatsApp para parear, Node.js e Python para scripts da base de conhecimento.

## Pré-requisitos (desenvolvimento)

- Rust estável. Linux: `libasound2-dev` e `pkg-config` (ALSA para o `cpal`); nenhuma lib de GUI em tempo de build (X11/Wayland são carregados em runtime). Windows/macOS: nada além do Rust.

```bash
cargo build --release -p will-desktop   # → target/release/will-ia
target/release/will-ia                  # serviço + bandeja + janela (fecha para a bandeja; "Sair" na bandeja)
target/release/will-ia --show           # força a janela principal
target/release/will-ia serve            # headless: só serviço + API local (--port, --data-dir, --file-secrets)
cargo test --workspace
scripts/smoke.sh                        # ponta a ponta com fake-claude, no modo headless
```

Executar `will-ia` com o serviço já no ar apenas mostra a janela da instância ativa e sai.

## Modo portátil (pendrive)

Coloque os binários em `bin/`, os lançadores `packaging/portable/will-ia.sh`/`.cmd` e um arquivo `portable` numa pasta do pendrive: o `will-ia.db` e a pasta `data/` (segredos em arquivo, sessões do WhatsApp, logs) ficam ao lado, e a base de conhecimento pode ser relativa (`workspace/`). Detalhes em [`docs/13`](docs/13-plataformas-e-distribuicao.md) e [`packaging/portable/README.md`](packaging/portable/README.md).

## Distribuição (planejada)

Binários por SO gerados no GitHub Actions em matriz paralela: Linux x86_64 (`.tar.gz`, `.deb`, AppImage opcional, com `.desktop` e *user unit* do systemd: `systemctl --user enable --now will-ia`), Windows x86_64 (`.zip` + instalador, autostart via chave `Run`), macOS x86_64 e aarch64 (`.app` em `.dmg`, uma build por arquitetura, Login Item, notarização quando houver certificado). Os workflows e a pasta `packaging/` ainda não existem; detalhes em [`docs/13`](docs/13-plataformas-e-distribuicao.md).

## Documentação

Comece por [docs/01-visao-geral.md](docs/01-visao-geral.md) e [docs/04-arquitetura.md](docs/04-arquitetura.md). Decisões em [docs/adr/](docs/adr/README.md). Roadmap em [docs/15-roadmap.md](docs/15-roadmap.md).

## Licença

A definir.
