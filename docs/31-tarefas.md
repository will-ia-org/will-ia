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
| `tags` | lista de strings (JSON na coluna `tags`, migration 0013) | **normalizadas** ao salvar (`will_core::task::normalize_tag`): minúsculas, sem acento (`ç`→`c`), separadores/espaços viram `-`, só `[a-z0-9._-]`, ≤ 40 chars, sem repetição; na edição a lista é substituída inteira (`[]` limpa). Não vão para o Google Calendar |
| `done_at` | instante opcional | fechada: `tasks_complete` / botão ✓ (concluída) ou `tasks_skip` / botão Não realizada; `tasks_reopen` limpa |
| `skipped` | bool | com `done_at`: fechada como **não realizada** (faltou/desistiu) — some da tela como a concluída, mas não conta como feita |
| `notified_start_at`, `notified_overdue_at` | instantes | marcas do laço (um aviso por transição) |
| `calendar_id`, `event_id`, `synced_at`, `remote_updated_at` | vínculo opcional | evento espelhado no Google Calendar (ver "Sincronizar") |
| `event_read_only` | bool | o evento espelhado é especial do Google (aniversário, Gmail, fora do escritório, foco): só recebe, nunca é escrito |

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
| `tasks_create` | `title`, `start_at` (obrig.), `description`, `end_at`, `priority`, `tags[]` | cria |
| `tasks_list` | `filter`: `pending` (padrão) · `running` · `overdue` · `upcoming` · `done` · `skipped` · `all`; `tags[]` = só tarefas com **qualquer** uma delas | lista `TaskView`s + `timezone` + `tags` (todas em uso, ordem alfabética) + `filter_tags` |
| `tasks_get` | `id` (completo ou prefixo único ≥ 4) | detalha |
| `tasks_update` | `id` + campos (só os enviados mudam; `end_at: ""` limpa; `tags` substitui a lista) | altera |
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

Configurações: seção `tasks` (`enabled`, `timezone`, `notify_native`, `notify_start`, `notify_overdue`, `sort`, `sync_calendar_id`, `soon_minutes`, `soon_beep_seconds`) por
`PUT /api/settings`; `apply_section("tasks")` reaplica no módulo a quente.

## Painel (app, `pages/tasks.rs`)

- Lista à esquerda (bandeira de prioridade 🔴🟡🟢 pintada, título, bolinha do estado) e **linha do
  tempo** à direita, mesma altura de linha; cabeçalho fixo com dias (fim de semana sombreado, hoje em
  destaque) e ticks de hora conforme o zoom; **linha do agora** em azul com o horário.
- **Zoom** por rolagem / pinça / botões − + (só quando o ponteiro está sobre o Gantt **e** nada está aberto por cima — dropdown, popup ou modal — senão a rolagem é do que está em cima; meio dia a **2 anos** visíveis; o eixo troca de escala: ≥ 30 px/dia = dias com ticks de hora, ≥ 7 px/dia = semanas seg–dom com o número ISO e os dias embaixo, abaixo disso = meses); **arrastar** o fundo
  navega; rolagem horizontal também; **Hoje** reposiciona (agora a 30 % da largura).
- Barra com cor do estado; sem fim, a barra vai até o agora com a ponta esvaecida; fora da janela, uma
  seta na borda indica o lado. Tooltip com título, período, estado, distância relativa, prioridade e descrição.
- **Quanto falta / há quanto tempo** (desde 0.1.25): texto cinza e curto fora da barra, sem segundos
  (`relative_short`: "agora", "em 5 min", "há 2 h", "em 3 d", "há 2 sem", "em 1 mês", "há 4 meses"; uma
  unidade só, arredondada para baixo). Ainda não começou → "em X" à **esquerda** da barra (até o início);
  em andamento → "há X" à esquerda (desde o início); já acabou ou concluída → "há X" à **direita**
  (contado do fim). Se não couber na janela do lado preferido, vai para o outro; se não couber em nenhum,
  some. Some também enquanto o mouse está na barra (ali entram os horários exatos). As **setas** de
  tarefa fora da janela ganham o mesmo texto ao lado. Atualiza no repaint de 30 s.
  **Está chegando** (desde 0.1.26): tarefa aberta que começa em **`tasks.soon_minutes` ou menos** (padrão 30; 0 desliga; campo em Configurações › Tarefas; `starting_soon`) tem o
  rótulo em amarelo com uma pílula translúcida pulsando atrás (ciclo de ~1,6 s; repaint a cada 50 ms só
  enquanto houver algum na tela), tanto fora da barra quanto ao lado da seta. Só para as que **ainda não
  começaram**; atrasadas não pulsam. O item **Tarefas** do menu ganha um **badge amarelo pulsante** com a
  contagem (`TasksPage::soon_count`, `ui::NavBadge::soon`), à esquerda dos badges de atrasadas e em andamento (pulso forte: dois anéis que abrem e somem). O **bipe** é do **serviço** (toca com o app fechado): `TasksModule::beep_loop` conta as tarefas `Task::starts_within` e chama
  `TaskNotifier::soon_beep` → `will_platform::sound::play_cue(CUE_SOON, volume 0,32)` na saída `realtime.output_device`
  (ou a padrão), na entrada da primeira tarefa na janela e a cada `tasks.soon_beep_seconds` (padrão 30; 0 desliga).
- Passar o mouse mostra o **círculo ✓** na ponta da barra (concluir; nas concluídas, ↺ reabrir).
  Clicar na barra ou na linha abre o formulário (título, descrição, início, fim, prioridade,
  Concluir/Reabrir, Excluir com confirmação).
- **Concluídas somem por padrão**; o toggle "mostrar concluídas" busca `all`.
- **Tags** (desde 0.1.23): campo no formulário (chips; digitar + Enter ou vírgula adiciona, prévia da forma
  normalizada enquanto digita, × remove), `#tag` em cinza depois do título na lista e no tooltip da barra, e um
  combo de **múltipla escolha** depois de "mostrar concluídas" (com **busca** no topo, que filtra a lista enquanto digita e zera ao fechar; lista com rolagem) com todas as tags em uso (`tags` da resposta de
  `GET /api/tasks`): nenhuma marcada = todas as tarefas; marcadas = tarefas que têm **qualquer** uma delas
  ("limpar filtro" zera). O filtro é só da sessão. A API aceita `GET /api/tasks?tags=a,b` com a mesma regra.
- **Ver: Tudo · Dia · Semana · Mês** (botões entre a ordenação e o toggle; desde 0.1.22, `Scope` em
  `pages/tasks.rs`, persistido em `ui-prefs.json` → `gantt.scope`): nos três recortes a lista mostra só as
  tarefas que tocam o período (início antes do fim do período e fim — ou início, sem fim — depois do começo)
  e a linha do tempo fica **travada**: o começo do período na borda esquerda e o fim na direita (dia =
  00:00–24:00 local; semana = segunda 00:00 a domingo 24:00; mês = dia 1 ao último), sem arrastar nem zoom.
  O zoom e a posição salvos não mudam: "Tudo" volta exatamente à visão livre anterior.
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
| Ignorados | cancelados, convites que **eu recusei**, eventos cujo `will_task_id` aponta para tarefa que ainda existe (em outra agenda/fora da janela), local de trabalho (`eventType = workingLocation`). |
| Eventos especiais (só leitura) | `eventType` ≠ `default` (`birthday` de contato, `fromGmail`, `outOfOffice`, `focusTime`): a API recusa escrita (até as propriedades privadas — "'birthday' event type must not have private extended properties"). Entram como tarefa com `event_read_only`; edição local **fica só aqui** (relatório `kept_local`, sem aviso; `synced_at` avança para não insistir) e apagar a tarefa **não apaga** o evento (sem lápide; a próxima rodada reimporta). Edição no Google ainda desce. Pílula "Google Agenda · só leitura" no formulário. |

Algoritmo (`TasksModule::sync_calendar`, uma rodada por vez): 1) lápides → `delete` no Google;
2) lista as ocorrências da janela; 3) tarefas locais = vinculadas a esta agenda ∪ soltas com início na
janela: solta → `insert` + vínculo; vinculada sem evento (nem por `get`) → apaga a tarefa; par
→ regra de conflito (`patch` ou `apply_remote_task`); 4) eventos sem par → `insert_task` já vinculada
(`created_by = calendar`, `created_at = updated_at = synced_at`). Relatório `SyncReport` (criadas /
atualizadas / removidas em cada sentido, sem alteração, ignorados, `kept_local`, avisos) mostrado na UI e no log.

Rotas: `POST /api/tasks/sync-calendar` `{from, to (YYYY-MM-DD), calendar_id}` (todos opcionais; janela
máxima 400 dias). App: `settings/tasks.rs`, aba **Google Calendar** (ao lado de Configuração) — estado da Agenda, seletor de agenda (habilitadas e
graváveis), dois calendários (`DatePickerButton`), botão **Sincronizar agora** e o relatório da última
rodada; o formulário da tarefa mostra a pílula "Google Agenda" quando vinculada.

## Limites

- Sem dependências entre tarefas, sem recorrência (pra isso há o scheduler, `docs/24`) e sem arrastar a
  barra pra mudar datas (decisão de 2026-11-10: clicar abre o formulário).
- Avisos no Telegram só no destino padrão; a notificação nativa é por evento, sem canal extra.
