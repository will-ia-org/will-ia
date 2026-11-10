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
| `done_at` | instante opcional | `tasks_complete` / botão ✓; `tasks_reopen` limpa |
| `notified_start_at`, `notified_overdue_at` | instantes | marcas do laço (um aviso por transição) |

**Estado derivado do relógio** (`Task::state_at(now)`), nunca gravado:

```
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
| `tasks_list` | `filter`: `pending` (padrão) · `running` · `overdue` · `upcoming` · `done` · `all` | lista `TaskView`s + `timezone` |
| `tasks_get` | `id` (completo ou prefixo único ≥ 4) | detalha |
| `tasks_update` | `id` + campos (só os enviados mudam; `end_at: ""` limpa) | altera |
| `tasks_delete` | `id` | remove |
| `tasks_complete` / `tasks_reopen` | `id` | conclui / reabre |

Com `tasks.enabled = false` o provider some do MCP e as chamadas devolvem `Unavailable`.

## API local

| Método | Rota | Descrição |
|---|---|---|
| GET | `/api/tasks?filter=` | `{tasks, enabled, timezone, offset_minutes, now}` |
| POST | `/api/tasks` | corpo `TaskInput` |
| GET / PUT / DELETE | `/api/tasks/{id}` | detalhar / alterar / remover |
| POST | `/api/tasks/{id}/complete` · `/reopen` | concluir / reabrir |

Configurações: seção `tasks` (`enabled`, `timezone`, `notify_native`, `notify_start`, `notify_overdue`, `sort`) por
`PUT /api/settings`; `apply_section("tasks")` reaplica no módulo a quente.

## Painel (app, `pages/tasks.rs`)

- Lista à esquerda (bandeira de prioridade 🔴🟡🟢 pintada, título, bolinha do estado) e **linha do
  tempo** à direita, mesma altura de linha; cabeçalho fixo com dias (fim de semana sombreado, hoje em
  destaque) e ticks de hora conforme o zoom; **linha do agora** em azul com o horário.
- **Zoom** por Ctrl + rolagem / pinça / botões − + (meio dia a 90 dias visíveis); **arrastar** o fundo
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

## Limites

- Sem dependências entre tarefas, sem recorrência (pra isso há o scheduler, `docs/24`) e sem arrastar a
  barra pra mudar datas (decisão de 2026-11-10: clicar abre o formulário).
- Avisos no Telegram só no destino padrão; a notificação nativa é por evento, sem canal extra.
