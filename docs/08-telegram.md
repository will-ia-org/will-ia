# 08 — Telegram (will-telegram)

Modelo baseado no projeto de referência `telegram-bot` (Node.js, em produção), portado para Rust. As decisões abaixo reproduzem o que funciona bem lá; onde divergimos, está marcado.

## Conceitos

| Conceito | Definição |
|---|---|
| **Supergrupo com Tópicos** | Grupo do Telegram convertido em supergrupo com "Tópicos" ligado (fórum). O bot deve ser **admin** com permissão de **gerenciar tópicos**. Bot admin ignora o privacy mode, então não precisa mexer no `/setprivacy`. |
| **Thread key** | `<chat_id>` para chat privado e para o "General" do fórum; `<chat_id>:<message_thread_id>` dentro de um tópico. **Uma thread key = uma thread do Claude Code** (processo + sessão + pilha de status). |
| **Pilha de status** | Uma única mensagem em itálico por thread, editada ao vivo, que espelha o que entrou (🧑) e o que o agente narra (🤖). Ao terminar o turno vira resposta final e some. |

> ⚠️ Tópicos em **chat privado** (Threaded Mode do BotFather) estão quebrados na Bot API desde a 10.0 (`sendMessage` com `message_thread_id` → `400 message thread not found`). O paralelismo usa **supergrupo com fórum**.

## Permissões (deny by default)

- `chat_allowlist`: IDs de chat liberados (o supergrupo, negativo, ex. `-100…`, e opcionalmente o privado do dono).
- `user_allowlist`: IDs de usuário que podem falar com o bot dentro desses chats. *(Divergência: a referência usa só chat IDs; aqui exigimos também o user ID porque um supergrupo pode ter mais membros.)*
- Fora da allowlist: só `/start` responde, mostrando o chat ID e o user ID para o usuário copiar na configuração. Tudo mais é ignorado e logado.
- Callback queries (botões) passam pela mesma allowlist.
- Com as duas listas vazias o módulo **não inicia** e a UI explica como preencher.

## Biblioteca

`teloxide` (com `rustls`), usando apenas a camada de requisições (`Bot` + métodos), **sem** o framework de dispatcher/dialogues: o roteamento é nosso, igual à referência. Alternativa mais fina considerada: `frankenstein`. Ver [ADR-0008](adr/0008-telegram-teloxide.md).

Long polling com `getUpdates(offset, timeout=30, allowed_updates=["message","callback_query"])` e timeout HTTP de `timeout + 10 s` para abortar conexões penduradas após suspensão da máquina. Em erro: log + espera 3 s.

## Fluxo de uma mensagem

1. **Offset** persistido a cada update (sobrevive a reinício).
2. Ignorar eventos de serviço do fórum (`forum_topic_created/edited/closed/reopened`).
3. Calcular thread key; extrair texto (`text` ou `caption`) e anexo.
4. Allowlist (chat **e** usuário). Reprovado → só `/start`.
5. Anexo → baixar para `<kb>/inbox/telegram/<chat>-<msg>-<nome-sanitizado>`; voice note → transcrever (OpenAI) e tratar como texto, ecoando "🎙️ _transcrição_" no tópico; demais arquivos → prompt com caminho e pedido da legenda.
6. Comando builtin → tratar. Comando do repo (`.claude/commands/*.md` da KB) → traduzir `_`→`-` e repassar ao Claude.
7. **Dispatch**: empurra 🧑 na pilha, `sendChatAction(typing)`, e escreve **na hora** no stdin do processo da thread (`Engine::send`). Mensagens durante um turno em andamento seguem o mesmo caminho e aparecem na pilha; não há espera.

## Pilha de status (Conversation)

| Parâmetro | Valor (referência) |
|---|---|
| Throttle entre edições | 1200 ms (rate limit do Telegram) |
| Tamanho máx. da prévia | 3500 chars (limite é 4096) |
| Tamanho máx. por linha | 280 chars |
| Atraso para apagar o status após a final | 10 s |

Comportamento: cria a mensagem de status na primeira entrada; edita com throttle; se a mensagem sumiu (`message to edit not found`, `message can't be edited`), recria; `not modified` é ignorado; quando estoura o limite, descarta as linhas mais antigas. Estado da pilha (entradas, `status_message_id`, `last_user_message_id`) é persistido para limpeza após reinício.

## Fim do turno (finalize)

1. Resposta final como **mensagem nova**, com `reply_parameters` apontando para a **última mensagem do usuário presente na pilha** (fallback: último id conhecido), `allow_sending_without_reply: true`.
2. Dividir em pedaços ≤ 4096 preferindo quebra em `\n` (se o melhor corte ficar antes da metade, corta no limite).
3. Agendar exclusão da mensagem de status (10 s).
4. Limpar a pilha.
5. Opcional: **resumo em áudio** (voice note) se habilitado e resposta ≥ 120 chars: resumo "falável" por um modelo de texto + TTS em Ogg/Opus → `sendVoice` com `record_voice` como chat action. Best-effort, nunca bloqueia.
6. Erro (`is_error`) → prefixo ⚠️ e sem áudio.

Formatação: a referência envia a final como **texto puro** e instrui o agente (via `--append-system-prompt`) a evitar Markdown. Mantemos isso como padrão, com opção "converter Markdown → HTML do Telegram" para quem preferir formatação (conversor próprio, escapando `& < >`).

## Comandos builtin

| Comando | Efeito |
|---|---|
| `/start` | Mostra chat ID, user ID, thread key e se está liberado. Único comando que responde fora da allowlist. |
| `/novo` | Zera a conversa **desta thread** (nova sessão). Limpa a pilha. |
| `/reiniciar` | Reinicia o processo desta thread mantendo o contexto (`--resume`). |
| `/topico <nome>` | Cria tópico no fórum (`createForumTopic`) e saúda nele. |
| `/apagar` | Só dentro de tópico: apaga a thread no serviço (sessão + pilha) **e** o tópico no Telegram (`deleteForumTopic`). |
| `/limpar [key\|tudo]` | Lista threads dormentes / remove do serviço (não toca no Telegram). Necessário porque o Telegram **não notifica** exclusão de tópico e a API não permite perguntar se um tópico existe. |
| `/status` | Uptime e lista de threads: 🟢 pronta · 🟡 trabalhando · ⚪ dormindo (volta com `--resume`) · ❌ sessão expirada (volta do zero). |
| `/chave` | Teclado inline para escolher o token ativo do Claude (✅ na ativa, 🔴 na reprovada). Testa antes de aplicar; aplica sem reiniciar o serviço (derruba os processos, voltam com `--resume`). |
| `/help` | Lista os comandos. |

Normalização: remove `/`, `@bot`, acentos e caixa, de modo que `/tópico` e `/TOPICO` caem no mesmo builtin (o Telegram só reconhece `[a-z0-9_]` como comando). Comandos do repo com hífen aparecem no menu com `_`.

`setMyCommands` é chamado no boot com builtins + comandos da KB (`.claude/commands/*.md`, descrição lida do frontmatter `description:`).

## Boot e reinício

- No boot: avisa cada chat da allowlist ("🟢 Bot no ar") — configurável.
- Limpa mensagens de status órfãs de todas as threads (o turno delas morreu com o reinício).
- Processos `claude` sobem **sob demanda**, um por thread, na primeira mensagem.

## Eventos do motor → Telegram

| Evento | Reação |
|---|---|
| `Narration` (bloco `text` do assistant) | 🤖 na pilha (apenas se há pendências na thread) |
| `TurnFinished` | finalize |
| `Dropped` (processo caiu) | finalize com "⚠️ A instância reiniciou no meio do processamento. Manda de novo." |
| `StaleSession` | 🤖 "♻️ a sessão antiga deste tópico expirou — comecei uma nova e reenviei sua mensagem" |

## Persistência (ver [11](11-persistencia.md))

`telegram_bindings(thread_key → thread_id)`, `telegram_state(offset)`, `telegram_status(thread_key → stack_json, status_message_id, last_user_message_id)`.

## Setup para o usuário (texto da UI)

1. Crie um bot no @BotFather e cole o token.
2. Crie um grupo, adicione o bot, ligue **Tópicos** (vira supergrupo), promova o bot a **admin** com "gerenciar tópicos".
3. Mande `/start` no grupo; copie o chat ID (negativo) e seu user ID para as allowlists.
4. Crie tópicos com `/topico Nome`. Cada tópico é uma conversa paralela com o cérebro.

## Testes

- Mock da Bot API (servidor HTTP local) com roteiros de updates: texto, tópico novo, anexo, voice, callback, comandos, não-autorizado.
- Testes unitários: thread key, split de mensagem, normalização de comando, render da pilha, throttle.
