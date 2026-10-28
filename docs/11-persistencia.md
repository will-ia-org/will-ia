# 11 — Persistência (will-storage)

## Locais

Via crate `directories` (`ProjectDirs::from("dev", "will", "will-ia")`):

| SO | Dados (`data_dir`) | Logs |
|---|---|---|
| Linux | `~/.local/share/will-ia/` | `~/.local/state/will-ia/logs/` |
| macOS | `~/Library/Application Support/will-ia/` | `~/Library/Logs/will-ia/` |
| Windows | `%APPDATA%\will\will-ia\data\` | `%LOCALAPPDATA%\will\will-ia\logs\` |

Arquivo principal: `will-ia.db` (SQLite, WAL). Transcrições das sessões ficam onde o Claude Code as guarda (`~/.claude/projects/<slug-do-cwd>/<session_id>.jsonl`); o serviço só referencia.

## Segredos

Crate `keyring` (Secret Service / Keychain / Credential Manager). Serviço: `will-ia`; usuário = `secret_ref` (ex.: `claude_token:<id>`, `openai_api_key`, `telegram_bot_token`, `google_client_secret`, `google:<account_id>:refresh_token`, `mcp:<nome>:<CHAVE>` para segredos de MCPs externos). A sessão do WhatsApp fica no store SQLite da própria biblioteca em `<data_dir>/whatsapp/store.db` (0600). Em Linux sem Secret Service disponível, fallback para arquivo `secrets.json` com permissão 0600 **mediante aviso explícito na UI**.

## Esquema SQLite

```sql
CREATE TABLE settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  schema_version INTEGER NOT NULL,
  json TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE threads (
  id TEXT PRIMARY KEY,                 -- uuid
  origin TEXT NOT NULL,                -- ui | telegram | realtime
  origin_ref TEXT,                     -- telegram: "<chat_id>:<topic_id>" ; realtime: "voice"
  title TEXT,
  claude_session_id TEXT,              -- NULL = próxima mensagem começa do zero
  status TEXT NOT NULL DEFAULT 'idle', -- idle | running | error | archived
  created_at TEXT NOT NULL,
  last_activity_at TEXT NOT NULL,
  total_cost_usd REAL NOT NULL DEFAULT 0
);
CREATE INDEX threads_origin_ref ON threads(origin, origin_ref);

CREATE TABLE turns (
  id TEXT PRIMARY KEY,
  thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  prompt TEXT NOT NULL,
  result_text TEXT,
  is_error INTEGER NOT NULL DEFAULT 0,
  error_kind TEXT,
  cost_usd REAL,
  duration_ms INTEGER,
  model TEXT,
  usage_json TEXT
);
CREATE INDEX turns_thread ON turns(thread_id, started_at);

CREATE TABLE turn_events (                -- opcional (toggle): narrações e tool_use para replay na UI
  id INTEGER PRIMARY KEY,
  turn_id TEXT NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
  at TEXT NOT NULL,
  kind TEXT NOT NULL,                     -- narration | tool_use | tool_result | status
  payload TEXT NOT NULL
);

CREATE TABLE claude_tokens (
  id TEXT PRIMARY KEY,                    -- curto e estável
  label TEXT NOT NULL,
  secret_ref TEXT NOT NULL,
  active INTEGER NOT NULL DEFAULT 0,
  status TEXT,                            -- ok | error | NULL
  verified_at TEXT,
  error TEXT,
  selected_at TEXT
);
CREATE UNIQUE INDEX claude_tokens_one_active ON claude_tokens(active) WHERE active = 1;

CREATE TABLE telegram_state (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  update_offset INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE telegram_status (            -- pilha de status por thread key
  thread_key TEXT PRIMARY KEY,
  thread_id TEXT REFERENCES threads(id) ON DELETE CASCADE,
  stack_json TEXT NOT NULL DEFAULT '[]',
  status_message_id INTEGER,
  last_user_message_id INTEGER
);

CREATE TABLE mcp_calls (
  id INTEGER PRIMARY KEY,
  at TEXT NOT NULL,
  thread_id TEXT,
  tool TEXT NOT NULL,
  args_redacted TEXT,
  ok INTEGER NOT NULL,
  error TEXT,
  duration_ms INTEGER
);

CREATE TABLE external_mcp_servers (
  name TEXT PRIMARY KEY,                  -- [a-zA-Z0-9_-]
  kind TEXT NOT NULL,                     -- stdio | http | sse
  config_json TEXT NOT NULL,              -- command/args/env/cwd ou url/headers; segredos como {"$secret":"mcp:<name>:<KEY>"}
  enabled INTEGER NOT NULL DEFAULT 1,
  auto_allow_tools INTEGER NOT NULL DEFAULT 1,
  allowed_tools_json TEXT,                -- NULL = todas (mcp__<name>__*)
  last_test_at TEXT, last_test_ok INTEGER, last_test_tools_json TEXT, last_test_error TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE mcp_json_state (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  last_generated_hash TEXT,
  last_generated_at TEXT
);

CREATE TABLE tool_policies (
  tool TEXT PRIMARY KEY,                  -- ex.: gmail_send
  policy TEXT NOT NULL                    -- auto | confirm | off
);

CREATE TABLE approvals (
  id TEXT PRIMARY KEY,
  requested_at TEXT NOT NULL,
  thread_id TEXT,
  tool TEXT NOT NULL,
  summary TEXT NOT NULL,                  -- texto legível mostrado ao usuário
  args_redacted TEXT,
  decided_at TEXT,
  decision TEXT,                          -- approved | denied | timeout
  decided_via TEXT                        -- ui | notification | telegram
);

-- Google
CREATE TABLE google_accounts (
  id TEXT PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  secret_ref TEXT NOT NULL,               -- refresh token no keyring
  scopes TEXT NOT NULL,                   -- separado por espaço
  status TEXT NOT NULL DEFAULT 'ok',      -- ok | reauthorize | error
  connected_at TEXT NOT NULL,
  last_used_at TEXT
);

CREATE TABLE google_calendars (
  account_id TEXT NOT NULL REFERENCES google_accounts(id) ON DELETE CASCADE,
  calendar_id TEXT NOT NULL,
  summary TEXT NOT NULL,
  timezone TEXT,
  access_role TEXT,                       -- owner | writer | reader
  color TEXT,
  enabled INTEGER NOT NULL DEFAULT 0,
  default_for_create INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (account_id, calendar_id)
);

-- WhatsApp (índice local; a sessão/criptografia fica no store da biblioteca)
CREATE TABLE wa_contacts (
  jid TEXT PRIMARY KEY, phone TEXT, name TEXT, push_name TEXT, updated_at TEXT NOT NULL
);
CREATE TABLE wa_chats (
  jid TEXT PRIMARY KEY, name TEXT, is_group INTEGER NOT NULL, archived INTEGER NOT NULL DEFAULT 0,
  unread_count INTEGER NOT NULL DEFAULT 0, last_message_at TEXT, last_message_preview TEXT
);
CREATE TABLE wa_groups (
  jid TEXT PRIMARY KEY REFERENCES wa_chats(jid) ON DELETE CASCADE,
  subject TEXT, description TEXT, participants_json TEXT, admins_json TEXT, updated_at TEXT
);
CREATE TABLE wa_messages (
  id TEXT PRIMARY KEY,                    -- message id do protocolo
  chat_jid TEXT NOT NULL REFERENCES wa_chats(jid) ON DELETE CASCADE,
  sender_jid TEXT, from_me INTEGER NOT NULL, at TEXT NOT NULL,
  kind TEXT NOT NULL,                     -- text | image | audio | video | document | sticker | location | other
  text TEXT, media_ref TEXT, media_path TEXT, quoted_id TEXT, read INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX wa_messages_chat_at ON wa_messages(chat_jid, at);
CREATE VIRTUAL TABLE wa_messages_fts USING fts5(text, content='wa_messages', content_rowid='rowid');
CREATE TABLE wa_outbox (
  id TEXT PRIMARY KEY, at TEXT NOT NULL, chat_jid TEXT NOT NULL, kind TEXT NOT NULL,
  status TEXT NOT NULL,                   -- sent | delivered | read | error
  error TEXT, approval_id TEXT
);

-- Agendamentos (scheduler, migration 0005; docs/24)
CREATE TABLE schedules (
  id TEXT PRIMARY KEY,                    -- uuid v7
  name TEXT NOT NULL,
  message TEXT NOT NULL,
  trigger_kind TEXT NOT NULL,             -- cron | once
  cron_expr TEXT,
  run_at TEXT,
  timezone TEXT,                          -- IANA; NULL = fuso padrão do scheduler/sistema
  thread_id TEXT NOT NULL,                -- sem FK: a thread pode ser apagada e recriada
  claude_session_id TEXT,                 -- sessão da thread na criação, para --resume ao recriar
  origin TEXT NOT NULL,                   -- ui | telegram | realtime
  origin_ref TEXT,                        -- telegram: "<chat_id>[:<topic_id>]"; realtime: "voice"
  enabled INTEGER NOT NULL DEFAULT 1,
  created_by TEXT NOT NULL DEFAULT 'agent',  -- agent | user
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  next_run_at TEXT,
  last_run_at TEXT,
  last_result TEXT,                       -- ok | motivo da falha
  run_count INTEGER NOT NULL DEFAULT 0,
  paused_reason TEXT                      -- preenchido quando o scheduler pausou sozinho
);
CREATE INDEX schedules_next ON schedules(enabled, next_run_at);
CREATE INDEX schedules_thread ON schedules(thread_id);

CREATE TABLE schedule_runs (              -- histórico curto: últimos 50 por agendamento
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  schedule_id TEXT NOT NULL REFERENCES schedules(id) ON DELETE CASCADE,
  at TEXT NOT NULL,
  thread_id TEXT,                         -- thread que recebeu (pode ser a recriada)
  ok INTEGER NOT NULL,
  detail TEXT                             -- manual | erro
);
CREATE INDEX schedule_runs_sched ON schedule_runs(schedule_id, at);

CREATE TABLE events_log (                 -- eventos operacionais para a aba Logs (retenção N dias)
  id INTEGER PRIMARY KEY,
  at TEXT NOT NULL,
  level TEXT NOT NULL,
  module TEXT NOT NULL,
  message TEXT NOT NULL,
  fields_json TEXT
);
```

## Acesso

- `sqlx` com `SqlitePool` (1 conexão de escrita, N de leitura), `PRAGMA journal_mode=WAL; synchronous=NORMAL; foreign_keys=ON; busy_timeout=5000`.
- Migrations embutidas (`sqlx::migrate!`).
- Repositórios por agregado (`ThreadRepo`, `TurnRepo`, `TokenRepo`, `TelegramRepo`, `McpLogRepo`), todos atrás de traits de `will-core`.
- Escritas frequentes (pilha de status, offset) com debounce de 200 ms em memória e flush no shutdown, como na referência.

## Retenção

- `turn_events`: 30 dias (configurável).
- `events_log`, `mcp_calls`: 30 dias.
- `turns`: sem limite por padrão (são pequenos); opção de limpeza por thread.
- `wa_messages`: 90 dias (configurável); mídia baixada segue a retenção de `inbox/`.
- `approvals`: 90 dias.
- `schedule_runs`: últimos 50 por agendamento (poda na própria transação do disparo); `schedules` não expira.

## Backup

Botão "Exportar dados" gera um zip com o `.db` (via `VACUUM INTO`) e a configuração sanitizada. Sem segredos.


## Custos de IA (`ai_costs`, migration 0003)

Tabela genérica: `at`, `source` (`voice`, `claude`, …), `ref` (sessão/thread/turno), `model`, `units_json` (contagens por tipo de token), `usd`, `estimated` (1 = calculado pela tabela de preços local), `duration_secs`, `note`. Resumo por período e fonte em `Storage::ai_cost_summary`.

## Agendamentos (`schedules`, `schedule_runs`, migration `0005_schedules.sql`)

Repositório em `will-storage::schedules` (`NewSchedule`, `insert/get/list/list_for_thread/count_enabled/update/delete_schedule`, `set_schedule_next`, `record_schedule_run`, `list_schedule_runs`). `record_schedule_run` é uma transação: atualiza `last_run_at`/`last_result`/`run_count`/`next_run_at`, troca `thread_id` se a thread foi recriada, pausa (`enabled = 0` + `paused_reason`) quando não há próxima ocorrência, insere o run e poda para 50. `thread_id` não tem FK de propósito (ADR-0017). Ver [24](24-scheduler.md).
