# 22 — API local (contrato interno)

O serviço (`will-ia`, em qualquer modo) sobe um servidor HTTP em `http://127.0.0.1:<porta>` com esta API. Desde a ADR-0016 ela **não serve UI**: a interface é o app nativo (egui) que roda no mesmo processo e fala com o `AppState` diretamente. A API fica como contrato **interno/headless/automação**: modo `will-ia serve`, `scripts/smoke.sh`, clientes externos e a segunda instância do binário (`POST /api/ui/show`).

## Autenticação

Token local gerado na primeira execução, gravado em `<data_dir>/ui-session.token` (0600) e impresso no log (`will-ia info` também o mostra). Enviado como `Authorization: Bearer <token>` e, no WebSocket, como `?token=`. Endpoints sem token: `GET /api/health`.

## Convenções

- JSON; erros `{ "error": "mensagem" }` com status 4xx/5xx.
- Datas ISO-8601 UTC.
- Segredos nunca retornam; campos `*_ref` indicam presença (`has_bot_token: true`).

## Endpoints

### Sistema
| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/health` | `{ ok, version, uptime_secs }` |
| GET | `/api/status` | módulos (`{module: ModuleStatus}`), KB, engine stats, counts |
| GET | `/api/settings` | `Settings` + `secrets_present: {campo: bool}` |
| PUT | `/api/settings` | corpo `Settings` parcial por seção: `{ "section": "telegram", "value": {...} }` → aplica a quente |
| POST | `/api/secrets` | `{ key, value }` grava no keyring (keys: `openai_api_key`, `telegram_bot_token`, `google_client_secret`, `claude_token:<id>`, `mcp:<name>:<KEY>`) |
| DELETE | `/api/secrets/{key}` | remove |
| GET | `/api/logs?limit&module&level` | registros |
| GET | `/api/costs?limit&source` | registros de custo de IA (`ai_costs`) |
| GET | `/api/costs/summary?source` | totais por período (hoje, 7 d, 30 d, total) e por fonte |
| POST | `/api/ui/screenshot?nav=` | captura da janela em `<data_dir>/screenshot.png` (diagnóstico) |
| GET | `/api/diagnostics` | zip (segredos redigidos) |
| WS | `/api/events?token=` | stream de `Event` (JSON por mensagem) |
| POST | `/api/ui/show` | pede à instância ativa que mostre a janela principal (usado pela segunda execução de `will-ia`). Só responde `{ "ok": true }` no modo GUI; no headless retorna `{ "ok": false, "error": "sem GUI" }` |

### Threads
| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/threads` | lista `ThreadSummary` (alive/pending/session_expired preenchidos) |
| POST | `/api/threads` | `{ title? }` cria thread de origem `ui` |
| GET | `/api/threads/{id}` | detalhe |
| GET | `/api/threads/{id}/turns?limit` | turnos (`prompt` cru, `prompt_display` sem notas de anexo, `attachments[]` = `{name, mime, path, size}`, `merged_into` em turnos absorvidos) |
| GET | `/api/turns/{id}/events` | eventos do turno |
| POST | `/api/threads/{id}/messages` | `{ text, attachments?: [caminho local] }` → `{ turn_id, mid_turn }` (imediato). Cada anexo é copiado para `<kb>/inbox/ui/<data-hora>-<nome>` e entra no prompt como nota de anexo; `text` pode ser vazio se houver anexos |
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
| POST | `/api/approvals/{id}` | `{ decision: approved\|denied }` |
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
| POST | `/api/voice/start` / `/stop` / `/toggle` | sessão |
| GET | `/api/voice/status` | |
| WS | `/api/voice/audio?token=` | **só no modo headless** (no app o áudio vai pelo `cpal`): cliente envia PCM16 24 kHz mono (frames), recebe PCM16 do assistente; mensagens de texto JSON para controle (`{"type":"ptt","down":true}`, `{"type":"commit"}`) e eventos (`speaking`, `listening`, `transcript`) |
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

### Google
| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/google/accounts` | |
| POST | `/api/google/connect` | `{ scopes: ["calendar","calendar_write","gmail","gmail_send","gmail_modify"] }` abre navegador; retorna `{ auth_url }` |
| POST | `/api/google/accounts/{id}/reauthorize` | |
| DELETE | `/api/google/accounts/{id}` | revoga |
| GET | `/api/google/calendars` | lista com flags |
| POST | `/api/google/calendars/refresh` | reconsulta |
| PUT | `/api/google/calendars/{account}/{calendar_id}` | `{ enabled, default_for_create }` |
