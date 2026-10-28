# ADR-0017 — Scheduler como módulo de ferramentas; disparo entregue pelo stdin; recriação da thread dona

**Status:** Aceita · **Data:** 2026-10-28

## Contexto

O usuário quer que o agente programe tarefas futuras ("todo dia às 8 resuma as notícias e mande no Telegram", "me lembre amanhã às 10") sem depender de cron do sistema nem de scripts em `tools/`. O Claude Code headless não tem agendador próprio: alguém precisa guardar o agendamento, acordar na hora e entregar a tarefa a um processo `claude`.

Pontos de tensão com as regras existentes:

- **"Só canais criam threads"** (regra 10 do `CLAUDE.md`; `docs/04`): WhatsApp e Google são só ferramentas. Um agendador que criasse threads novas a cada disparo seria um canal disfarçado e perderia o contexto da conversa que o originou.
- **Thread dona pode sumir**: o usuário apaga threads pela UI, e a sessão do Claude expira (`cleanupPeriodDays`). O agendamento precisa continuar válido.
- **App fechado no horário**: o serviço é um app de sessão (ADR-0010), não um daemon de sistema; disparos vão ser perdidos.
- **Políticas** (ADR-0013): agendar não tem efeito externo, mas o que o agente fizer no disparo pode ter.

Alternativas avaliadas:

| Opção | Veredito |
|---|---|
| Scheduler como **canal** (origem `scheduler`, thread nova por disparo) | Rejeitado: perde o contexto da conversa, multiplica threads, resposta não tem para onde ir |
| `crontab`/Agendador de Tarefas do SO chamando `claude -p` | Rejeitado: fora do serviço (sem MCP, sem aprovações, sem histórico), específico por SO |
| Disparar como **turno novo** só quando a thread está ociosa (fila) | Rejeitado: viola a regra 2 (mensagem vai para o stdin na hora, sem esperar o turno acabar) |
| **Módulo de ferramentas + laço interno; disparo = mensagem na thread dona** | **Escolhido** |

## Decisão

1. **`will-scheduler` é um módulo de ferramentas + UI**, não um canal: expõe `schedule_create/list/get/update/delete` (família `scheduler`), a página Agendamentos e as rotas `/api/schedules*`. Depende só de `will-core` e `will-storage`; usa `ThreadHost` para falar com as threads.
2. **Disparo = mensagem no stdin da thread dona** via `ThreadHost::send_message`, inclusive mid-turn (steering). O texto leva um cabeçalho gerado pelo serviço (`will_core::schedule::fired_message`) que identifica o agendamento, para o agente saber que não é o usuário ao vivo; o `message` é conteúdo do agente/usuário e não é interpretado pelo serviço.
3. **Cada agendamento grava a `origin` e o `claude_session_id` da thread** na criação/edição. Se a thread dona foi apagada, no disparo o scheduler chama `ThreadHost::revive_thread(origin, nome, session_id)`: cria uma thread nova com a **mesma origem** (UI / mesmo tópico do Telegram / voz) e a sessão gravada; o motor tenta `--resume` e, se a sessão expirou, o caminho normal de sessão expirada recomeça do zero e reenvia a mensagem. Esta é a **única exceção** à regra "só canais criam threads", e é uma recriação de algo que já existiu, não uma thread nova por disparo.
4. **Disparos perdidos são ignorados**: na partida (e ao religar o módulo), cada cron recalcula a próxima ocorrência a partir de agora; agendamento único no passado é pausado com `paused_reason` visível. Nada é disparado retroativamente, para não despejar N tarefas de uma vez ao abrir o app.
5. **Políticas `auto`** nas cinco ferramentas `schedule_*`: agendar não tem efeito externo. As ferramentas usadas pelo agente no disparo seguem as suas políticas (regra 9/ADR-0013). O agente só agenda na própria thread (`ToolContext.thread_id`); só UI/API escolhem a thread dona.
6. **Laço único em processo**, dorme até o próximo `next_run_at` (máx. 60 s), acordado por `Notify` a cada mudança. Limites configuráveis (`max_schedules` 100, `min_interval_minutes` 5, nome 80, mensagem 8000) para conter agente em loop. Cron de 5 campos via `croner`, fusos IANA via `chrono-tz`, fuso do agendamento > `SchedulerSettings.timezone` > sistema.
7. Notificação nativa ao disparar em threads UI/voz (`notify_on_fire`); em threads do Telegram não, porque a resposta já vai para o tópico.

## Consequências

- (+) O disparo cai na conversa que o criou, com contexto e destino de resposta (tópico do Telegram, chat da UI) já resolvidos pelo canal de origem; funciona com threads dormentes (`--resume`).
- (+) Nada do scheduler fura as defesas: segue a regra 2 (stdin na hora), as políticas de aprovação e as allowlists dos envios.
- (+) Sem dependência de agendador do SO; portátil e multiplataforma como o resto.
- (−) Quem precisa de disparo com o app fechado não é atendido: é uma decisão consciente (app de sessão). Mitigação: `warn` nos logs, `paused_reason` na lista, autostart do app.
- (−) Thread recriada pode perder o contexto se a sessão também expirou; por isso a orientação de escrever `message` como instrução completa.
- (−) Uma exceção documentada à regra "só canais criam threads"; qualquer outro módulo que precise criar threads deve passar por nova ADR.
- Em threads do Telegram, a recriação reaproveita o `<chat_id>:<topic_id>`; o módulo Telegram volta a encontrar a thread pela origem (`ensure_thread`). Se o tópico foi apagado no Telegram, o envio da resposta falha como em qualquer thread órfã.
