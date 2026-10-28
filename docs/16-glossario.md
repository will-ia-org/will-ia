# 16 — Glossário

| Termo | Significado |
|---|---|
| **Base de Conhecimento (KB)** | Pasta local escolhida pelo usuário onde o Claude Code roda (`cwd`). Contém `CLAUDE.md`, `.claude/`, `knowledge/`, `inbox/`, `tools/`. |
| **Cérebro / Motor** | O Claude Code CLI em modo headless, spawnado pelo serviço. |
| **Thread** | Unidade de contexto: um processo `claude` vivo (ou dormente) + uma sessão do Claude Code + origem (UI, Telegram, voz). |
| **Thread key** | Identificador de thread no Telegram: `<chat_id>` ou `<chat_id>:<message_thread_id>`. |
| **Turno** | Um ciclo pergunta → resposta final (`result`) dentro de uma thread. |
| **Steering / mid-turn** | Enviar nova mensagem ao processo enquanto um turno está em andamento; o Claude Code a incorpora. |
| **Pendência** | Entrada na fila de correlação de uma thread: mensagem enviada ainda sem `result`. |
| **Live / Dormant** | Thread com processo vivo / sem processo (sessão salva, volta com `--resume`). |
| **Retire** | Encerrar o processo de uma thread mantendo a sessão. |
| **Reset / Zerar** | Encerrar o processo e esquecer a sessão (começa do zero). |
| **Sessão expirada** | Transcrição apagada pelo Claude Code (`cleanupPeriodDays`); `--resume` falha e a thread recomeça. |
| **stream-json** | Formato NDJSON de entrada/saída do `claude -p`. |
| **Pilha de status** | Mensagem única no Telegram, editada ao vivo, mostrando o progresso do turno. |
| **Handoff** | Delegação do Realtime ao cérebro via função `ask_brain`, com resultado devolvido depois. |
| **PTT** | Push-to-talk: áudio só é enviado enquanto a tecla está pressionada. |
| **MCP** | Model Context Protocol; o serviço expõe um servidor MCP local com ferramentas para o agente. |
| **Bearer efêmero** | Token aleatório por processo `claude` que autentica chamadas MCP e identifica a thread. |
| **Token de subscription** | Token OAuth de longa duração do Claude Code (`claude setup-token`), injetado em `CLAUDE_CODE_OAUTH_TOKEN`. |
| **acceptEdits / bypassPermissions** | Modos de permissão do Claude Code: edita arquivos sem perguntar com allowlist de tools / tudo liberado. |
| **Módulo** | Subsistema ligável: Motor, Telegram, Voz, MCP, WhatsApp, Google, Scheduler, Browser CDP. Cada um com `start/stop` e status. |
| **Política de ferramenta** | `auto` / `confirmar` / `desligada`; define se uma ferramenta MCP executa direto, pede aprovação ou fica oculta. |
| **Aprovação** | Pedido gerado por ferramenta em `confirmar`, decidido pelo dono na UI, notificação ou Telegram; expira por timeout. |
| **Módulo de ferramentas** | Módulo que não é canal de entrada (WhatsApp, Google, Scheduler, Browser CDP): só expõe ferramentas MCP (e, no caso do Scheduler, uma página na UI; o Browser CDP não tem ferramentas MCP, só o proxy, a página e o env do `claude`). |
| **Agendamento** | Mensagem programada pelo agente (`schedule_*`) ou pelo usuário para ser entregue à **thread dona** no futuro: recorrente (cron de 5 campos, fuso IANA) ou único (`run_at`). Guarda a origem e o `claude_session_id` da thread para recriá-la se for apagada. |
| **Disparo** | Momento em que o scheduler entrega a mensagem de um agendamento à thread dona (stdin do processo, inclusive mid-turn), com cabeçalho `[Agendamento … disparado em …]` gerado pelo serviço; registrado em `schedule_runs`. "Disparar agora" é um disparo manual que não altera o próximo horário. |
| **Disparo perdido** | Ocorrência cujo horário passou com o app fechado (ou módulo desligado). É **ignorada**: cron recalcula a próxima a partir de agora; único é pausado com `paused_reason`. |
| **CDP** | Chrome DevTools Protocol: protocolo JSON (HTTP de descoberta em `/json/*` + WebSocket) com que ferramentas controlam Chrome/Chromium/Brave/Edge abertos com `--remote-debugging-port`. |
| **Proxy CDP** | Servidor do will-ia em `127.0.0.1:9223` (`browser.listen_port`) que repassa HTTP e WebSocket ao navegador real em 9222, reescreve `host:porta` na descoberta, registra o tráfego e lança o navegador (perfil dedicado `<data_dir>/browser-profile`) quando ninguém responde. Vai em `AGENT_BROWSER_CDP` no ambiente do `claude`. |
| **agent-browser** | CLI em Rust (vercel-labs) que o agente usa para navegar via CDP; instalado com `npm i -g agent-browser`, vem com uma skill (`SKILL.md`) que o serviço copia para `<kb>/.claude/skills/agent-browser/`. Liberado na allowlist como `Bash(agent-browser:*)`. |
| **JID** | Identificador de chat/contato no protocolo do WhatsApp (`<número>@s.whatsapp.net`, `<id>@g.us`). |
| **Label** | Marcador do Gmail; substitui pastas. Arquivado = sem `INBOX`. |
| **OAuth Client** | Credencial (`client_id`/`client_secret`) criada pelo usuário no Google Cloud para autorizar o will-ia. |
| **MCP interno / externo** | `will_ia`, servido pelo próprio serviço / servidores de terceiros catalogados e entregues via `.mcp.json`. |
| **`.mcp.json`** | Arquivo de MCPs por projeto do Claude Code, gerado pelo serviço na KB com a união de internos e externos; segredos como `${VAR}`. |
| **Degraded** | Estado de módulo que continua rodando mas com falha conhecida (auth, rate limit). |
| **Servidor / `will-ia`** | Binário residente (crate `crates/will-ia`): serviço + API + módulos, bandeja com "Abrir will-ia" quando há desktop (ADR-0019, `docs/26`). |
| **App / `will-ia-app`** | Binário da janela (crate `crates/will-app`): UI egui/eframe cliente do servidor via `will-client`, com perfis de conexão, atalho PTT e áudio; não é residente. |
| **Perfil (do app)** | Entrada de `<config>/will-ia-app/profiles.json`: nome, URL do servidor e token de sessão. "Esta máquina" é descoberto pelo `api-port` + `ui-session.token` locais. |
| **Modo headless (`serve`)** | `will-ia serve`: só serviço + API HTTP, sem bandeja; encerra limpo com Ctrl+C/SIGTERM; usado em servidores, testes (`smoke.sh`) e systemd. |
| **Indicador flutuante** | Janela secundária do egui (sem decoração, sempre no topo) exibida durante a sessão de voz com estado ouvindo / pensando / falando e nível do microfone. |
| **Token local da API** | `<data_dir>/ui-session.token` (0600): bearer de quem tem acesso ao disco do servidor (app em loopback, `smoke.sh`, automações). Autentica como **admin implícito** `local`, sem `user_id`. Não é token de pessoa. |
| **Conta / Usuário** | Linha de `users`: `username` único, nome, e-mail opcional, senha (argon2id), papel `admin` ou `member`, `active`. Nunca é apagada, só desativada. O primeiro `admin` nasce na primeira subida com senha em `admin-initial-password.txt`. |
| **Sessão (de usuário)** | Token aleatório devolvido por `POST /api/auth/login`, guardado no servidor só como sha256 (`sessions`), válido por 30 dias, revogável por logout, desativação ou redefinição de senha. É o que o perfil do app guarda. Não confundir com a sessão do Claude Code (`claude_session_id`). |
| **Principal** | Identidade resolvida pelo middleware de autenticação para cada requisição (`will_core::auth::Principal`): `user_id?`, `username`, `name`, `role`, `must_change_password`. Vem do token local (`local`, admin) ou de uma sessão. Dá a autoria (`turns.author`, `decided_via`) e passa pelo portão de admin. |
| **Papel (`admin` / `member`)** | `admin` mexe em configurações, segredos, tokens, políticas, MCPs externos, usuários, instalações e pareamentos; `member` conversa, anexa, aprova, agenda, usa voz e vê logs/custos. Rotas de configuração respondem 403 a membros (`admin_gate`). |
| **Sessão de voz (por usuário)** | `RealtimeManager` próprio de cada usuário conectado (`AppState.voice[username]`), com thread "Voz · <user>", `BrainHost` próprio e custo marcado `user:<u>`. `realtime.max_sessions` limita quantas podem estar ativas ao mesmo tempo no servidor. |
| **`max_sessions`** | Teto de sessões de voz simultâneas no servidor (padrão 3); contado sobre as sessões ativas de outros usuários ao iniciar. |
| **`bind`** | Endereço em que a API escuta (`settings.network.bind` ou `will-ia --bind`): `127.0.0.1` por padrão; fora do loopback só com TLS ou `allow_remote_without_tls`, senão volta ao loopback. |
| **Certificado fixado (`ca_pem`)** | PEM do certificado do servidor (autoassinado gerado em `<data_dir>/tls/` ou CA própria) guardado no perfil do app e usado como única raiz confiável no HTTP e no WS. Conferido pela impressão digital SHA-256 do DER (`/api/health`, `/api/network/cert`). `insecure` desliga a verificação (só testes). |
| **`has_desktop()`** | `will_platform`: há sessão gráfica? (`DISPLAY`/`WAYLAND_DISPLAY`; sempre sim em Windows/macOS). Decide bandeja do servidor, Chrome headless forçado e notificações nativas no servidor. |
| **Autoria** | Rótulo de quem fez: `turns.author` (username de quem enviou pela API) e `approvals.decided_via = "ui:<username>"`. Auditoria, não controle de acesso: threads são compartilhadas. |
