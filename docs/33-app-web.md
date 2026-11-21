# 33 — App web (o app no navegador)

**Status:** primeira versão em 2026-11-21 (0.1.38), em validação. O mesmo app de desktop (`will-ia-app`, egui) compilado para WebAssembly e **servido pelo próprio servidor** em `/`, na mesma porta da API. Serve para usar o will-ia de um iPad, celular ou outra máquina sem instalar nada.

## Como funciona

```
navegador (Mac, iPad, outra máquina)                 servidor will-ia (porta da API)
┌──────────────────────────────────┐                ┌──────────────────────────────────────┐
│ página: <canvas> + WASM          │  GET /         │ web_ui.rs: index.html, JS e WASM      │
│  (o mesmo código das páginas     │ ─────────────► │ embutidos no binário (gzip + ETag)    │
│   do app de desktop)             │  /api, WS      │ api/: as mesmas rotas do app desktop  │
└──────────────────────────────────┘ ◄────────────► └──────────────────────────────────────┘
```

- **Uma base de código.** As páginas (`crates/will-app/src/pages`) são as mesmas. O que muda fica nas bordas, com `#[cfg(target_arch = "wasm32")]`:
  - `crates/will-app/src/platform.rs`: arquivos, diálogos, notificações, threads (versão desktop × navegador).
  - `crates/will-app/src/web/`: substitutos de `hotkey.rs` e `oauth_cb.rs`.
  - `crates/will-app/src/service.rs`: `Service::call` usa `spawn_local` em vez do runtime tokio.
  - `crates/will-client/src/web.rs`: WS de eventos sobre o `WebSocket` do navegador; `voice_web.rs`: `VoiceLink` do navegador.
  - `crates/will-app/src/profiles.rs`: perfis no `localStorage` (a senha **não** é guardada, só o token de sessão).
- **Mesma origem.** A página só fala com o servidor que a serviu; a URL do perfil é sempre o endereço da página (o campo fica fixo na tela de entrada).
- **`Instant`** vem do crate `web-time` (no desktop é o `std`). No navegador o relógio começa em zero: use `checked_sub`, nunca `Instant::now() - Duration`.

## "Esta máquina" × acesso de fora

| De onde abre | O que acontece |
|---|---|
| `http://localhost:<porta>` na máquina do servidor | Entra direto em **Esta máquina**, como o app de desktop: o servidor injeta o token local na página (`window.__WILL_IA_LOCAL_TOKEN__`). |
| Qualquer outro endereço (IP da rede, `tailscale serve`, proxy) | Tela de entrada: **usuário e senha** de uma conta do servidor (`docs/26`). |

A regra está em `web_ui::is_local_visit`: conexão de loopback **e** `Host` igual a `localhost`/`127.0.0.1` **e** nenhum cabeçalho de encaminhamento (`X-Forwarded-*`, `Forwarded`, `Tailscale-*`). O `Host` barra DNS rebinding; os cabeçalhos barram proxies locais que repassam visitas de fora. O `index.html` vai com `Cache-Control: no-store`.

## Acesso de outras máquinas

A página segue a configuração de **Rede** do servidor (`docs/26`, Configurações › Rede), porque está na mesma porta:

| Cenário | Como |
|---|---|
| Rede local, sem criptografia | `bind = 0.0.0.0` **e** "permitir acesso remoto sem TLS". Abrir `http://<ip>:<porta>`. O microfone não funciona (o navegador exige HTTPS fora de `localhost`). |
| Rede local com TLS do servidor | `bind = 0.0.0.0` + TLS. Abrir `https://<ip>:<porta>` e aceitar o certificado autoassinado. |
| De fora de casa (recomendado) | Servidor em `127.0.0.1` + `tailscale serve --bg <porta>`: endereço `https://<máquina>.<rede>.ts.net` com certificado válido, visível só para os aparelhos da conta Tailscale. |

Com TLS ligado, "Esta máquina" continua funcionando: o servidor grava `<data_dir>/api-scheme` e o app de desktop conecta em `https://127.0.0.1` (sem fixar certificado: é loopback); no navegador do próprio servidor o endereço passa a ser `https://localhost:<porta>`.

No iPad/iPhone: abrir o endereço no Safari → Compartilhar → **Adicionar à Tela de Início** (ícone próprio, tela cheia; `manifest.webmanifest` + metas `apple-mobile-web-app-*`).

## O que não existe no navegador

| Recurso | Situação |
|---|---|
| Atalhos globais, iniciar com o sistema, bandeja | Não se aplicam a uma página. |
| Caixinhas flutuantes (voz, "Falar", aprovações) | Painéis ancorados no canto direito da página (`ui::show_floating`: no desktop é uma janela do SO; no navegador, uma `egui::Area`). A pilha de aprovações aparece inteira, sem rolagem, enquanto couber na altura da página. |
| Gravação de conversa | Só no desktop (precisa do áudio do sistema e de arquivo local). |
| Backup/restauração, escolher pasta da KB local, "abrir pasta" | Só no desktop (acesso a disco). |
| Conectar conta Google | Só no desktop (o retorno do OAuth usa uma porta local). |
| Abrir anexo recebido | Pendente (baixar pelo navegador). |
| "Ouvir" resposta, teste de microfone, escolha de dispositivo | Pendente; a sessão de voz e o ditado ("Falar") funcionam (ver "Voz"). |

O player das gravações toca pela Web Audio da página (`audio_web::ClipPlayer`). **Pendente:** no iPad o tempo anda mas o som não sai (suspeita: sessão de áudio "ambiente" do Safari, emudecida pelo modo silencioso, e/ou a taxa forçada de 24 kHz do `AudioContext`); há um rascunho de correção com `navigator.audioSession` ainda não validado.

Preferências locais (`ui_prefs`, hoje o Gantt de Tarefas) ficam no `localStorage` da página.

### iPad/iPhone

- **Teclado cobrindo o campo de mensagem:** o `<canvas>` acompanha `window.visualViewport` (script no `index.html`), então o app se redesenha na área visível acima do teclado virtual ou da barra de atalhos do teclado físico.
- **Autocompletar:** o `<input>` oculto que o egui usa para o teclado virtual recebe `autocomplete="off"`, `autocorrect="off"` e `spellcheck="false"` (`MutationObserver` no `index.html`); sem isso o Safari mostra a barra de preenchimento automático a cada toque. Efeito colateral: sem autocorreção.

Anexar arquivos funciona: o diálogo é o do navegador (`rfd::AsyncFileDialog`) e arrastar-e-soltar também; os bytes ficam em memória por um caminho inventado (`platform::web_register`) até o envio.

## Voz

A sessão de voz funciona no navegador com a mesma arquitetura do desktop (`docs/07`, `docs/26`): microfone e alto-falante na página, sessão OpenAI no servidor, áudio PCM16 mono 24 kHz pelo WS `/api/voice/audio`.

| Peça | Onde |
|---|---|
| Microfone e alto-falante (Web Audio) | `window.willIaAudio`, no `index.html` (`getUserMedia` + `ScriptProcessor`; fala agendada em `AudioBufferSource`) |
| Ponte para o app | `will-app/src/voice/audio_web.rs` (`AudioPipeline` com a mesma interface do desktop) |
| WS de áudio | `will-client/src/voice_web.rs` (`VoiceLink` sobre o `WebSocket` do navegador, quadros binários) |

- **HTTPS obrigatório fora de `localhost`:** o navegador só libera o microfone em contexto seguro. Por `http://<ip>:<porta>` a voz responde "o navegador só libera o microfone em HTTPS ou em localhost". Use o TLS do servidor ou o `tailscale serve`.
- **Permissão:** o pedido de microfone é assíncrono; o erro (negado, sem HTTPS) chega pelo `AudioPipeline::take_error`, lido no `VoiceController::tick`.
- **Eco:** quem cancela é o navegador (`echoCancellation`). Sem "interromper falando", o microfone fecha enquanto o assistente fala, como no desktop.
- **iOS/Safari:** o áudio só destrava depois de um toque; a página cria/retoma o `AudioContext` no primeiro gesto.
- **Ditado ("Falar"):** `audio_web::Recorder` grava em memória (PCM16 16 kHz) e envia o WAV para transcrever, como no desktop.
- **Ainda não:** "ouvir" resposta, teste de microfone e escolha de dispositivo (o navegador usa o padrão do sistema), sinais sonoros de início/fim.

## Compilar

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version <a do Cargo.lock> --locked
tools/app-web/build.sh                      # gera dist/app-web (index.html, JS, WASM, ícones, .gz)
WILL_APP_WEB_DIR="$PWD/dist/app-web" cargo build --release -p will-ia -p will-app
```

- O `crates/will-server/build.rs` embute a pasta de `WILL_APP_WEB_DIR` (cfg `app_web`). Sem a variável o servidor compila **sem** a página e `/` responde 404: `cargo build`/`cargo test` do dia a dia não dependem do WASM.
- O ETag dos arquivos é um hash do conteúdo (`WILL_APP_WEB_TAG`): o navegador só baixa o WASM (~7 MB comprimido) de novo quando o servidor é atualizado.
- Release (`release.yml`): o job `web` gera `dist/app-web` uma vez e os builds de cada plataforma embutem o mesmo pacote.
- Desenvolvimento sem reinstalar o servidor: `node tools/app-web/serve.mjs` entrega `dist/app-web` em `http://localhost:8891` e repassa `/api` ao servidor local.
- CI: `cargo check --target wasm32-unknown-unknown -p will-app` garante que o alvo web continua compilando.
