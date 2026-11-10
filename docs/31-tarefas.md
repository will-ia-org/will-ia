# 31 — Tarefas (painel Gantt)

Módulo de tarefas do dia a dia: itens com **título**, **descrição**, **início com hora**
(obrigatório), **fim** (opcional), **prioridade** e **conclusão**, vistos num **Gantt** no app
(primeiro item do menu) e manipulados pelo agente pelas ferramentas `tasks_*`. Crate `will-tasks`,
página `pages/tasks.rs`, seção Configurações › Tarefas.

## Modelo (`will_core::task`)

| Campo | Tipo | Regra |
|---|---|---|
| `title` | texto ≤ 120 | obrigatório |
| `description` | texto ≤ 8000 | livre |
| `start_at` | instante UTC | obrigatório, **sempre com hora** (só a data é recusada) |
| `end_at` | instante UTC opcional | precisa ser depois do início; `""` na edição limpa |
| `priority` | `high` (1) · `medium` (2, padrão) · `low` (3) | aceita alta/média/baixa e 1/2/3 na entrada |
| `done_at` | instante opcional | fechada: `tasks_complete` / botão ✓ (concluída) ou `tasks_skip` / botão Não realizada; `tasks_reopen` limpa |
| `skipped` | bool | com `done_at`: fechada como **não realizada** (faltou/desistiu) — some da tela como a concluída, mas não conta como feita |
| `notified_start_at`, `notified_overdue_at` | instantes | marcas do laço (um aviso por transição) |
| `calendar_id`, `event_id`, `synced_at`, `remote_updated_at` | vínculo opcional | evento espelhado no Google Calendar (ver "Sincronizar") |

**Estado derivado do relógio** (`Task::state_at(now)`), nunca gravado:

```
done_at presente e skipped .................. skipped   (cinza, riscada; "não realizada")
done_at presente ............................ done      (verde)
now < start_at .............................. upcoming  (cinza)
end_at presente e now >= end_at ............. overdue   (vermelho)
senão ....................................... running   (azul)
```

Tarefa **sem fim nunca atrasa**: fica em andamento até ser concluída. Prioridade e atraso são
independentes (atrasada pode ser de prioridade baixa).

Datas de entrada: ISO-8601 com fuso (`2026-11-10T14:00:00-03:00`, `…Z`) ou `YYYY-MM-DDTHH:MM[:SS]` /
`YYYY-MM-DD HH:MM` no fuso de `tasks.timezone` (padrão `America/Sao_Paulo`; nome inválido → fuso do
sistema). As visões (`TaskView`) devolvem `start_local`/`end_local` (`YYYY-MM-DDTHH:MM` no fuso) e
rótulos `dd/mm HH:MM`; a lista HTTP traz `offset_minutes` para o app desenhar o eixo.

## Laço de observação (`TasksModule::run_loop`)

A cada **30 s** lista as pendentes e nota transições:

- entrou em **andamento** (`running`/`overdue` sem `notified_start_at`) → grava a marca e, com
  `notify_start`, avisa "▶️ Tarefa em andamento";
- entrou em **atraso** (`overdue` sem `notified_overdue_at`) → grava a marca e, com `notify_overdue`,
  avisa "⏰ Tarefa atrasada".

Nas duas transições, com `notify_native` (padrão **ligado**), o módulo emite `Event::Notification`
(notificação nativa do sistema, como triggers e aprovações). Os avisos no Telegram vão pelo [`TaskNotifier`] injetado pelo servidor (`TelegramTaskNotifier`): módulo Telegram
no ar → `TelegramSender::send_text` no **destino padrão** (`default_chat_id`/`default_topic_id` ou o
primeiro chat da allowlist), HTML básico. Sem Telegram, só log. As marcas são gravadas mesmo com as
flags desligadas (ligar depois não dispara avisos retroativos). Editar início/fim zera as marcas;
reabrir zera só a de atraso. Qualquer transição emite `Event::TasksChanged` (a UI recarrega).

## Ferramentas MCP (família `tasks`, política padrão **auto**, requer `Module::Tasks`)

| Ferramenta | Entrada | Efeito |
|---|---|---|
| `tasks_create` | `title`, `start_at` (obrig.), `description`, `end_at`, `priority` | cria |
| `tasks_list` | `filter`: `pending` (padrão) · `running` · `overdue` · `upcoming` · `done` · `skipped` · `all` | lista `TaskView`s + `timezone` |
| `tasks_get` | `id` (completo ou prefixo único ≥ 4) | detalha |
| `tasks_update` | `id` + campos (só os enviados mudam; `end_at: ""` limpa) | altera |
| `tasks_delete` | `id` | remove |
| `tasks_complete` / `tasks_skip` / `tasks_reopen` | `id` | conclui / fecha como não realizada / reabre |

Com `tasks.enabled = false` o provider some do MCP e as chamadas devolvem `Unavailable`.

## API local

| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/tasks?filter=` | `{tasks, enabled, timezone, offset_minutes, now}` |
| POST | `/api/tasks` | corpo `TaskInput` |
| GET / PUT / DELETE | `/api/tasks/{id}` | detalhar / alterar / remover |
| POST | `/api/tasks/{id}/complete` · `/skip` · `/reopen` | concluir / não realizada / reabrir |

| POST | `/api/tasks/sync-calendar` | `{from, to, calendar_id}` → `SyncReport` |

Configurações: seção `tasks` (`enabled`, `timezone`, `notify_native`, `notify_start`, `notify_overdue`, `sort`, `sync_calendar_id`) por
`PUT /api/settings`; `apply_section("tasks")` reaplica no módulo a quente.

## Painel (app, `pages/tasks.rs`)

- Lista à esquerda (bandeira de prioridade 🔴🟡🟢 pintada, título, bolinha do estado) e **linha do
  tempo** à direita, mesma altura de linha; cabeçalho fixo com dias (fim de semana sombreado, hoje em
  destaque) e ticks de hora conforme o zoom; **linha do agora** em azul com o horário.
- **Zoom** por rolagem / pinça / botões − + (meio dia a **2 anos** visíveis; o eixo troca de escala: ≥ 30 px/dia = dias com ticks de hora, ≥ 7 px/dia = semanas seg–dom com o número ISO e os dias embaixo, abaixo disso = meses); **arrastar** o fundo
  navega; rolagem horizontal também; **Hoje** reposiciona (agora a 30 % da largura).
- Barra com cor do estado; sem fim, a barra vai até o agora com a ponta esvaecida; fora da janela, uma
  seta na borda indica o lado. Tooltip com título, período, estado, prioridade e descrição.
- Passar o mouse mostra o **círculo ✓** na ponta da barra (concluir; nas concluídas, ↺ reabrir).
  Clicar na barra ou na linha abre o formulário (título, descrição, início, fim, prioridade,
  Concluir/Reabrir, Excluir com confirmação).
- **Concluídas somem por padrão**; o toggle "mostrar concluídas" busca `all`.
- **Ordenação** (seletor): data de início (padrão; atrasadas primeiro por serem mais antigas), ordem
  alfabética, prioridade (alta → média → baixa, empate pela data). A escolha vai para
  `settings.tasks.sort` (persistida).
- O estado é recalculado no app pelo relógio (repaint a cada 30 s), então a cor muda sem recarregar.

## Sincronizar com o Google Calendar (botão em Configurações › Tarefas)

Ação explícita (não é automática) sobre **uma agenda** e **uma janela de datas** (padrão: hoje →
+2 meses; editável antes de clicar). Crate `will-tasks/sync.rs`; o servidor liga a ponte
[`CalendarBridge`] ao módulo Google (`GoogleCalendarBridge` → `GoogleModule::{sync_calendar,
calendar_events_raw, calendar_event_raw, calendar_insert_raw, calendar_patch_raw, calendar_delete_raw}`,
sem política de ferramenta — é o usuário clicando). Decisões de 2026-11-10 (perguntas ao Will):

| Decisão | Regra |
|---|---|
| Agenda | **uma só**: `tasks.sync_calendar_id` (vazio = a "padrão pra criar" de Google › Agenda; senão a única gravável). Precisa estar habilitada e ser gravável. |
| Identidade (nunca duplica) | a tarefa guarda `calendar_id` + `event_id`; o evento guarda `extendedProperties.private.will_task_id`. Se o vínculo local se perdeu (restore em outra máquina), a propriedade recupera. |
| Conflito | **última edição vence**: `updated_at > synced_at` ⇒ editada aqui; `updated` do evento ≠ `remote_updated_at` ⇒ editado lá; os dois ⇒ compara `updated_at` × `updated`. |
| Exclusão | **nos dois sentidos**: evento vinculado sumiu (404/cancelado) ⇒ tarefa apagada na sincronização; tarefa apagada no app/`tasks_delete` ⇒ evento apagado **na hora**; se o Google falhar, lápide em `task_tombstones` (a próxima rodada tenta de novo e não reimporta). |
| Concluída | título do evento vira `✓ título` (+ `will_done_at`); reabrir tira. Evento com `✓ ` importa já concluída. |
| Não realizada | título vira `✗ título` (+ `will_skipped = "1"`); evento com `✗ ` importa fechada como não realizada. |
| Prioridade | `will_priority` (high/medium/low) na propriedade privada; evento sem ela = média. |
| Sem fim | o Google exige fim: envia início + 1 h com `will_open_end = "1"`; na volta, fim igual a início + 1 h com a marca ⇒ sem fim. Se você mudar o fim no Google, ele passa a valer. |
| Dia inteiro | evento `date` ⇒ tarefa 00:00 → 23:59 (fuso das tarefas); tarefa 00:00 → 23:59 ⇒ evento de dia inteiro. |
| Recorrência | `singleEvents=true`: cada ocorrência da janela vira uma tarefa (id `série_AAAAMMDDTHHMMSSZ`); editar a tarefa altera só aquela ocorrência. |
| Ignorados | cancelados, convites que **eu recusei**, eventos cujo `will_task_id` aponta para tarefa que ainda existe (em outra agenda/fora da janela). |

Algoritmo (`TasksModule::sync_calendar`, uma rodada por vez): 1) lápides → `delete` no Google;
2) lista as ocorrências da janela; 3) tarefas locais = vinculadas a esta agenda ∪ soltas com início na
janela: solta → `insert` + vínculo; vinculada sem evento (nem por `get`) → apaga a tarefa; par
→ regra de conflito (`patch` ou `apply_remote_task`); 4) eventos sem par → `insert_task` já vinculada
(`created_by = calendar`, `created_at = updated_at = synced_at`). Relatório `SyncReport` (criadas /
atualizadas / removidas em cada sentido, sem alteração, ignorados, avisos) mostrado na UI e no log.

Rotas: `POST /api/tasks/sync-calendar` `{from, to (YYYY-MM-DD), calendar_id}` (todos opcionais; janela
máxima 400 dias). App: `settings/tasks.rs`, aba **Google Calendar** (ao lado de Configuração) — estado da Agenda, seletor de agenda (habilitadas e
graváveis), dois calendários (`DatePickerButton`), botão **Sincronizar agora** e o relatório da última
rodada; o formulário da tarefa mostra a pílula "Google Agenda" quando vinculada.

## Limites

- Sem dependências entre tarefas, sem recorrência (pra isso há o scheduler, `docs/24`) e sem arrastar a
  barra pra mudar datas (decisão de 2026-11-10: clicar abre o formulário).
- Avisos no Telegram só no destino padrão; a notificação nativa é por evento, sem canal extra.
