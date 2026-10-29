# will-ia

Assistente pessoal de IA para Windows, Linux e macOS em **dois binários**: o **servidor `will-ia`** (residente, na bandeja quando há desktop; headless em servidor Linux) e o **app `will-ia-app`** (janela nativa, cliente do servidor; abre pela bandeja). Ele orquestra o **Claude Code CLI** (cérebro, rodando sobre uma pasta local de conhecimento), a **OpenAI Realtime API** (voz) e um **bot do Telegram** (mensageria), e expõe um **servidor MCP** local com ferramentas para os agentes agirem no mundo do usuário: Telegram, **WhatsApp pessoal**, **Google Agenda** e **Gmail**.

> Status: **v0.1, pré-release**. Núcleo, API local, motor, MCP, Telegram, Google, WhatsApp (várias contas) e voz compilam e têm testes; a **UI nativa (egui/eframe)** está implementada (ADR-0016) e, desde a fase 5 (ADR-0019, [`docs/26`](docs/26-cliente-servidor.md)), é um **app cliente** separado do servidor (etapas 1–3 prontas; contas por pessoa, rede/TLS e voz multiusuário a caminho). Documentação completa em [`docs/`](docs/README.md); guia de teste em [`docs/23-guia-de-teste.md`](docs/23-guia-de-teste.md).

## Como funciona

```
 will-ia-app (janela egui, voz/áudio cpal, PTT) ─ HTTP + WS ─┐          ┌─► claude -p (thread A) ─┐
 Telegram ──────────────────────────────────────────────────┼─► will-ia ─┼─► claude -p (thread B) ─┼─► Base de Conhecimento (pasta do servidor)
                                                            │ (servidor)└─► claude -p (thread N) ─┘
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
| App desktop `will-ia-app` (janela, atalho) | `egui`/`eframe` (nativo, sem WebView), cliente HTTP/WS `will-client` (`reqwest` + `tokio-tungstenite`), `global-hotkey`, `rfd` |
| Servidor `will-ia` (bandeja) | `axum` (API local), `tray-icon` (Windows/macOS) e `ksni` (Linux) com o item "Abrir will-ia" |
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
│   ├── will-server      # orquestrador, API/WS (contrato app↔servidor), módulos
│   ├── will-client      # cliente tipado da API (HTTP + WS de eventos + WS de áudio)
│   ├── will-ia          # binário `will-ia`: servidor residente, bandeja, lança o app; `serve` headless
│   └── will-app         # binário `will-ia-app`: app egui/eframe, perfis de conexão, hotkey, áudio cpal, indicador de voz
├── packaging/           # linux/ (.desktop do app), systemd/ (user unit do servidor), macos/ (Info.plist), portable/ (lançadores)
├── tools/fake-claude/   # simulador do `claude -p` para testes
└── scripts/smoke.sh     # teste ponta a ponta da API (modo headless)
```

## Pré-requisitos (uso)

- [Claude Code CLI](https://docs.anthropic.com/claude-code) instalado e um token de subscription (`claude setup-token`).
- Opcional: chave da OpenAI (voz), bot do Telegram (@BotFather), OAuth Client no Google Cloud (Agenda/Gmail), telefone com WhatsApp para parear, Node.js e Python para scripts da base de conhecimento.

## Pré-requisitos (desenvolvimento)

- Rust estável. Linux: `libasound2-dev` e `pkg-config` (ALSA para o `cpal`); nenhuma lib de GUI em tempo de build (X11/Wayland são carregados em runtime). Windows/macOS: nada além do Rust.

```bash
cargo build --release -p will-ia -p will-app   # → target/release/will-ia (servidor) e target/release/will-ia-app (app)
target/release/will-ia                         # servidor residente + bandeja ("Abrir will-ia" lança o app); --show já abre o app
target/release/will-ia serve                   # headless: só serviço + API local (--port, --data-dir, --file-secrets); Ctrl+C encerra
target/release/will-ia info                    # caminhos, URL e token da API local
target/release/will-ia-app                     # app: conecta no último perfil ou em "Esta máquina"; --url/--token ou --profile para outro servidor
target/release/will-ia-app screenshot --nav threads   # captura da janela do app (diagnóstico)
cargo test --workspace
scripts/smoke.sh                               # ponta a ponta com fake-claude (will-ia serve)
```

Os dois binários ficam **lado a lado** (o servidor acha o app na própria pasta ou no PATH). Executar `will-ia` com o servidor já no ar apenas abre o app e sai; executar `will-ia-app` com o app já aberto só traz a janela para frente. Fechar a janela do app (botões nativos do SO) encerra o app; o servidor é o processo residente (sem "Sair"; só kill forçado). Detalhes em [`docs/13`](docs/13-plataformas-e-distribuicao.md) e [`docs/26`](docs/26-cliente-servidor.md).

## Modo portátil (pendrive)

Coloque os binários (`will-ia-<os>-<arch>` e `will-ia-app-<os>-<arch>`) em `bin/`, os lançadores `packaging/portable/will-ia.sh`/`.cmd` (sobem o servidor e abrem o app) e um arquivo `portable` numa pasta do pendrive: o `will-ia.db` e a pasta `data/` (segredos em arquivo, sessões do WhatsApp, logs) ficam ao lado, e a base de conhecimento pode ser relativa (`workspace/`). Detalhes em [`docs/13`](docs/13-plataformas-e-distribuicao.md) e [`packaging/portable/README.md`](packaging/portable/README.md).

## Distribuição

**Jeito recomendado: o instalador.** Em [Releases](https://github.com/will-ia-org/will-ia/releases) baixe `will-ia-setup-<versão>-<plataforma>` (Linux/Windows) ou `will-ia-setup-<versão>-macos-<arch>.app.zip` (macOS), abra e siga o assistente: ele instala/atualiza por usuário, registra o servidor no login, cria os atalhos e preserva seus dados (`docs/27`). Servidor sem tela: `./will-ia-setup --yes`.

Pacotes manuais, sempre com `will-ia` **e** `will-ia-app` juntos: Linux x86_64 (`.tar.gz` e `.deb`, com `.desktop` do app e *user unit* do servidor: `systemctl --user enable --now will-ia`), Windows x86_64 (`.zip` com os dois `.exe`), macOS Intel e Apple Silicon (`.tar.gz` e `will-ia.app`, cujo executável principal é o app e leva o servidor ao lado), e o pacote portátil `will-ia-<versão>-portable.zip` para pendrive. Gerados pelo workflow `release.yml` a cada tag `v*`. Instalador Windows, `.dmg` e notarização ficam para depois; detalhes em [`docs/13`](docs/13-plataformas-e-distribuicao.md).

## Documentação

Comece por [docs/01-visao-geral.md](docs/01-visao-geral.md) e [docs/04-arquitetura.md](docs/04-arquitetura.md). Decisões em [docs/adr/](docs/adr/README.md). Roadmap em [docs/15-roadmap.md](docs/15-roadmap.md).

## Licença

A definir.
