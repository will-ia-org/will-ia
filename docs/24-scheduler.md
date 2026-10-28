# 24 — Agendamentos (will-scheduler)

Lembretes e tarefas que o agente (ou o usuário) programa para o futuro: **recorrentes** (expressão cron) ou **únicos** (data/hora). Quando chega a hora, o serviço entrega a mensagem do agendamento **na thread que o criou**, pelo stdin do processo `claude`, como se o usuário tivesse escrito; o agente então executa o que ela pede (resumir notícias, mandar um Telegram, checar a agenda…).

É um **módulo de ferramentas + UI**, não um canal: expõe `schedule_*` ao agente, uma página na janela e rotas na API. Não cria threads novas; a única exceção é **recriar** uma thread dona que foi apagada (ver [ADR-0017](adr/0017-scheduler-revive-threads.md)).

Crate: `crates/will-scheduler` (`lib.rs` = módulo e laço, `cron.rs` = cron/fusos, `tools.rs` = ferramentas MCP). Tipos em `will-core::schedule`; repositório em `will-storage::schedules`.

## Modelo de dados

`will_core::Schedule` (tabela `schedules`, migration `0005_schedules.sql`):

| Campo | Tipo | Significado |
|---|---|---|
| `id` | `ScheduleId` (uuid v7) | |
| `name` | texto ≤ 80 | aparece na lista, na notificação e no cabeçalho da mensagem |
| `message` | texto ≤ 8000 | entregue ao agente no disparo |
| `trigger` | `{"kind":"cron","expr":"0 8 * * *"}` ou `{"kind":"once","at":"<ISO UTC>"}` | colunas `trigger_kind` (`cron` \| `once`), `cron_expr`, `run_at` |
| `timezone` | IANA opcional | fuso do cron; `None` = `SchedulerSettings.timezone`, senão fuso do sistema |
| `thread_id` | `ThreadId` | thread dona (**sem FK**: pode ser apagada e recriada; é atualizada ao recriar) |
| `claude_session_id` | opcional | sessão do Claude da thread **na criação/edição**, usada no `--resume` ao recriar |
| `origin` | `Origin` (`origin` + `origin_ref`) | origem da thread na criação (`ui`, `telegram` `<chat_id>[:<topic_id>]`, `realtime`/`voice`) para recriar no mesmo canal/tópico |
| `enabled` | bool | `false` = pausado (manual ou pelo scheduler) |
| `created_by` | `agent` \| `user` | via MCP / via UI ou API |
| `created_at`, `updated_at` | UTC | |
| `next_run_at` | UTC opcional | próximo disparo; recalculado a cada disparo/edição/partida; `None` = nada a fazer |
| `last_run_at`, `last_result` | | `last_result` = `ok` ou motivo da falha |
| `run_count` | int | inclui disparos manuais |
| `paused_reason` | opcional | preenchido quando o scheduler desligou sozinho: `concluído (único)`, `sem próxima ocorrência`, `perdido: o app estava fechado no horário` |

`will_core::ScheduleRun` (tabela `schedule_runs`, `ON DELETE CASCADE`): `id`, `schedule_id`, `at`, `thread_id` (thread que recebeu, pode ser a recriada), `ok`, `detail` (`manual`, ou o erro). Mantém só os **últimos 50** por agendamento; a API/ferramentas devolvem 20.

`ScheduleView` (o que a API e as ferramentas devolvem) = `Schedule` achatado + `trigger_short` (`cron 0 8 * * *` / `once 2026-…Z`), `cron_description` (texto em inglês gerado pelo `croner`, vazio para únicos), `timezone_label`, `next_run_local` (`dd/mm/aaaa HH:MM TZ`), `next_run_in` (`em 2 h 05 min`, `agora`, `atrasado 4 min`), `thread_title`, `thread_exists`.

## Expressões e fusos (`cron.rs`)

- Cron de **5 campos** (`min hora dia mês dia-da-semana`), com segundos opcionais como campo extra **à frente**; parse e próxima ocorrência via crate `croner`. Exemplos: `0 8 * * *` (todo dia 08:00), `30 9 * * 1-5` (dias úteis 09:30).
- Fuso: `Zone::Named(chrono_tz::Tz)` para nomes IANA (`America/Sao_Paulo`) ou `Zone::Local` (fuso do sistema). Nome inválido → erro acionável.
- `run_at` aceita ISO-8601 com fuso (`2026-10-29T10:00:00-03:00`) ou `YYYY-MM-DDTHH:MM[:SS]` / `YYYY-MM-DD HH:MM` **sem fuso** (assume o fuso do agendamento); horário ambíguo/inexistente no fuso (mudança de horário de verão) → erro. Precisa estar pelo menos 5 s no futuro.
- `check_min_interval`: olha as 4 próximas ocorrências e rejeita a expressão se duas delas distam menos de `min_interval_minutes` (`* * * * *` é rejeitado com o padrão de 5 min).

## Ciclo de vida

### Criação / edição

1. Entrada (`ScheduleInput`): `name`, `message`, `cron` **ou** `run_at` (os dois → erro), `timezone?`, `enabled?` (padrão `true`), `thread_id?` (**só UI/API**; o agente sempre usa a thread de onde chamou).
2. Validações: nome e mensagem obrigatórios e dentro dos limites; fuso válido; cron válido e respeitando o intervalo mínimo; `run_at` no futuro; `count_enabled_schedules() < max_schedules`; thread dona existe.
3. Grava `thread_id`, `claude_session_id` e `origin` **da thread naquele momento**; calcula `next_run_at`; emite `Event::SchedulesChanged` e acorda o laço.
4. Edição (`update`): só os campos presentes mudam; trocar `thread_id` (UI/API) regrava sessão e origem da nova thread; `next_run_at` é recalculado; se ficar sem próxima ocorrência o agendamento é pausado com `paused_reason`. Editar um único já concluído com um `run_at` novo o reativa (`enabled: true`).

### Laço de disparo

Um único laço no processo (`run_loop`), ligado quando `SchedulerSettings.enabled` é `true`:

- `fire_due()` lê todos os habilitados, dispara os que têm `next_run_at <= agora` e devolve quanto dormir até o mais próximo, **no máximo 60 s**.
- Dorme com `tokio::select!` entre `CancellationToken`, `Notify` (acordado em qualquer criação/edição/remoção/mudança de configuração) e o `sleep`.
- Status do módulo (`Module::Scheduler`): `Running` com o laço no ar; `Disabled` quando desligado. Desligar/ligar nas configurações para/sobe o laço sem reiniciar o app (`update_config`).

### Disparo (`fire`)

1. Monta o texto com `will_core::schedule::fired_message` (cabeçalho **gerado pelo serviço**, nunca pelo agente) + `message`:
   ```
   [Agendamento "<nome>" disparado em <dd/mm/aaaa HH:MM TZ> (recorrente, cron `<expr>` | único; id <id>). Esta mensagem foi programada anteriormente nesta conversa; execute o que ela pede. Para ajustar ou cancelar, use as ferramentas schedule_*.]

   <message>
   ```
2. `deliver`: `ThreadHost::get_thread(thread_id)`; se a thread não existe mais, **recria** (abaixo). Depois `ThreadHost::send_message(thread, texto)` → vai para o stdin do processo **na hora**, inclusive no meio de um turno (regra 2 do `CLAUDE.md`; o Claude Code incorpora como steering). Thread dormente volta com `--resume`.
3. Calcula a próxima ocorrência (cron) ou `None` (único) e registra em uma transação (`record_schedule_run`): `last_run_at`, `last_result`, `run_count + 1`, `next_run_at`, `thread_id` (atualizado se foi recriada), linha em `schedule_runs`, poda para 50. Único → `enabled = 0`, `paused_reason = "concluído (único)"`; cron sem próxima ocorrência → `"sem próxima ocorrência"`.
4. Emite `Event::ScheduleFired { schedule_id, thread_id, name, ok, detail }` e `Event::SchedulesChanged`; log `info`/`error` na família `scheduler`.
5. **Notificação nativa** (`Event::Notification`, título "Agendamento") se `notify_on_fire` e a origem da thread **não** é Telegram (no Telegram a resposta do agente já vai para o tópico). Também avisa quando a entrega falhou.

Se a entrega falha (sem thread e sem conseguir recriar, motor indisponível), o disparo é registrado como `ok = false` com o motivo em `last_result`/`detail`; um cron segue para a próxima ocorrência, um único é dado como concluído. A resposta do agente ao disparo aparece na thread (UI) e, se a thread é do Telegram, no tópico, como qualquer turno.

### Thread dona apagada → recriação

Se `get_thread` falha no disparo, o scheduler chama `ThreadHost::revive_thread(&origin, Some(&name), claude_session_id)`:

- O `Host` **sempre cria** uma thread nova (diferente de `ensure_thread`, que reaproveita por origem) com a mesma `origin` (UI, o mesmo `<chat_id>:<topic_id>` do Telegram, ou voz), título = nome do agendamento, e grava o `claude_session_id` salvo; emite `ThreadCreated`.
- A mensagem é enviada à thread nova; o motor tenta `--resume <session_id>`. Se a sessão também **expirou** (`No conversation found with session ID`, ver `docs/05`), o fluxo normal de sessão expirada esquece a sessão, respawna do zero e reenvia a mensagem: o agente perde o contexto antigo, mas **a mensagem é entregue** (por isso a orientação de escrever `message` como instrução completa).
- `schedules.thread_id` passa a apontar para a thread nova (`COALESCE` em `record_schedule_run`). Para threads do Telegram, o módulo Telegram volta a encontrar a thread pela origem (`ensure_thread` procura por `origin`/`origin_ref`), então a conversa do tópico continua nela.

### Disparos perdidos (app fechado)

Na partida do laço (`reconcile_on_start`), para cada agendamento habilitado:

- **cron**: recalcula `next_run_at` **a partir de agora**; se o `next_run_at` gravado já passou, registra `warn` "disparo de … perdido com o app fechado; próximo …" e segue. Nada é disparado retroativamente.
- **único** com `run_at` no passado: pausa (`enabled = 0`, `paused_reason = "perdido: o app estava fechado no horário"`) com `warn`. O usuário vê o motivo na lista e pode reagendar.

A mesma regra vale quando o scheduler fica desligado nas configurações por um tempo: ao religar, o laço reconcilia do mesmo jeito.

### Disparar agora (`run_now`)

Entrega a mensagem imediatamente com o cabeçalho marcado `(manual)`, grava um run com `detail = "manual"` e **não** altera `next_run_at` nem o estado (`enabled`/`paused_reason`) do agendamento; funciona inclusive em agendamentos pausados/concluídos.

## Ferramentas MCP

Família `scheduler` (`will_scheduler::tools::SchedulerProvider`), visível só com o módulo habilitado; exigem `Module::Scheduler`. Todas nascem com política **`auto`**: agendar não tem efeito externo; as ferramentas que o agente usar **no disparo** (`telegram_send_message`, `gmail_send`…) continuam sujeitas às próprias políticas (regra 9). O `id` das ferramentas aceita o uuid completo **ou um prefixo único** (≥ 4 caracteres).

| Ferramenta | Entrada | Saída | Política |
|---|---|---|---|
| `schedule_create` | `name`, `message` (obrigatórios), `cron` **ou** `run_at`, `timezone?` | `{ ok, schedule: ScheduleView, hint }` | auto |
| `schedule_list` | `all?: bool` (padrão: só os da thread chamadora) | `{ count, schedules: [ScheduleView] }` | auto |
| `schedule_get` | `id` | `{ schedule: ScheduleView, runs: [ScheduleRun] }` (últimos 20) | auto |
| `schedule_update` | `id` + qualquer de `name`, `message`, `cron`/`run_at`, `timezone`, `enabled` (`false` pausa, `true` retoma) | `{ ok, schedule }` | auto |
| `schedule_delete` | `id` | `{ ok, deleted }` | auto |

- `schedule_create` **exige thread chamadora** (`ToolContext.thread_id`): com o token para scripts (chamador genérico) retorna erro, pois o disparo não teria para onde voltar. A thread dona é sempre a da chamada; o agente não escolhe outra.
- `approval_summary` (usado se o usuário subir a política para `confirmar`): "Agendar \"<nome>\" (cron …|em …)", "Alterar agendamento <id>", etc.
- Erros de validação voltam como `InvalidArgs` com a mensagem em pt-BR (`intervalo mínimo entre disparos é de 5 min…`, `limite de 100 agendamentos ativos atingido…`, `fuso horário desconhecido…`).

## Configurações (`SchedulerSettings`, seção `scheduler`)

| Campo | Tipo | Padrão | Efeito |
|---|---|---|---|
| `enabled` | bool | `true` | sobe/para o laço; ferramentas somem do `tools/list`; `create` recusa |
| `notify_on_fire` | bool | `true` | notificação nativa ao disparar em threads de origem UI/voz |
| `timezone` | IANA opcional | `None` (fuso do sistema) | fuso padrão dos crons e de `run_at` sem fuso |
| `max_schedules` | int | `100` | limite de agendamentos **habilitados** (proteção contra agente em loop) |
| `min_interval_minutes` | int | `5` | intervalo mínimo entre disparos de um mesmo cron (`0` desliga a checagem) |

`PUT /api/settings { "section": "scheduler", … }` aplica a quente (`start_scheduler` → `update_config`).

## UI

> Implementado em 2026-10-28: página **Agendamentos** (`crates/will-desktop/src/pages/schedules.rs`) e seção Configurações → Agendamentos (`pages/settings/scheduler.rs`), com prévia do cron por `/api/schedules/preview`. Ainda não validado interativamente pelo usuário.

**Página Agendamentos** (navegação lateral; recarrega em `SchedulesChanged`/`ScheduleFired`):

- Lista: nome, gatilho (`trigger_short` + `cron_description`), fuso, **próximo disparo** (`next_run_local` + `next_run_in`), thread dona (`thread_title`; aviso quando `thread_exists = false`: "será recriada no disparo"), último disparo e resultado, `run_count`, estado (ativo / pausado com `paused_reason`), `created_by`.
- Ações por item: editar, pausar/retomar (`enabled`), **Disparar agora** (`run_now`), excluir (confirmação), ver histórico (`runs`).
- Formulário (criar/editar): nome, mensagem (textarea), tipo (recorrente/único), expressão cron **com prévia** (`GET /api/schedules/preview` enquanto digita: descrição, fuso e as próximas 5 ocorrências; erro de intervalo mínimo inline) ou data/hora única, fuso (vazio = padrão), thread dona (select entre as threads existentes; obrigatório na UI), habilitado.
- Ícone "?" (`help.rs`) explicando cron de 5 campos, fusos e disparos perdidos.

**Configurações → Agendamentos**: os cinco campos de `SchedulerSettings` acima; bolinha do módulo verde (`Running`) ou cinza (`Disabled`).

## API local

Ver [22](22-api-local.md). Corpo de `POST`/`PUT` = `ScheduleInput`; `503` se o módulo não iniciou.

| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/schedules?thread_id=` | `{ schedules: [ScheduleView], enabled }` |
| POST | `/api/schedules` | `ScheduleInput` com `thread_id` **obrigatório**; `created_by = "user"` → `ScheduleView` |
| GET | `/api/schedules/preview?cron=&timezone=&n=` | `{ description, timezone, next: [{ at, local, in }], error }` (`n` 1–20, padrão 5; `error` = intervalo mínimo) |
| GET | `/api/schedules/{id}` | `{ schedule, runs }` (404 se não existe) |
| PUT | `/api/schedules/{id}` | campos parciais (`ScheduleInput`, inclusive `thread_id`) → `ScheduleView` |
| DELETE | `/api/schedules/{id}` | `{ ok }` |
| POST | `/api/schedules/{id}/run` | dispara agora → `{ ok }` |

Eventos no WS `/api/events`: `ScheduleFired`, `SchedulesChanged`, `Notification`, `ModuleStatus { module: "scheduler" }`.

## Orientação ao agente

No `CLAUDE.md` da KB (`docs/06`) e na descrição das ferramentas:

- Escreva `message` como uma **instrução completa para você mesmo**: o que fazer, com quais fontes/ferramentas e para quem entregar ("Resuma as notícias de tecnologia de hoje e envie no tópico do Telegram desta conversa"). No disparo, o contexto pode ter sido compactado ou a thread recriada; a mensagem precisa bastar sozinha.
- Ao receber uma mensagem com cabeçalho `[Agendamento … disparado em …]`, execute o pedido; não é o usuário ao vivo. Para ajustar ou cancelar, use `schedule_update`/`schedule_delete` com o `id` do cabeçalho.
- Depois de `schedule_create`, confirme ao usuário **o que** foi agendado e **quando** (`next_run_local`), citando o fuso.
- Para "lembre-me daqui a 2 horas", calcule o `run_at` em ISO com fuso; para rotinas, prefira cron de 5 campos; respeite o intervalo mínimo.
- Pausar é `schedule_update { enabled: false }`; `schedule_delete` é definitivo.

## Limites e segurança

- Limites: `max_schedules` habilitados (padrão 100), `min_interval_minutes` (padrão 5), nome ≤ 80 e mensagem ≤ 8000 caracteres, prévia de no máximo 20 ocorrências, 50 runs por agendamento.
- O cabeçalho do disparo é gerado pelo serviço; o `message` é texto do agente/usuário e é tratado como **conteúdo**, não como instrução para o serviço (regra 11): o scheduler nunca interpreta o texto, só o entrega.
- O agente só agenda **na própria thread**; não consegue apontar para outra thread nem para um tópico/chat diferente do de origem. Só UI/API trocam `thread_id`.
- Recriar thread usa só `origin` e `claude_session_id` gravados pelo serviço; nada vem de entrada externa.
- Sem efeito externo próprio: tudo o que o agente fizer no disparo passa pelas políticas das demais ferramentas e pelas aprovações (`docs/09`, ADR-0013).
- Falha do módulo (banco, host) vira log `error` e o laço continua; nunca derruba o app (regra 7).

## Testes

Unitários em `crates/will-scheduler/src/cron.rs`:

| Teste | Garante |
|---|---|
| `cron_next_in_named_zone` | `0 8 * * *` em `America/Sao_Paulo` após 09:00 locais → 08:00 do dia seguinte (11:00Z) |
| `rejects_bad_cron_and_zone` | expressão inválida e fuso desconhecido são rejeitados; fuso vazio = sistema |
| `min_interval` | `* * * * *` falha com mínimo 5 min; `*/10 * * * *` passa |
| `run_at_formats` | `YYYY-MM-DDTHH:MM` sem fuso assume o fuso dado; ISO com offset equivale; texto livre falha |
| `humanize` | `em 5 min`, `em 1 d 2 h`, `atrasado 3 min` |

A fazer (`docs/23`): teste do laço com `Storage` em arquivo temporário e `ThreadHost` falso (disparo, perdido na partida, recriação de thread), e `smoke.sh` criando um `run_at` curto e conferindo a mensagem no `fake-claude`.
