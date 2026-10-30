# 29 — Telemetria (OTLP/HTTP → Axiom ou qualquer backend OpenTelemetry)

> Opcional e **desligada por padrão**. Quando ligada, o servidor (e, se marcado, o app) envia
> os próprios **logs**, **spans** e **métricas de uso** pelo protocolo padrão do OpenTelemetry
> (**OTLP/HTTP com Protobuf**). O Axiom é só um preset (endpoint + `X-Axiom-Dataset`); trocar de
> provedor é trocar endpoint e cabeçalhos. ADR-0025.

## Por quê

O will-ia roda em várias máquinas (macOS, Windows, Linux) de pessoas diferentes. Sem um lugar
central para ver o que aconteceu, cada bug vira "me manda o log". A telemetria dá:

- **rastreabilidade**: cada evento carrega `instance_id`, host, SO, versão, processo e os ids do
  contexto (`thread_id`, `turn_id`, `tool`, `channel`, `account_id`…), herdados dos spans abertos;
- **correlação**: spans (`turn`, `tool_call`, `inbound`, `google_op`, `voice_session`…) têm
  `trace_id`/`span_id` reais; os logs dentro deles levam os mesmos ids;
- **métricas de uso** sem instrumentação extra: um `heartbeat` a cada 5 min (e no start) com
  eventos por nível, por módulo e spans por nome — módulo mais usado, taxa de erro, uptime;
- **relatórios de falha**: no start, o servidor envia o que ficou em `will-ia-fatal.log`,
  `service.err.log` e `setup.log` desde a última vez (um evento por arquivo).

## Arquitetura (`crates/will-telemetry`)

```
tracing (eventos + spans) ──► TelemetryLayer ──► fila (4096) ──► Shipper ──► OTLP/HTTP (protobuf)
                                   │                                 │            /v1/logs  /v1/traces
                                   └─ Metrics (contadores)           └─ spool em disco (NDJSON, 20 MB)
```

| Peça | O que faz |
|---|---|
| `TelemetryLayer` | `Layer` do `tracing-subscriber`. Em `on_new_span` guarda os campos do span + gera `trace_id` (raiz) e `span_id`; em `on_event` monta o registro com os campos dos spans abertos (raiz → folha), redige segredos e enfileira; em `on_close` enfileira o span fechado com duração e `status` (erro se `ok=false` ou campo `error`). Só alvos `will_*` (e `warn`/`error` de dependências). |
| `Metrics` | Contadores atômicos: eventos por nível, por módulo (`events/warn/error`), spans por nome e por `nome:módulo`, descartados. `take_snapshot()` devolve o delta e zera. |
| `Shipper` | Tarefa tokio: lotes de 500 ou 5 s; `POST` protobuf (`application/x-protobuf`) com os cabeçalhos do `ShipConfig` e `Authorization: Bearer`. Falhou → spool `logs/telemetry-spool.ndjson` (até 20 MB), reenviado a cada 60 s. Antes de configurado, guarda os últimos 1000 itens em memória (o início do processo chega antes das configurações). |
| `otlp` | Codificação com os tipos oficiais do `opentelemetry-proto` (`ExportLogsServiceRequest`, `ExportTraceServiceRequest`). Objetos aninhados viram atributos com ponto (`events_by_level.warn`). |
| `InstanceInfo` | `instance_id` (UUID v4 persistido em `<data_dir>/instance-id`; o app usa o seu em `<config>/will-ia-app/instance-id` e carrega `server.instance.id`), `host.name`, `os.type`, `host.arch`, `service.version`, `process` (`server`/`app`), `label`. |
| `Handle` | `configure(Option<ShipConfig>, level)` em tempo de execução, `stats()`, `send_test()`, `emit()`, `heartbeat()`, `ship_file_tail()`. |

## Modelo de rastreio (nomes de spans e campos)

| Span | Campos | Onde |
|---|---|---|
| `turn` | `thread_id`, `turn_id`, `source` (telegram\|whatsapp\|app\|voice\|schedule\|mcp\|api), `channel` | will-server (orquestrador) |
| `tool_call` | `tool`, `call_id`, `thread_id`, `policy`, `ok`, `duration_ms` | will-mcp / will-server |
| `approval` | `approval_id`, `tool`, `thread_id`, `decision` | will-server |
| `schedule_run` | `schedule_id`, `run_id`, `thread_id` | will-scheduler |
| `engine_proc` | `thread_id`, `turn_id`, `pid`, `exit_code`, `token_id` | will-engine |
| `api` | `method`, `path`, `status`, `user`, `duration_ms` | will-server (axum) |
| `module` | `module`, `status`, `reason` | will-server |
| `inbound` / `outbound` | `channel`, `chat_id`, `msg_id`, `thread_id`, `kind`, `ok`, `duration_ms` | will-telegram / will-whatsapp |
| `google_op` / `oauth` | `account_id`, `product`, `op`, `ok`, `duration_ms`, `http_status` / `step` | will-google |
| `voice_session` | `session_id`, `thread_id`, `model` | will-realtime |
| `browser` | `op`, `ok`, `duration_ms` | will-browser |
| `wa_conn` / `tg_conn` / `platform` | `attempt`, `reason`, `ok` / `permission`, `granted` | will-whatsapp, will-telegram, will-platform |

Regras: `skip_all` sempre (nunca logar structs inteiras); nunca tokens, textos de mensagens,
e-mails completos ou nomes de contatos — ids e tamanhos. O redator (`will_telemetry::redact`)
mascara `sk-ant-…`, `sk-…`, `xaat-…`, tokens Telegram, `Bearer …`, `ya29.…`, `1//…`.

## Eventos especiais (`kind`)

| `kind` | Quando | Campos |
|---|---|---|
| `heartbeat` | start + a cada 5 min | `reason`, `uptime_s`, `interval_s`, `events_by_level.*`, `events_by_module.<m>.{events,warn,error}`, `spans.<nome>` e `spans.<nome>:<módulo>`, `dropped`, `ship.{sent,failed,spooled}` |
| `file` | start (relatórios de falha) | `file_kind` (`fatal`\|`service_err`\|`setup`), `file`, `content` (últimos 64 KB novos, redigidos), `bytes` |
| `test` | botão "Enviar evento de teste" | — |

## Configuração (`Settings.telemetry`, aba Geral → Telemetria)

| Campo | Padrão | Nota |
|---|---|---|
| `enabled` | `false` | |
| `provider` | `axiom` | `axiom` (preset) ou `otlp` (genérico) |
| `endpoint` | `https://api.axiom.co` | base; o serviço acrescenta `/v1/logs` e `/v1/traces` |
| `dataset` | `will-ia` | só Axiom → `X-Axiom-Dataset` |
| `headers` | `[]` | só `otlp`; linhas `Nome: valor` |
| `send_traces` | `true` | spans como traces |
| `level` | `info` | nível mínimo enviado (as métricas contam tudo) |
| `label` | vazio | rótulo da instância; vazio = IP público detectado |
| `seeded` | `false` | padrões embutidos já aplicados (interno) |
| `ship_app_logs` | `true` | o app pede `GET /api/telemetry/client-config` (admin) e envia os próprios logs |
| `ship_crash_reports` | `true` | arquivos de falha no start |

Token: segredo `telemetry_token` (`POST /api/secrets`), vira `Authorization: Bearer`. Obrigatório
no Axiom; opcional no `otlp`.

Presets de outros provedores (provedor `otlp`):

| Backend | Endpoint | Cabeçalhos / token |
|---|---|---|
| OpenTelemetry Collector local | `http://localhost:4318` | — |
| Grafana Cloud | `https://otlp-gateway-<região>.grafana.net/otlp` | token = `base64(instanceId:apiKey)` com cabeçalho `Authorization: Basic …` (coloque em `headers`, sem token) |
| New Relic | `https://otlp.nr-data.net` | `api-key: <license>` em `headers` |
| OpenObserve (self-host) | `https://<host>/api/<org>` | `Authorization: Basic …` em `headers` |

## Distribuição pré-configurada (`baked_telemetry`)

Quem instala o will-ia de um pacote oficial já sai enviando telemetria para o nosso Axiom, sem
configurar nada — é assim que acompanhamos bugs e uso enquanto outras pessoas usam.

1. Numa instalação configurada e **ligada**, exporte: `will-ia telemetry-defaults --out <arquivo>`
   (lê o SQLite e o cofre; JSON com provedor, endpoint, dataset, cabeçalhos, nível, flags e o
   **token de ingest**; arquivo 0600).
2. No build, `crates/will-core/build.rs` embute o JSON se `WILL_TELEMETRY_DEFAULTS=<arquivo>` ou
   `WILL_TELEMETRY_DEFAULTS_JSON=<json>` estiver no ambiente (`cargo:rustc-env=WILL_TELEMETRY_BAKED`).
   No CI de release o JSON vem do segredo do repositório `WILL_TELEMETRY_DEFAULTS_JSON`.
3. Na primeira execução de um binário assim (`telemetry.seeded == false`), o servidor copia os
   padrões para as configurações, grava o token no cofre (`telemetry_token`) e liga. Acontece
   **uma vez**: se o usuário desligar depois, fica desligado. Uma instalação que já tinha
   telemetria ligada não é sobrescrita.
4. **Rótulo da instância**: se vazio, o servidor usa o **IP público** detectado no start
   (`api.ipify.org` → `checkip.amazonaws.com` → `ifconfig.me`, 5 s cada); o IP também vai sempre
   como atributo `net.public_ip` do resource. Sem rede, o rótulo fica vazio e o `host.name` identifica.

Risco assumido: o token embutido pode ser extraído do binário. Por isso ele é **só de ingest**
no dataset `will-ia` (não lê nada, não administra); se vazar, revogue no Axiom, gere outro, rode
o passo 1 de novo e atualize o segredo do repositório (`gh secret set WILL_TELEMETRY_DEFAULTS_JSON`).

## API

| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/telemetry/status` | `enabled`, `configured`, `provider`, `endpoint`, `dataset`, `instance_id`, `host`, `label` (efetivo), `public_ip`, `sent`, `failed`, `spooled`, `spool_bytes`, `last_sent_at`, `last_error` |
| POST | `/api/telemetry/test` | envia um evento agora; `{ ok, message }` |
| GET | `/api/telemetry/client-config` | destino para o app (`logs_url`, `traces_url`, `headers`, `bearer`, `level`, `label`, `instance_id`); admin |

## Dashboard "will-ia — Operação" (Axiom)

Criado em 2026-10-30 na org `will-ia` (`app.axiom.co/will-ia-1dhc/dashboards`), 14 painéis: instâncias
ativas, turnos, erros e avisos (indicadores); eventos por nível; erros/avisos por módulo; uso por módulo
e spans por tipo (pizzas); mensagens por canal; duração dos turnos p50/p95; custo Claude por instância;
ferramentas mais chamadas; instâncias (última atividade, versão, IP); quedas e relatórios de falha.
A definição está em `docs/telemetria/axiom-dashboard-operacao.json` (campo `dashboard` do
`GET /api/v2/dashboards/uid/<uid>`); para recriar/atualizar, faça `PUT /api/v2/dashboards/uid/<uid>`
com `{ "dashboard": {...}, "version": <int64 atual> }` e o cabeçalho `X-AXIOM-ORG-ID`. O grid tem
12 colunas (`layout[]: {i, x, y, w, h}`).

Mapeamento de campos no Axiom (ingest OTLP): logs → `body`, `severity_text`, `attributes.<campo>`;
spans → `name`, `duration` (timespan), `status.code` (`OK`/`Error`), `attributes.custom.<campo>`
(mapa; acesse com `['attributes.custom'].modulo`); resource → `service.instance.id`,
`service.version`, `resource.host.name`, `resource.os.type`, `resource.process`, `resource.label`,
`resource.net.public_ip`. Campos ainda inexistentes no dataset quebram a consulta: use
`column_ifexists('attributes.x', '')`.

## Consultas úteis no Axiom (APL)

```kusto
// módulo mais usado (spans, sem o ruído das chamadas de API do app)
['will-ia'] | where isnotempty(name) and name != 'api'
| summarize count() by modulo = tostring(['attributes.custom'].module) | order by count_ desc

// erros por instância e módulo
['will-ia'] | where severity_text == 'ERROR'
| summarize count() by ['resource.label'], ['attributes.module'] | order by count_ desc

// um turno do início ao fim (logs + spans com o mesmo trace)
['will-ia'] | where ['attributes.turn_id'] == '<id>' or trace_id == '<trace>' | order by _time asc

// quedas (relatórios de falha)
['will-ia'] | where tostring(column_ifexists('attributes.kind','')) == 'file'
| project _time, ['resource.label'], ['service.version'], ['attributes.file_kind'], ['attributes.content']
```

## Privacidade

Vai: instante, nível, módulo, mensagem (redigida), ids, rótulo, host, SO, versão, duração.
Não vai: tokens/segredos, textos de conversas, e-mails, mensagens do WhatsApp, conteúdo de
arquivos. Nível `debug` pode carregar mais detalhe de dependências — use só enquanto investiga.
