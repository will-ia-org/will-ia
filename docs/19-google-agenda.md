# 19 — Google Agenda (will-google::calendar)

Permite aos agentes ler e criar compromissos nas agendas que o usuário habilitar. Depende de [18 — Conta Google](18-conta-google.md).

## Seleção de agendas (UI)

Seção **Agenda**:

| Campo | Tipo |
|---|---|
| Habilitar módulo | bool |
| Por conta Google conectada: lista de agendas (`calendarList`) | **multi-check** por agenda (nome, cor, fuso, papel: dono/escritor/leitor) |
| Agenda padrão para criação | select entre as habilitadas com papel de escrita |
| Permitir criar/editar eventos | bool (liga escopo `calendar.events`) |
| Permitir excluir eventos | bool (política `confirmar` forçada) |
| Fuso horário padrão | auto (do sistema) / manual |
| Janela máxima por consulta | dias (padrão 62) |

Botão **Atualizar lista** reconsulta `calendarList`. Agendas desmarcadas são **invisíveis** para as ferramentas (não aparecem em `calendar_list_calendars` e qualquer `calendar_id` fora da lista retorna erro).

## Ferramentas MCP

Prefixo `calendar_`. `account` é opcional quando há uma só conta.

| Ferramenta | Entrada | Saída | Política padrão |
|---|---|---|---|
| `calendar_list_calendars` | — | agendas habilitadas (id, nome, fuso, papel, padrão) | auto |
| `calendar_list_events` | `time_min`, `time_max` (ISO-8601), `calendar_ids?`, `query?`, `max_results?` | eventos normalizados (id, agenda, título, início/fim com fuso, dia inteiro?, local, participantes, link, status, recorrência) ordenados por início | auto |
| `calendar_get_event` | `calendar_id`, `event_id` | evento completo | auto |
| `calendar_free_busy` | `time_min`, `time_max`, `calendar_ids?` | blocos ocupados por agenda | auto |
| `calendar_create_event` | `calendar_id?` (padrão), `summary`, `start`, `end` **ou** `date` (dia inteiro), `description?`, `location?`, `attendees?[]`, `reminders?`, `recurrence?`, `conference?: bool` | evento criado (id, link) | **confirmar** |
| `calendar_update_event` | `calendar_id`, `event_id`, campos parciais | evento atualizado | **confirmar** |
| `calendar_delete_event` | `calendar_id`, `event_id` | ok | **confirmar** (toggle off por padrão) |

Normalização: datas sempre com fuso explícito; `start`/`end` aceitam `YYYY-MM-DDTHH:MM` e assumem o fuso padrão; conflitos de horário são detectados antes de criar e retornados como aviso (`conflicts: [...]`) sem bloquear.

Convites a participantes (`attendees`) só são enviados se `send_updates` estiver ligado nas configurações; caso contrário, cria sem notificar.

## Implementação

- Endpoints: `calendarList.list`, `events.list` (`singleEvents=true`, `orderBy=startTime`, `timeMin/timeMax`, `q`), `events.get/insert/patch/delete`, `freeBusy.query`.
- Cache leve em memória da `calendarList` (5 min).
- Erros 403 `insufficientPermissions` → pedir escopo; 404 → agenda/evento inexistente; 429 → backoff.

## Orientação ao agente

No `CLAUDE.md` da KB: "antes de criar um compromisso, verifique conflitos com `calendar_free_busy`; confirme data, hora e fuso com o usuário quando ambíguos; use a agenda padrão salvo pedido contrário".

## Testes

Mock da API com fixtures reais (anonimizadas); testes de normalização de fuso e de dia inteiro; teste de filtragem por agendas habilitadas.
