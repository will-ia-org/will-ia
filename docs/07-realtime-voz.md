# 07 — Voz: OpenAI Realtime (will-realtime)

Conversa por voz em tempo real. O modelo Realtime é a **boca e o ouvido**; o Claude Code é o **cérebro**. O Realtime responde sozinho o que é trivial e delega o resto via function calling, sem interromper a conversa.

> **Implementação (ADR-0016, ADR-0019):** a captura e a reprodução de áudio acontecem **no app** (`will-ia-app`, via `cpal`) e viajam em PCM16 24 kHz pelo WebSocket `/api/voice/audio` (`will-client::VoiceLink`) até o **servidor**, onde o crate `will-realtime` cuida do WebSocket com a OpenAI, do handoff e do pipeline de áudio. Esse WS é o **único** caminho de áudio (loopback ou rede).
>
> **Uma sessão por usuário (etapa 6 de `docs/26`):** o servidor mantém um `RealtimeManager` por usuário conectado (`AppState.voice: HashMap<username, …>`, `modules::voice_manager_for`), criado sob demanda com `RealtimeDeps.owner = username`. `POST /api/voice/start|stop|toggle`, `GET /api/voice/status` e o WS de áudio agem sobre a sessão **do usuário do token** (`Principal`); o token local da API é o usuário `local`. `RealtimeSettings.max_sessions` (padrão 3, Voz › "Sessões simultâneas") limita as sessões **ativas** no servidor: quem tenta iniciar além do teto recebe 400 "limite de N sessão(ões) de voz simultânea(s) atingido" (a própria sessão já ativa nunca conta contra si). Cada usuário tem a sua thread de voz **"Voz · <user>"** ("Voz" para `local`), com `BrainHost` próprio (`orchestrator::UserBrain`) e autor `voz:<user>` nos turnos; `Event::Voice { state, detail, user }` identifica a sessão nos eventos, e o custo entra em `ai_costs` com `note = "user:<u>"`. Mudar chave/configurações da voz (`start_realtime`) reaplica a todas as sessões; a bolinha do módulo (`recompute_realtime_status`) é global (desligado / sem chave / pronto).

## Transporte

- **WebSocket** direto do serviço (`tokio-tungstenite` + `rustls`): `wss://api.openai.com/v1/realtime?model=<modelo>` com header `Authorization: Bearer <OPENAI_API_KEY>`.
- Áudio: PCM16 mono 24 kHz nos dois sentidos (`input_audio_format` / `output_audio_format` = `pcm16`). Captura e reprodução via `cpal`, com reamostragem linear quando o dispositivo não oferece 24 kHz.
- Modelo padrão: `gpt-realtime-mini` (configurável; `gpt-realtime` para mais qualidade, ~3x o preço). Transcrição do usuário desligada por padrão (campo vazio); informe um modelo (`gpt-4o-mini-transcribe`) para ter o texto do que foi dito. Nomes de eventos variam entre a API beta (`response.audio.delta`) e GA (`response.output_audio.delta`); o cliente aceita ambos.

## Ativação

| Modo | Comportamento |
|---|---|
| **Toggle** (padrão) | Atalho global abre a sessão; `turn_detection: server_vad` (ou `semantic_vad`) decide quando o usuário terminou de falar. Atalho de novo encerra. |
| **Push-to-talk** | Áudio só é enviado enquanto o atalho está pressionado; ao soltar, `input_audio_buffer.commit` + `response.create`. `turn_detection: null`. |

Atalho global via `global-hotkey` (Windows, macOS, X11; no Wayland não funciona: a UI mostra aviso e oferece o item "Voz" da bandeja e o botão do painel de voz como alternativa). Formato próprio, ex.: `Ctrl+Shift+Space` (`Cmd` no macOS).

Indicadores: ícone da bandeja muda para "gravando" e um **indicador flutuante** (viewport secundário do egui, sem decoração, sempre no topo, transparente) aparece enquanto a sessão está ativa, mostrando o estado (ouvindo / pensando / falando) e o nível do microfone. Na janela principal, o painel de voz tem iniciar/parar, botão segurar-para-falar e as transcrições.

## Configuração de sessão

```json
{
  "type": "session.update",
  "session": {
    "modalities": ["audio", "text"],
    "voice": "<configurável>",
    "instructions": "<persona de voz, ver abaixo>",
    "input_audio_format": "pcm16",
    "output_audio_format": "pcm16",
    "input_audio_transcription": { "model": "<modelo de transcrição>" },
    "turn_detection": { "type": "server_vad", "silence_duration_ms": 600 },
    "tools": [
      {
        "type": "function",
        "name": "ask_brain",
        "description": "Delega ao cérebro (Claude Code) tarefas que exigem pesquisa, memória de longo prazo, leitura/escrita de arquivos, execução de scripts ou raciocínio longo. Chame novamente para acrescentar contexto a uma tarefa em andamento.",
        "parameters": {
          "type": "object",
          "properties": {
            "prompt": { "type": "string", "description": "Pedido completo, em texto, para o cérebro." },
            "new_thread": { "type": "boolean", "description": "true para começar uma conversa nova com o cérebro (contexto limpo)." }
          },
          "required": ["prompt"]
        }
      },
      { "type": "function", "name": "brain_status", "description": "Verifica se o cérebro ainda está trabalhando na última tarefa.", "parameters": { "type": "object", "properties": {} } }
    ],
    "tool_choice": "auto"
  }
}
```

Instruções padrão da persona (editáveis):

> Você é a voz do assistente pessoal. Responda de forma breve e natural. Para qualquer coisa que exija lembrar informações do usuário, pesquisar, consultar ou alterar arquivos, ou pensar por mais de alguns segundos, chame `ask_brain` e diga ao usuário que está verificando. Enquanto o cérebro trabalha, continue conversando; se o usuário acrescentar detalhes relevantes, chame `ask_brain` de novo com o complemento. Quando o resultado chegar, resuma em fala corrida, sem listas nem símbolos.

## Ferramentas da voz

| Função | Uso |
|---|---|
| `ask_brain { prompt, new_thread?, thread?, title? }` | Delega ao cérebro. `thread` (id/prefixo ou título) continua um assunto existente; `new_thread` começa do zero, com `title` (2 a 5 palavras) proposto pela voz; sem ambos, segue na thread de voz atual. Thread criada sem título recebe como assunto o primeiro pedido (`derive_title`, ~48 caracteres). |
| `list_threads` | Lista as threads não arquivadas (id, título, origem, última atividade, trabalhando?) para a voz oferecer "continuar o assunto X". |
| `brain_status` | Há tarefa em andamento? |

As instruções da persona recebem um parágrafo fixo orientando a usar `list_threads` quando o usuário mencionar um assunto que pode já existir, e a nunca abrir threads com títulos genéricos ("Voz", "Conversa"): cada thread nova leva um `title` curto do assunto, para a lista de threads e a própria busca por título (`thread`) ficarem inequívocas. A resolução por título prefere o igual exato ao parcial; entre iguais, a mais recente. Na subida, threads antigas com título genérico são renomeadas pelo primeiro pedido.

## Custos

Cada `response.done` traz `usage` (tokens de texto/áudio, entrada/saída, cache). O app soma por sessão e, ao encerrar, estima o custo pela tabela de preços editável (Voz › Preços, USD por 1M tokens; padrões do `gpt-realtime-mini`), grava em `ai_costs` (fonte `voice`), emite `Event::CostRecorded` e mostra o resumo (toast e "Última sessão" na página Voz). Os turnos do Claude entram na mesma tabela com o custo informado pelo CLI (fonte `claude`). Página **Custos** no app e `GET /api/costs`, `GET /api/costs/summary?source=`.

## Fluxo de delegação (handoff)

```mermaid
sequenceDiagram
    participant OAI as Realtime
    participant RTM as will-realtime
    participant ORC as Orchestrator/Engine
    OAI-->>RTM: response.function_call_arguments.done {name: ask_brain, call_id, arguments}
    RTM->>ORC: SendMessage(thread_voz, prompt)  [new_thread → cria thread]
    RTM->>OAI: conversation.item.create {function_call_output, call_id, output: "{\"status\":\"working\",\"task_id\":…}"}
    RTM->>OAI: response.create  (modelo diz "estou verificando")
    Note over OAI,RTM: conversa continua; novos ask_brain → SendMessage mid-turn
    ORC-->>RTM: TurnFinished {text}
    RTM->>OAI: conversation.item.create {type: message, role: user, content: [{type: input_text, text: "[Resultado do cérebro para a tarefa …]: <texto>"}]}
    RTM->>OAI: response.create
    OAI-->>RTM: áudio com a resposta
```

Observações:

- A resposta ao `function_call` é **imediata** (`status: working`), porque a Realtime API espera um `function_call_output` para prosseguir. O resultado real chega depois como item de conversa.
- Se a sessão de voz já tiver sido encerrada quando o cérebro terminar, o resultado vai para a thread na UI e gera notificação nativa ("O cérebro terminou: …"), opcionalmente com TTS curto.
- Texto do cérebro passa por uma limpeza antes de ir ao Realtime: remove blocos de código, tabelas e caminhos longos, e limita tamanho (configurável, padrão 1500 caracteres) para a fala não ficar interminável. O texto completo fica na thread.
- A thread padrão de voz é **por usuário** (`origin = realtime`, título "Voz · <user>", ou "Voz" para o token local) e reutilizada: `Host::voice_thread_for(user, new_thread, title)` devolve a corrente da sessão, senão a última thread `realtime` com esse título, senão cria uma. `new_thread: true` cria outra e a torna a ativa da sessão de voz daquele usuário. Como as threads são compartilhadas, outros usuários veem "Voz · ligia" na lista, mas só a sessão da Ligia escreve nela pela voz.

## Interrupção (barge-in)

Com `server_vad`, ao receber `input_audio_buffer.speech_started` enquanto áudio está sendo reproduzido: parar a reprodução, descartar o buffer local e enviar `conversation.item.truncate` com os milissegundos efetivamente reproduzidos. Em PTT, pressionar o atalho durante a fala do assistente tem o mesmo efeito.

## Áudio (cpal, no app)

- Thread de captura: callback do `cpal` copia frames para um buffer sem alocação; uma task tokio drena, converte para PCM16 mono 24 kHz (reamostragem linear a partir da taxa do dispositivo), codifica base64 e envia `input_audio_buffer.append` em chunks de ~100 ms. O nível do microfone alimenta o indicador flutuante.
- Thread de reprodução: buffer alimentado pelas `*.audio.delta` decodificadas (`AudioOut`); callback do `cpal` consome, reamostrando para a taxa do dispositivo. Underrun → silêncio, sem pânico.
- Dispositivos de entrada/saída enumerados pelo `cpal` e selecionáveis nas configurações; padrão do sistema se não configurado. Troca a quente reinicia apenas os streams.
- O app envia/recebe esse PCM16 pelo WS `/api/voice/audio` do servidor (`will-client::VoiceLink`), autenticado pelo token do perfil, que o liga à sessão do usuário; clientes externos usam o mesmo WS com o mesmo formato. A latência extra é a da rede (ida e volta); aceitável em LAN/VPN.
- Sem gravação em disco por padrão.

## Ouvir resumo (TTS fora da sessão)

Além da conversa por voz, cada resposta do agente na página Threads tem o botão **ouvir** (ao lado de "copiar"), que lê um **resumo falado** da resposta sem abrir sessão Realtime. É o mesmo pipeline do áudio do Telegram, agora compartilhado em `will_realtime::openai::OpenAi`:

1. O app manda o Markdown da resposta em `POST /api/voice/summary { text }`. O servidor chama o modelo de chat configurado (`realtime.summary.model`, padrão `gpt-4o-mini`) com o **prompt do resumo** (`realtime.summary.prompt`, editável em Configurações → Voz → Ouvir resumo; padrão em `will_core::settings::default_speak_summary_prompt`), que transforma a resposta num roteiro fluido para ouvir: frases corridas, sem código, URLs, IDs, tabelas ou marcação. Devolve `{ summary, usd }`.
2. O app manda o roteiro em `POST /api/voice/tts { text, voice? }`. O servidor sintetiza com `realtime.summary.tts_model` (padrão `gpt-4o-mini-tts`), a voz `realtime.summary.voice` (vazio = `realtime.voice`) e `realtime.summary.speed`, pedindo `response_format = pcm`, e devolve **PCM16 LE mono 24 kHz** sem cabeçalho (`Content-Type: audio/L16; rate=24000`, header `X-Sample-Rate`).
3. O app guarda roteiro + PCM em `speak_cache` (memória e `<config>/will-ia-app/ouvir/<turn>.pcm|.json`, podado aos 200 mais recentes) e toca o PCM com `voice::audio::Player` (stream de saída `cpal` no dispositivo de saída configurado, reamostragem linear para a taxa do dispositivo) e libera o botão ao terminar. Clicar de novo em qualquer etapa cancela/para (o `Player` é solto e o stream fecha). Uma leitura por vez por página. **Ouvir de novo** a mesma resposta (botão "ouvir de novo", roteiro no hover) toca o áudio do cache sem chamar a OpenAI: zero tokens.

Robustez à troca de modelo: o cliente (`OpenAi::post_tolerant`) monta os parâmetros por família — modelos de raciocínio (`gpt-5*`, `o1/o3/o4*`) recebem `reasoning_effort: minimal` e **sem** `temperature`; os demais, `temperature: 0.4`; `instructions` do TTS só fora de `tts-1*` — e, se mesmo assim a OpenAI responder 400 apontando um parâmetro não suportado (`error.param` + `unsupported_parameter`/`unsupported_value`), remove esse parâmetro e repete (até 3), registrando em log. Trocar `gpt-4o-mini` por `gpt-5-nano` (ou `gpt-4o-mini-tts` por `tts-1`) funciona sem erro na tela; erros de outra natureza (chave, cota, modelo inexistente) chegam com a mensagem da OpenAI.

Condições: o botão só aparece com o módulo de voz em `Running` (chave da OpenAI válida e módulo habilitado — a mesma regra do botão **Falar** do ditado) e `realtime.summary.enabled`. Os dois endpoints recusam com 400 quando o módulo está desligado, sem chave ou com o resumo desabilitado. Custos estimados em `ai_costs`: `voice_summary` (tokens de entrada/saída do modelo de chat) e `voice_tts` (por caractere do roteiro), com `ref = user:<username>`.

## Reconexão e erros

- Queda do WS durante a sessão: tentar reconectar 3× com backoff (1 s, 2 s, 4 s), recriando `session.update`; falhando, encerrar sessão com aviso sonoro curto e notificação.
- 401: módulo `Degraded`, UI pede nova API key.
- Rate limit / `error` events: logar, falar "tive um problema, tenta de novo".

## Custos

A Realtime API cobra por minuto de áudio de entrada e saída. A UI mostra duração da sessão e estimativa; há opção de limite de minutos por dia. Com várias sessões, cada registro em `ai_costs` leva `note = "user:<username>"`, e a página Custos pode filtrar por usuário.

## Transcrições

Com `input_audio_transcription` ligado, `conversation.item.input_audio_transcription.completed` traz o texto do usuário; `response.output_audio_transcript.done` traz o do assistente. Ambos são gravados como turnos da thread de voz (configurável).

## Testes

- Cliente WS contra um servidor mock que reproduz roteiros de eventos (handoff, barge-in, reconexão).
- Pipeline de áudio testado com arquivos WAV → verificação de chunking, resample e base64.
- Teste manual guiado (checklist) por SO para dispositivos, atalho global e indicador flutuante (ver `docs/23`).
