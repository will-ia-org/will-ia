# 22 — API local (contrato interno)

O servidor (`will-ia`, residente ou `serve`) sobe esta API em `http://127.0.0.1:<porta>` por padrão. O endereço vem de `settings.network.bind` (ou `will-ia --bind <ip>`, que sobrepõe); fora do loopback só com TLS (`settings.network.tls`, certificado próprio ou autoassinado em `<data_dir>/tls/`) ou com `allow_remote_without_tls` explícito; senão o servidor volta para `127.0.0.1` e registra erro (`docs/12`). Com TLS a URL é `https://host:porta` e o cliente precisa confiar no certificado (`will-client::TlsOptions { ca_pem, insecure }`; o app fixa o PEM no perfil). **Não há CORS**: a API não atende navegadores, só o app (`will-client`) e scripts; requisições com `Origin` não recebem cabeçalhos `Access-Control-*`. Ela **não serve UI** em HTML: desde a fase 5 (`docs/26`, ADR-0019) é o **contrato entre o app desktop `will-ia-app` e o servidor**, consumido pelo crate tipado `will-client` (um método por rota + WS de eventos + WS de áudio), além de `scripts/smoke.sh`, automações e clientes externos. A porta efetiva fica em `<data_dir>/api-port` (é assim que o app descobre o servidor local).

## Autenticação

Toda rota (exceto `GET /api/health` e `POST /api/auth/login`) exige um bearer, enviado como `Authorization: Bearer <token>` ou, no WebSocket e em downloads, como `?token=`. O middleware `auth` (`crates/will-server/src/api/mod.rs`) aceita **dois tipos de token** e põe um `Principal` (`will_core::auth`: `user_id?`, `username`, `name`, `role`, `must_change_password`) na requisição:

| Token | De onde vem | Quem é | Uso |
|---|---|---|---|
| **Token local da API** | `<data_dir>/ui-session.token` (0600), gerado na primeira execução; `will-ia info` o mostra | `Principal::local()`: **admin implícito**, `username = "local"`, sem `user_id` | Só para quem tem acesso ao disco do servidor: app em loopback ("Esta máquina"), `scripts/smoke.sh`, automações locais. Não serve para login de pessoa; `POST /api/auth/password` com ele responde 400 |
| **Token de sessão** | `POST /api/auth/login` (usuário + senha) | O usuário da sessão, com seu papel (`admin` \| `member`) | App conectado a um servidor (remoto ou local) com conta; é o que fica salvo no perfil do app |

Sessão: token aleatório de 32 bytes (hex); o servidor guarda só o **sha256** (`sessions.token_hash`), com validade **fixa de 30 dias** (não renova pelo uso; `last_used_at` é atualizado no máximo uma vez por minuto), `client` livre (ex.: `will-ia-app/0.1.2 linux`). Sessões expiradas são apagadas ao serem usadas e na subida do servidor. Desativar um usuário derruba todas as suas sessões; usuário inativo não autentica.

Primeira subida sem usuários (`bootstrap_admin`): cria `admin` ("Administrador", papel `admin`, `must_change_password = 1`) com senha aleatória de 16 caracteres gravada em `<data_dir>/admin-initial-password.txt` (0600; duas linhas: usuário e senha), avisa no log e em `/api/logs`; `will-ia info` imprime usuário e senha. O arquivo é apagado na primeira troca de senha.

**Portão de administrador** (`admin_gate` / `is_admin_path`): com `Principal` de membro, estas rotas respondem **403** `{ "error": "só administradores" }`: `PUT /api/settings`, `/api/secrets*`, `/api/engine/tokens*`, `/api/mcp/script-token*`, `PUT /api/mcp/tools/{name}/policy`, `/api/mcp/external*`, `/api/users*`, `POST /api/kb/scaffold`, `POST /api/kb/open`, `GET /api/kb/browse`, `GET /api/diagnostics`, `/api/browser/install/*`, `POST /api/browser/launch`, `POST /api/browser/close`, `/api/google/connect`, `/api/google/callback`, `/api/google/accounts/*` (reautorizar, revogar), `/api/google/calendars*`, `/api/whatsapp/pair|unlink|reconnect`, `/api/whatsapp/accounts/*`, `GET /api/network/cert`. Tudo o mais (threads, mensagens, uploads, `files`, aprovações, agendamentos, voz, logs, custos, leitura de settings/status/health) é liberado a membros.

Códigos: **401** `token de sessão ausente` / `token de sessão inválido` (também para sessão expirada ou usuário desativado) / `usuário ou senha inválidos` / `senha atual incorreta`; **403** só administradores; **429** `muitas tentativas; aguarde alguns minutos` no login (`LoginLimiter`: **10 tentativas por IP em 15 min**, contador zerado no login bem-sucedido).

**Autoria**: o `Principal` vira rótulo em `turns.author` (`username`; `local` para o token local; `NULL` para Telegram/voz/scheduler), em `approvals.decided_via = "ui:<username>"` e nos logs de login/criação de usuário. Threads continuam compartilhadas: todo usuário vê todas.

## Convenções

- JSON; erros `{ "error": "mensagem" }` com status 4xx/5xx.
- Datas ISO-8601 UTC.
- Segredos nunca retornam; campos `*_ref` indicam presença (`has_bot_token: true`).

## Endpoints

### Autenticação e usuários (`docs/26`, etapa 4)
| Método | Rota | Descrição |
|---|---|---|
| POST | `/api/auth/login` | **sem token**. `{ login, password, client? }` (`login` = usuário **ou** e-mail, sem diferenciar maiúsculas) → `{ token, user: Principal, must_change_password }`. 401 credenciais/usuário inativo; 429 rate limit |
| POST | `/api/auth/logout` | invalida a sessão do token usado → `{ ok }` (com o token local é inócuo) |
| GET | `/api/auth/me` | o `Principal` do token (`{ user_id, username, name, role, must_change_password }`) |
| POST | `/api/auth/password` | `{ current?, new }` troca a própria senha; `current` é dispensado quando `must_change_password` está ligado; mínimo 8 caracteres (400); apaga `admin-initial-password.txt`. 400 com o token local |
| GET | `/api/users` | **admin**. Lista `UserRecord` (`id, username, name, email, role, active, must_change_password, created_at, last_login_at`; nunca o hash) |
| POST | `/api/users` | **admin**. `{ username, password, name?, email?, role?: admin\|member, must_change_password?: true }`; `username` minúsculo `[a-z0-9._-]`, único; senha ≥ 8 |
| PATCH | `/api/users/{id}` | **admin**. `{ name?, email?, role?, active?, password? }`; `password` redefine, força troca no próximo login e derruba as sessões do usuário; `active: false` derruba as sessões. 400 ao desativar/rebaixar a própria conta |
| DELETE | `/api/users/{id}` | **admin**. **Desativa** (não apaga: a autoria em turnos/aprovações continua válida); 400 na própria conta |
| GET | `/api/users/{id}/sessions` | **admin**. `[ { token_hash, user_id, created_at, expires_at, last_used_at, client } ]` |

### Sistema
| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/health` | sem token. `{ ok, version, scheme: "http"\|"https", tls_fingerprint_sha256: hex\|null }` (o app compara a impressão digital com a do PEM fixado no perfil) |
| GET | `/api/network/cert` | **admin**. Certificado TLS em uso: `{ path, pem, fingerprint_sha256 }` (tudo `null` sem TLS). É o PEM que o admin copia para os perfis dos apps remotos |
| GET | `/api/status` | módulos (`{module: ModuleStatus}`), KB, engine stats, counts |
| GET | `/api/settings` | `Settings` + `secrets_present: {campo: bool}` |
| PUT | `/api/settings` | corpo `Settings` parcial por seção: `{ "section": "telegram", "value": {...} }` → aplica a quente |
| POST | `/api/secrets` | `{ key, value }` grava no keyring (keys: `openai_api_key`, `telegram_bot_token`, `google_client_secret`, `claude_token:<id>`, `mcp:<name>:<KEY>`) |
| DELETE | `/api/secrets/{key}` | remove |
| GET | `/api/logs?limit&module&level` | registros |
| GET | `/api/costs?limit&source` | registros de custo de IA (`ai_costs`) |
| GET | `/api/costs/summary?source` | totais por período (hoje, 7 d, 30 d, total) e por fonte |
| POST | `/api/ui/screenshot?nav=` | pede uma captura da janela do **app**: o servidor roda `will-ia-app screenshot --nav <pagina>` (binário ao lado do `will-ia` ou no PATH), que grava `<config>/will-ia-app/screenshot.png`. Diagnóstico/iteração de layout |
| GET | `/api/diagnostics` | zip (segredos redigidos) |
| WS | `/api/events?token=` | stream de `Event` (JSON por mensagem) |
| POST | `/api/ui/show` | **lança (ou traz para frente) o app** `will-ia-app`: o servidor executa o binário ao lado do seu ou no PATH; se o app já está aberto, ele mesmo só mostra a janela (ipc do app, `docs/13`). Responde `{ "ok": true }` quando há o gancho (hoje em `will-ia` e `will-ia serve`); sem app instalado o erro fica só no log. Em servidor sem desktop é inútil, mas inofensivo |

### Threads
| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/threads` | lista `ThreadSummary` (alive/pending/session_expired preenchidos) |
| POST | `/api/threads` | `{ title? }` cria thread de origem `ui` |
| GET | `/api/threads/{id}` | detalhe |
| GET | `/api/threads/{id}/turns?limit` | turnos (`author` = quem enviou pela API, `null` para outros canais; `prompt` cru, `prompt_display` sem notas de anexo, `attachments[]` = `{name, mime, path, size}`, `merged_into` em turnos absorvidos) |
| GET | `/api/turns/{id}/events` | eventos do turno |
| POST | `/api/threads/{id}/messages` | `{ text, attachments?: [caminho no servidor] }` → `{ turn_id, mid_turn }` (imediato). Cada anexo é copiado para `<kb>/inbox/ui/<data-hora>-<nome>` (já no inbox, como os vindos de `POST /api/uploads`, entra sem nova cópia) e vai ao prompt como nota de anexo; `text` pode ser vazio se houver anexos |
| POST | `/api/threads/{id}/restart` | mantém sessão |
| POST | `/api/threads/{id}/reset` | esquece sessão |
| POST | `/api/threads/{id}/interrupt` | |
| PATCH | `/api/threads/{id}` | `{ title }` |
| DELETE | `/api/threads/{id}?purge=true` | |

### Motor Claude
| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/engine/tokens` | lista (mascarada) |
| POST | `/api/engine/tokens` | `{ label, token }` cadastra (grava no keyring) |
| POST | `/api/engine/tokens/{id}/test` | testa num processo descartável |
| POST | `/api/engine/tokens/{id}/activate` | testa e ativa; derruba vivos |
| DELETE | `/api/engine/tokens/{id}` | |
| GET | `/api/engine/check` | binário encontrado? versão? |

### Base de conhecimento
| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/kb/status` | existe, gravável, tem CLAUDE.md, node/python detectados, avisos |
| POST | `/api/kb/scaffold` | cria esqueleto |
| POST | `/api/kb/open` | abre no explorador |
| GET | `/api/kb/mcp-json` | conteúdo gerado |
| GET | `/api/kb/browse?path=` | Subpastas de um diretório do **servidor** (`{path, parent, home, writable, dangerous, dirs:[{name,path}]}`); sem `path` = home. Para o app escolher a KB sem diálogo local (etapa 2 de `docs/26`) |
| GET | `/api/files?path=` | Conteúdo de um arquivo **dentro da KB** (miniaturas e download no app); fora da KB → 403; até 64 MiB |
| POST | `/api/uploads?name=` | Corpo bruto do arquivo → `<kb>/inbox/ui/<carimbo>-<nome>`; devolve `{name, mime, path, size}`; o `path` entra direto em `POST /api/threads/{id}/messages` sem nova cópia; limite 64 MiB |

### MCP
| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/mcp/script-token` | `{ token, masked, url, active }` — token fixo para scripts fora das threads (gera e grava no cofre se não existir) |
| POST | `/api/mcp/script-token/rotate` | gera outro; o anterior é revogado na hora |
| GET | `/api/mcp/tools` | `[ { name, family, description, default_policy, policy, available } ]` |
| PUT | `/api/mcp/tools/{name}/policy` | `{ policy: auto\|confirm\|off\|null }` |
| GET | `/api/mcp/calls?limit` | log |
| GET | `/api/approvals` | pendentes |
| GET | `/api/approvals/history?limit` | |
| POST | `/api/approvals/{id}` | `{ decision: approved\|denied }`; grava `decided_via = "ui:<username>"` |
| GET | `/api/mcp/external` | catálogo |
| PUT | `/api/mcp/external/{name}` | cria/edita (`ExternalMcpServer`; segredos como `{ "$secret_value": "..." }` no corpo → gravados no keyring) |
| DELETE | `/api/mcp/external/{name}` | |
| POST | `/api/mcp/external/{name}/test` | `initialize` + `tools/list` |
| POST | `/api/mcp/external/import-claude` | lê `~/.claude.json` |
| POST | `/api/mcp/external/import-json` | `{ json }` bloco `mcpServers` |

### Telegram
| Método | Rota | Descrição |
|---|---|---|
| POST | `/api/telegram/test` | `getMe` |
| GET | `/api/telegram/status` | threads por key |

### Voz
| Método | Rota | Descrição |
|---|---|---|
| POST | `/api/voice/start` / `/stop` / `/toggle` | sessão de voz **do usuário do token** (uma por usuário; `docs/07`). `start`/`toggle` respeitam `realtime.max_sessions` contando as sessões ativas de outros usuários → 400 `limite de N sessão(ões) de voz simultânea(s) atingido`. Resposta `{ ok, active, user }` |
| GET | `/api/voice/status` | estado da sessão do usuário (`active`, `state`, …) + `user`, `sessions: [{ user, active, state }]` (todas), `max_sessions`, `active_sessions` |
| WS | `/api/voice/audio?token=` | **único caminho de áudio** (app e clientes): liga à sessão do usuário do token; cliente envia PCM16 24 kHz mono (frames), recebe PCM16 do assistente; mensagens de texto JSON para controle (`{"type":"ptt","down":true}`, `{"type":"commit"}`) e eventos (`speaking`, `listening`, `transcript`) |
| GET | `/api/voice/transcripts?limit` | |

### WhatsApp
| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/whatsapp/status` | `{ enabled, started, backend, media_dir, accounts: [{ id, label, phone, enabled, state, detail, me, last_qr, last_pair_code, last_sync, has_session, index: {chats, messages, contacts} }], index_total }` |
| POST | `/api/whatsapp/pair` | `{ account_id?, phone?, label? }` → `{ ok, account_id }`. Sem `account_id` cria uma conta nova; `phone` → pareamento por código, senão QR. QR chega pelo evento `WhatsApp{state:"qr", detail:"{account_id,data,svg,…}"}`; código por `state:"code"`. Sincroniza `settings.whatsapp.accounts` |
| POST | `/api/whatsapp/unlink` | `{ account_id, clear_index?, remove? }` — logout remoto + apaga sessão; `clear_index` limpa o índice da conta; `remove` tira a conta da lista/configurações |
| POST | `/api/whatsapp/reconnect` | `{ account_id }` |
| PATCH | `/api/whatsapp/accounts/{id}` | `{ label?, enabled? }` → `{ ok, account }`; desativar desconecta, ativar reconecta se houver sessão |

### Agendamentos (scheduler)
Corpo de `POST`/`PUT` = `ScheduleInput` (`name`, `message`, `cron` **ou** `run_at`, `timezone?`, `enabled?`, `thread_id?`). `503` se o módulo não iniciou. Ver [24](24-scheduler.md).

| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/schedules?thread_id=` | `{ schedules: [ScheduleView], enabled }` — `ScheduleView` = `Schedule` + `trigger_short`, `cron_description`, `timezone_label`, `next_run_local`, `next_run_in`, `thread_title`, `thread_exists` |
| POST | `/api/schedules` | cria; `thread_id` **obrigatório** (thread dona); `created_by = "user"` → `ScheduleView` |
| GET | `/api/schedules/preview?cron=&timezone=&n=` | prévia de expressão: `{ description, timezone, next: [{ at, local, in }], error }` (`n` 1–20, padrão 5; `error` = viola intervalo mínimo) |
| GET | `/api/schedules/{id}` | `{ schedule, runs }` (últimos 20 disparos); 404 se não existe |
| PUT | `/api/schedules/{id}` | campos parciais (inclusive `thread_id` e `enabled` para pausar/retomar) → `ScheduleView` |
| DELETE | `/api/schedules/{id}` | `{ ok }` |
| POST | `/api/schedules/{id}/run` | dispara agora (cabeçalho marcado `(manual)`; não altera `next_run_at`) → `{ ok }` |

Eventos relacionados no WS `/api/events`: `ScheduleFired { schedule_id, thread_id, name, ok, detail }`, `SchedulesChanged`, `Notification`, `ModuleStatus { module: "scheduler" }`. Seção de settings: `PUT /api/settings { "section": "scheduler", "value": SchedulerSettings }`.

### Browser CDP
`503` `Browser CDP não iniciado` se o módulo não existe. Ver [25](25-browser-cdp.md).

| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/browser/status` | `BrowserStatus`: `enabled`, `listening`, `listen_url`, `cdp_env`, `upstream`, `upstream_authority` (quem respondeu, ex. `[::1]:9222`), `upstream_source` (`http` \| `DevToolsActivePort (<perfil>)`), `upstream_http_discovery`, `upstream_up`, `upstream_browser`, `launched { pid, path, headless, started_at }`, `launching`, `connections`, `active_connections`, `counters`, `last_seq`, `last_error`, `resolved_browser`, `user_data_dir` |
| GET | `/api/browser/detect` | `{ browsers: [{ name, path }] }` (Chrome, Chromium, Brave, Edge, Chromium do agent-browser) |
| GET | `/api/browser/traffic?after=&limit=` | `{ entries: [TrafficEntry { seq, at, conn, dir, id, method, session_id, kind, preview, bytes }], last_seq, counters }` (`limit` 1–1000, padrão 200; `after` = último `seq` visto) |
| POST | `/api/browser/traffic/clear` | `{ ok }` |
| POST | `/api/browser/launch` | garante o upstream (lança o navegador se preciso) → `BrowserStatus`; `400` com o motivo |
| POST | `/api/browser/close` | fecha o navegador lançado pelo serviço → `BrowserStatus` |
| POST | `/api/browser/test/upstream` | `GET /json/version` direto no navegador → `{ ok, version }` (não lança) |
| POST | `/api/browser/test/new-tab` | `{ url? }` (padrão `about:blank`) → `PUT /json/new` **pelo proxy** → `{ ok, target }` |
| POST | `/api/browser/test/agent-browser` | `agent-browser --cdp http://127.0.0.1:<listen_port> --session will-ia-test open about:blank` → `{ ok, output }` (sempre `200`) |
| GET | `/api/browser/install/status` | `InstallStatus { node, node_version, npm, npm_version, agent_browser, agent_browser_version, agent_browser_chrome, kb_path, skill_in_kb, skill_in_kb_path, warnings }` |
| GET | `/api/browser/install/job` | `Job { name, running, started_at, finished_at, ok, output }` (um trabalho por vez; saída = últimas 6000 caracteres) |
| POST | `/api/browser/install/agent-browser` | inicia `npm install -g agent-browser@latest` → `Job`; `400` se já há trabalho ou sem `npm` |
| POST | `/api/browser/install/chromium` | inicia `agent-browser install` → `Job` |
| POST | `/api/browser/install/skill` | copia o `SKILL.md` do CLI para `<kb>/.claude/skills/agent-browser/` (fallback `npx -y skills add …` em segundo plano) → `{ ok, path, job }` |

Eventos no WS `/api/events`: `Browser { state: listening | launched | browser_up | closed | ws_open | ws_closed, detail }`, `ModuleStatus { module: "browser" }`. Seção de settings: `PUT /api/settings { "section": "browser", "value": BrowserSettings }`.

### Google
| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/google/accounts` | |
| POST | `/api/google/connect` | `{ scopes: [...], callback_port?, open_browser? }`; retorna `{ auth_url, state }`. Com `callback_port`, o cliente escuta o callback e conclui em `/api/google/callback` (ver `docs/18`) |
| POST | `/api/google/callback` | `{ state, code }` do loopback do cliente; o servidor troca pelo token; retorna `{ email }` |
| POST | `/api/google/accounts/{id}/reauthorize` | `{ callback_port?, open_browser? }`; retorna `{ auth_url, state }` |
| DELETE | `/api/google/accounts/{id}` | revoga |
| GET | `/api/google/calendars` | lista com flags |
| POST | `/api/google/calendars/refresh` | reconsulta |
| PUT | `/api/google/calendars/{account}/{calendar_id}` | `{ enabled, default_for_create }` |
