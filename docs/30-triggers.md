# 30 — Triggers (will-triggers)

Módulo **universal** de gatilhos: uma **fonte de evento** passa por um **filtro** e, se casar, **acorda uma thread** do Claude Code entregando o **prompt configurado + todo o contexto do evento**. É *fire-and-forget*: o serviço entrega a mensagem (`ThreadHost::send_message_from`) e segue — ninguém espera a resposta; se o processo `claude` da thread estiver dormindo, o motor o sobe para processar.

O agendamento por tempo (`docs/24`, `will-scheduler`) é o "trigger de relógio": mesma saída (thread + mensagem), outra entrada. Na UI os dois moram na página **Triggers** (abas *WhatsApp e E-mail* e *Agendamentos*).

## Modelo (`will_core::trigger`)

| Campo | Significado |
|---|---|
| `source` | `whatsapp` \| `gmail` (`TriggerSource`); novas fontes = novo valor + produtor de eventos |
| `filter` (`TriggerFilter`) | **escopo** por fonte + **condições** genéricas |
| `filter.account` | conta da fonte (id/e-mail); vazio = qualquer |
| `filter.chats[]` (WhatsApp) | `{jid, name}` escolhidos; vazio = qualquer conversa 1:1 |
| `filter.include_groups` / `include_from_me` (WhatsApp) | aceitar grupos / minhas próprias mensagens (padrão: não) |
| `filter.gmail_query` (Gmail) | busca do Gmail; vazio = `in:inbox` |
| `filter.conditions[]` | `{field, glob, negate}`, todas em **E**, sobre os campos do evento |
| `prompt` | instrução entregue à thread |
| `thread_id` / `thread_title` | thread dona; `NULL` = criar no primeiro disparo (com o título) e fixar |
| `claude_session_id` / `origin` | para recriar a thread com `--resume` se ela for apagada |
| `cooldown_secs` | intervalo mínimo entre dois disparos da mesma trigger (anti-rajada) |
| `debounce_secs` (WhatsApp) | **agrupamento**: espera N segundos sem mensagem nova **na conversa** e dispara uma vez com todas as mensagens acumuladas (`merge_events`: corpo `[HH:MM] texto` por mensagem, campo `messages` = total); teto `triggers.debounce_max_wait_secs` desde a primeira; 0 = cada mensagem dispara |
| `filter.include_status` (WhatsApp) | aceitar atualizações de status do contato (`status@broadcast`); padrão **não** |
| `cursor` | posição de leitura da fonte (Gmail: `internalDate` ms do e-mail mais novo visto) |
| `last_fired_at`, `last_result`, `fire_count`, `paused_reason` | estado/histórico |

**Evento normalizado** (`TriggerEvent`): `source`, `account`, `at`, `summary` (1 linha), `fields: [(nome, valor)]` na ordem de exibição, `body` (corpo longo), `ref_id` (id na fonte), e flags do WhatsApp (`from_me`, `is_group`, `chat_jid`). Campos por fonte:

- **WhatsApp**: `from` (JID), `from_name`, `from_phone`, `chat`, `chat_jid`, `is_group`, `from_me`, `kind` (`text|image|audio|…`), `text` (= corpo).
- **Gmail**: `from`, `from_name`, `to`, `cc`, `subject`, `date`, `labels`, `gmail_message_id`, `gmail_thread_id`, `attachments` (nomes), `snippet`, `text` (= corpo em texto, aspas/citações removidas, até `gmail.max_body_kb`).

**Glob** (`glob_match`): sem distinção de maiúsculas; `*` qualquer trecho, `?` um caractere; casa em **qualquer posição** (`*@xpto.com` acha `Fulano <joao@xpto.com>`); `^` e `$` ancoram; padrão sem curinga = "contém". `negate` inverte.

## Mensagem entregue (`fired_message`)

```
[Trigger "<nome>" (<fonte>) disparada em <dd/mm/aaaa HH:MM>; id <uuid>. Esta mensagem foi gerada por uma regra …]

## Prompt da trigger
<prompt configurado>

## Contexto do evento
- fonte: WhatsApp
- conta: …
- quando: <RFC 3339>
- from: … / from_name: … / chat: … / subject: … (todos os campos não vazios)
- id na fonte: …

## Conteúdo
<corpo: texto da mensagem / do e-mail>
```

O agente recebe **tudo**: localizar a conversa/e-mail e agir (responder pelo `whatsapp_send_message`, `gmail_send`, registrar na KB…) é decisão dele conforme o prompt. O autor do turno é `trigger:<fonte>`.

## Fontes

### WhatsApp (imediato)
`will-whatsapp::index_message` emite `Event::WhatsAppMessage { account_id, msg_id, chat_jid, chat_name, sender_jid, sender_name, sender_phone, from_me, is_group, kind, text, at }` a cada mensagem indexada. O módulo de triggers assina o barramento, converte em `TriggerEvent` e chama `dispatch(WhatsApp, ev)`: toda trigger habilitada da fonte cujo filtro casa dispara (cada uma na sua thread).

### Gmail (varredura)
Laço a cada `triggers.gmail_poll_minutes`: agrupa as triggers habilitadas por (conta, query) — conta vazia = **todas as contas conectadas**, uma consulta por conta e **cursor por conta** (`cursor` em JSON `{"email": ms}`; o formato antigo, um número, segue aceito) — consulta uma vez por grupo (`GoogleModule::gmail_poll` → lista + `detail(text)` + `internal_date_ms`) e, por trigger, considera só e-mails com `internal_date_ms > cursor` (mais antigos primeiro, até `gmail_max_per_poll`). **Primeira varredura só inicializa o cursor** (não dispara pelo que já estava na caixa). `POST /api/triggers/gmail/poll` força uma varredura. Sem módulo Google (feature `google` ou sem conta), a fonte fica indisponível e a UI avisa.

### Tempo
`docs/24`. Continua em `schedules`/`will-scheduler`, com as ferramentas `schedule_*`.

## Disparo (`fire`)
1. **Cooldown**: se `last_fired_at` está dentro de `cooldown_secs`, ignora (log `debug`).
2. **Thread**: `thread_id` existente → usa; ausente/apagada → `ThreadHost::revive_thread(origin, título, claude_session_id)` cria (`Origin::Ui` quando nunca teve thread) e grava `thread_id`/sessão na trigger.
3. **Entrega**: `send_message_from(thread, fired_message, [], "trigger:<fonte>")` — o motor sobe o processo se preciso. Não espera a resposta.
4. **Registro**: `trigger_runs` (últimos 50), `last_fired_at`/`last_result`/`fire_count`; eventos `TriggerFired` e `TriggersChanged`; notificação nativa se `notify_on_fire`.

`Testar` (UI / `POST /api/triggers/{id}/test`) dispara com um evento de exemplo da fonte, ignorando filtro e cooldown.

## Persistência (`docs/11`, migration `0008_triggers.sql`)
Tabelas `triggers` (uma linha por regra, `filter_json`) e `trigger_runs` (histórico curto, `ON DELETE CASCADE`).

## Configurações (`TriggersSettings`, seção `triggers`; `docs/10`)
`enabled` · `notify_on_fire` · `gmail_poll_minutes` (5) · `gmail_max_per_poll` (10) · `max_triggers` (200) · `default_cooldown_secs` (0). Mudar a seção aplica sem reiniciar o serviço (`start_triggers` → `update_config`; o laço do Gmail reinicia se o intervalo mudou).

## API local (`docs/22`)
`GET/POST /api/triggers` · `GET/PUT/DELETE /api/triggers/{id}` · `GET /api/triggers/{id}/runs` · `POST /api/triggers/{id}/test` · `POST /api/triggers/gmail/poll` · `GET /api/whatsapp/chats?account=&q=&limit=` (contas pareadas + conversas/contatos do índice, para o seletor da UI).

## UI (`crates/will-app/src/pages/triggers.rs`)
Cartões (toggle pausar/retomar, fonte, escopo resumido, thread, contagem/último disparo, falha em vermelho; expandido: prompt, condições, Testar/Editar/Excluir, últimos disparos) e formulário: nome, fonte, habilitada; escopo (WhatsApp: conta, **seletor de contatos** com busca no índice e chips, grupos, minhas mensagens; Gmail: conta, busca); condições (campo da fonte + glob + "não"); thread (existente ou **criar nova** com título); prompt; cooldown. Configurações → Triggers: seção *WhatsApp e E-mail* + *Agendamentos (por tempo)*.

## Não é exposto ao MCP
As triggers de WhatsApp/e-mail são mantidas **só pela UI/API** (dono). O agente não tem ferramentas `trigger_*`; as únicas ferramentas de gatilho do agente continuam sendo as `schedule_*` do agendamento por tempo (`docs/24`).

## Limites e segurança
- Só mensagens **recebidas** disparam por padrão (`include_from_me` desligado) e grupos só quando permitido — evita loops com as próprias respostas do agente.
- `max_triggers` ativas; `cooldown` por regra; Gmail limitado por varredura.
- O evento inteiro vai para a thread (incluindo corpo do e-mail/mensagem): é o mesmo dado que o agente já alcança pelas ferramentas `whatsapp_*`/`gmail_*`.
- Fonte Gmail depende de conta Google com escopo Gmail (`docs/20`).

## Testes
`will-core::trigger` (glob, filtro, mensagem), `will-storage::triggers` (CRUD/disparo), `will-triggers` (conversão WhatsApp/Gmail → evento). Manual: criar uma trigger de WhatsApp para um contato com "criar nova", mandar uma mensagem desse contato e ver a thread nascer com o prompt + contexto; `Testar` em uma de Gmail.
