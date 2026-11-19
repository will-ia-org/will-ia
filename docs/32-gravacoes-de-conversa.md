# 32 — Gravações de conversa

Grava uma conversa que acontece no computador (ligação do WhatsApp, reunião no Meet, qualquer programa) para transcrever e consultar depois.

## O que é gravado

Um único WAV estéreo PCM16 16 kHz:

| Canal | Fonte | Quem é |
|---|---|---|
| Esquerdo | microfone | quem gravou ("Você") |
| Direito | saída do sistema (tudo que toca nas caixas/fone) | o outro lado (o nome informado ao parar) |

Um arquivo com dois canais mantém as faixas sincronizadas amostra a amostra.

## Captura (app)

`will-app/src/voice/call_recorder.rs` (`CallRecorder`), com a `cpal`:

| Sistema | Saída do sistema |
|---|---|
| macOS 14.6+ | "process tap" do Core Audio sobre a saída padrão; pede a permissão de gravação de áudio do sistema (`NSAudioCaptureUsageDescription` no `Info.plist`) |
| Windows | loopback do WASAPI sobre a saída padrão |
| Linux | fonte "monitor" do PulseAudio/PipeWire |

- Os dois dispositivos têm relógios próprios e a saída do sistema nem sempre entrega áudio em silêncio. Cada bloco é posicionado pelo **relógio da gravação** (`Track::push`): buraco maior que 250 ms vira silêncio; faixa adiantada mais de 250 ms tem o bloco descartado.
- Uma thread escreve no disco a cada 200 ms, com 500 ms de margem; a gravação não fica na memória.
- Sem a saída do sistema (permissão negada, dispositivo sem suporte), a gravação segue só com o microfone e avisa.
- **A captura do som do sistema roda numa linha própria** (`system_loop`, confere a saída a cada 200 ms), para que abrir o dispositivo nunca trave a tela nem o microfone. Ela só abre depois de ver a mesma saída em duas conferências seguidas e reabre quando o dispositivo padrão ou o formato dele muda. No macOS, se a captura aberta ficar sem entregar nenhum bloco (1,2 s, dobrando a cada tentativa até 10 s), ela é reaberta: acontecia quando abria no meio da troca de modo do fone Bluetooth e o som só chegava vários segundos depois. Cobre trocar de caixa/fone no meio da gravação e o fone Bluetooth que cai para o modo de chamada (mono 16 kHz) quando o microfone dele abre.
- Se nenhum som chega do computador nos primeiros segundos, a caixinha mostra "sem som do computador" (permissão negada ou nada tocando).
- A permissão do macOS vale para o processo responsável pelo app: como o servidor pode abrir o app, `NSAudioCaptureUsageDescription` está nos dois `Info.plist` (app e servidor).
- Ao encerrar, o log do app traz os blocos recebidos e o pico de cada faixa (`mic_blocks`, `sys_blocks`, `mic_peak`, `sys_peak`), para diagnosticar faixa muda.
- Limites conhecidos: no macOS, fone USB com microfone num dispositivo só não permite capturar a saída (a `cpal` abriria o microfone do fone); sem fone, o microfone também capta a caixa de som.

## Fluxo

1. Atalho global `realtime.recording_hotkey` (padrão `Ctrl+Alt+R`) ou botão "Gravar conversa" em **Gravações**. O texto do atalho é o mesmo nos três sistemas; a tela mostra os nomes das teclas do teclado local (`hotkey::display`: no Mac, `Control (⌃) + Option (⌥) + R`).
2. Caixinha flutuante sempre no topo (`overlay::show_recording`): tempo, nível das duas faixas e Parar. As caixinhas (voz, ditado, gravação) abrem no canto inferior direito da tela onde a janela do will-ia está e se empilham (`overlay::corner_pos`; no macOS as telas vêm do AppKit, `screens.rs`; nos outros sistemas vale o monitor principal).
3. Ao parar, o app pergunta **"Com quem você falou?"** (pessoa, turma ou grupo) e o assunto.
4. O WAV sobe para o servidor (`POST /api/recordings`), que cria a pasta `<dados>/recordings/AAAA-MM-DD_HH-MM-SS_<id curto>/`, a ficha (tabela `recordings`, migrations 0018 e 0019) e começa a processar.
5. Se o envio falhar, o áudio fica em `<config do app>/gravacoes/` e aparece como "não enviada".

## Transcrição (servidor)

`will-server/src/recordings.rs`:

- separa os canais e transcreve cada um em pedaços de 10 min (`whisper-1`, `verbose_json`, com tempo por trecho);
- pedaço em silêncio não é enviado; trecho com `no_speech_prob` alto ou sem energia no áudio é descartado (em silêncio o modelo inventa frases);
- intercala os trechos por tempo e junta falas seguidas da mesma pessoa → `transcript.json` (`[{start, end, speaker: me|other, text}]`);
- custo registrado em `ai_costs` com a fonte `recording_stt`.

Com várias pessoas do outro lado, todas saem com o mesmo nome (o do grupo).

## Pasta, resumo e busca por assunto

Cada gravação fica numa pasta com a data e a hora do início:

```text
recordings/2026-11-18_14-32-05_a1b2c3d4/
  audio.wav         dois canais
  transcript.json   falas com tempo e quem falou
  transcricao.md    o mesmo, legível
  resumo.md         resumo detalhado
```

Depois da transcrição, o servidor segue sozinho:

1. **Resumo detalhado** (`summarize`): chat da OpenAI (modelo de `engine.thread_summary.model`) com um prompt que pede assuntos, fatos, números, o que cada lado disse, o que ficou combinado e pendências. Fica em `recordings.summary` e em `resumo.md`.
2. **Índice** (`index`): embeddings `text-embedding-3-small` do resumo e de cada trecho de cerca de 1.200 caracteres (sem partir uma fala). Vão para `recording_chunks` (vetor `f32` em BLOB). O texto vetorizado leva o cabeçalho (data, com quem, assunto).
3. **Busca** (`search`): vetoriza a pergunta e compara por cosseno, em memória, com todos os trechos.

Se o resumo ou o índice falharem, a transcrição continua salva e o erro diz qual etapa falhou; "Transcrever de novo" refaz tudo. Custos em `ai_costs`: `recording_stt`, `recording_summary`, `recording_embedding`.

### Ferramentas MCP (família `recordings`, política automática, só leitura)

| Ferramenta | O quê |
|---|---|
| `recordings_search {query, limit?}` | trechos e resumos mais próximos do texto, com gravação, data, com quem e o minuto da fala |
| `recordings_list {limit?}` | gravações da mais recente para a mais antiga, com resumo |
| `recordings_get {id}` | ficha, resumo e transcrição completa |

## Tela

Lista à esquerda e, à direita: player embutido (tocar/pausar e barra para arrastar; `voice/clip_player.rs`), resumo e a transcrição em formato de conversa. Clicar numa fala toca a partir dela.

## API

| Método | Rota | O quê |
|---|---|---|
| GET | `/api/recordings` | lista (mais recentes primeiro) |
| POST | `/api/recordings?with=&subject=&started_at=&has_system=` | corpo = WAV; salva e transcreve |
| GET | `/api/recordings/{id}` | `{recording, other_label, indexed_chunks, segments, text}` |
| GET | `/api/recordings/{id}/audio` | o WAV (para o player do app) |
| PATCH | `/api/recordings/{id}` | `{with_whom, subject}` |
| DELETE | `/api/recordings/{id}` | apaga ficha, áudio e transcrição |
| POST | `/api/recordings/{id}/transcribe` | transcreve (de novo) |
| POST | `/api/recordings/{id}/ask` | abre uma thread com a transcrição e pede um resumo → `{thread_id}` |
