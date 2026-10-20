# 23 — Guia de teste

## Pré-requisitos
- Rust estável (≥ 1.94; o projeto foi construído com 1.99). Se o sistema tiver um Rust antigo, instale via `rustup` e use `export PATH=$HOME/.cargo/bin:$PATH`.
- Linux: `libasound2-dev` e `pkg-config` (ALSA, exigido pelo `cpal`): `sudo apt install libasound2-dev pkg-config`. Sem `sudo`, um sysroot local com os headers da ALSA serve: `export PKG_CONFIG_PATH=$HOME/.local/will-ia-sysroot/usr/lib/x86_64-linux-gnu/pkgconfig` (truque desta máquina de dev; o normal é instalar o pacote). Windows e macOS não precisam de nada além do Rust.
- Claude Code CLI instalado e um token de subscription: `claude setup-token`.
- Opcional: bot do Telegram, API key da OpenAI, OAuth Client do Google, telefone com WhatsApp.

## Compilar
```bash
cargo build --release -p will-desktop             # binário: target/release/will-ia (app egui + serviço)
```
Não há etapa de frontend: um só toolchain (Rust).

## Rodar
```bash
target/release/will-ia                            # serviço + bandeja; janela oculta após o onboarding (fechar = esconder; "Sair" na bandeja)
target/release/will-ia --show                     # força a janela principal
target/release/will-ia                            # com o serviço já no ar: mostra a janela da instância ativa (POST /api/ui/show) e sai
target/release/will-ia serve                      # headless: só serviço + API local em http://127.0.0.1:47300 (sem janela/bandeja/áudio)
target/release/will-ia info                       # mostra data_dir, logs e o token da API local
```
Opções de `serve`: `--port N`, `--data-dir PASTA`, `--file-secrets` (sem keyring; útil em servidores/SSH). No headless, a API (`docs/22`) é o único acesso; o token fica em `<data_dir>/ui-session.token`.
Logs: `~/.local/state/will-ia/logs/` (Linux), `~/Library/Logs/will-ia/` (macOS), `%LOCALAPPDATA%\will\will-ia\logs\` (Windows).

## Primeiro uso (onboarding)
1. **Configurações → Base de Conhecimento**: informe uma pasta dedicada (não a home) e clique **Criar estrutura**.
2. **Motor Claude**: cole o token (`claude setup-token`) e salve. O primeiro token é testado e ativado automaticamente.
3. **Threads → Nova thread**: envie "olá". Durante um turno longo, envie outra mensagem: ela entra como contexto adicional (steering) e aparece como `mid_turn`.

## Testar sem gastar tokens (motor simulado)
```bash
cargo build -p fake-claude
# em Motor Claude → "Caminho do binário claude": target/debug/fake-claude ; token: qualquer texto ≥ 20 chars
```
Prompts especiais do `fake-claude`: `sleep N`, `crash`, `tool`, `fail`. O script `scripts/smoke.sh` automatiza um fluxo completo contra a API.

## Telegram
1. Crie o bot no @BotFather; cole o token em **Telegram** e clique **Testar**.
2. Crie um grupo, ligue **Tópicos**, torne o bot **admin** (gerenciar tópicos), mande `/start` no grupo e copie o **chat ID** (negativo) e o seu **user ID** para as allowlists. Habilite e salve.
3. Crie um tópico com `/topico Nome` e converse. Comandos: `/novo`, `/reiniciar`, `/status`, `/apagar`, `/limpar`, `/chave`, `/help`.
4. Peça ao cérebro "me mande uma mensagem no Telegram dizendo X": a ferramenta `telegram_send_message` pede aprovação (UI, notificação ou botões no próprio Telegram).

## Voz (OpenAI Realtime)

Diagnóstico de áudio: `will-ia audio-devices` lista os dispositivos vistos pelo app (no Linux, via PulseAudio/PipeWire: fones Bluetooth e USB aparecem com nome legível). Em Configurações → Voz escolha microfone e saída, use **Testar microfone** (barra de nível) e **Tocar tom na saída**, salve, e só então **Iniciar**. Sem fone, deixe **Interromper por voz (barge-in)** desligado: o microfone fica mudo enquanto o assistente fala, senão o alto-falante corta a própria resposta. Captura da janela para relatar problemas de layout: `will-ia screenshot --nav voice` grava `<data_dir>/screenshot.png`.

1. **Voz**: cole a API key da OpenAI, habilite, salve.
2. Escolha os dispositivos de entrada/saída (enumerados pelo `cpal`) se não quiser o padrão do sistema. No painel de voz, **Iniciar**; o indicador flutuante aparece (ouvindo / pensando / falando + nível do microfone). Fale; para tarefas que exigem a base, o modelo chama `ask_brain` e continua conversando até o resultado chegar.
3. Modo push-to-talk: segure o botão **segurar para falar** ou o atalho global. O atalho global (`Ctrl+Shift+Space`; `Cmd` no macOS) alterna a sessão no modo toggle; em Wayland ele não funciona — use o item "Voz" da bandeja.
4. Headless (`will-ia serve`): não há áudio local; um cliente externo pode usar o WS `/api/voice/audio` (`docs/22`).

## MCP e aprovações
- **MCP → Ferramentas**: políticas por ferramenta (auto/confirmar/desligada). Ações externas nascem em **confirmar**.
- **MCPs externos**: adicione um servidor (ex.: `npx -y @modelcontextprotocol/server-everything`), **Testar**, e veja o `.mcp.json` gerado em **Base de Conhecimento → ver .mcp.json**. Segredos aparecem como `${WILL_MCP_<NOME>_<CHAVE>}`.

## Google (Agenda e Gmail)
1. Crie o OAuth Client (tipo *App para computador*) seguindo o passo a passo em **Conta Google** e **publique o app** (evita expiração do refresh token em 7 dias).
2. **Conectar conta Google** com os escopos desejados; conclua no navegador.
3. **Agenda**: Atualizar lista → marque as agendas visíveis e a padrão. **Gmail**: habilite; envio fica em *confirmar*.
4. Peça ao cérebro: "o que tenho amanhã?" / "resuma os e-mails não lidos de hoje" / "marque reunião X às 15h" (pede aprovação).

## WhatsApp
1. **WhatsApp**: habilite, salve e adicione uma conta: **Parear por QR** (o QR aparece na própria janela) ou por código com o número. Várias contas podem ser pareadas; cada uma tem reconectar/desvincular.
2. Após conectar, o índice local começa a receber mensagens. Peça ao cérebro: "liste meus chats não lidos", "resuma o grupo X de hoje", "responda ao fulano com Y" (aprovação).
3. O backend real (`whatsapp-rust`) é compilado por padrão (`--features whatsapp-real`); para desligar: `cargo build --release -p will-server --no-default-features --features telegram,google,whatsapp`.

## App desktop (egui)
- Bandeja: menu Abrir / Voz / Sair; o ícone muda com o estado. No GNOME é preciso a extensão AppIndicator para o ícone aparecer (KDE, Cinnamon e XFCE funcionam direto).
- Fechar a janela só a esconde; reabra pela bandeja, por `will-ia --show` ou executando `will-ia` de novo (segunda instância pede à ativa que mostre a janela e sai).
- Base de conhecimento: o botão de pasta abre o diálogo nativo (`rfd`).
- WhatsApp: o QR é desenhado na própria janela.
- Em implementação (ADR-0016); ainda não validado em todas as plataformas.

## Modo portátil (pendrive)

```bash
mkdir -p /media/pendrive/will-ia/bin
cp target/release/will-ia /media/pendrive/will-ia/bin/will-ia-linux-x86_64   # e os binários dos outros SOs
cp packaging/portable/will-ia.sh packaging/portable/will-ia.cmd /media/pendrive/will-ia/
touch /media/pendrive/will-ia/portable
/media/pendrive/will-ia/will-ia.sh --show
```

Confira no log "modo portátil root=…", `segredos backend="file"`, o `will-ia.db` na raiz e a pasta `data/`. Em Configurações → Geral a seção "Dados" mostra o modo e os caminhos; em Base de conhecimento o botão "usar workspace/ do pendrive" cria uma KB relativa à raiz.

## Testes automatizados
```bash
cargo test --workspace --exclude will-desktop
cargo clippy --workspace --exclude will-desktop --all-targets -- -D warnings
```
