# 13 — Plataformas e Distribuição

## "Serviço" = aplicação de sessão do usuário com autostart

O will-ia precisa de bandeja, áudio, atalho global e keyring, que só existem na **sessão gráfica do usuário**. Por isso, na máquina pessoal, ele **não** é um daemon de sistema (systemd system unit, Windows Service, launchd system daemon): é iniciado no login. Ver [ADR-0010](adr/0010-user-session-app.md), [ADR-0016](adr/0016-ui-nativa-egui.md) (UI nativa egui) e [ADR-0019](adr/0019-cliente-servidor.md) (dois binários: servidor + app; `docs/26`). A exceção é o servidor em Linux sem tela, que roda como `will-ia serve` sob systemd (user unit com *linger*, ou system unit) com `--file-secrets`.

### Dois binários: `will-ia` (servidor) e `will-ia-app` (app)

Desde a etapa 3 de `docs/26` (2026-10-28) cada plataforma recebe **dois executáveis, instalados lado a lado** (o servidor acha o app na própria pasta e, depois, no PATH):

| | `will-ia` (servidor) | `will-ia-app` (app) |
|---|---|---|
| O que é | O serviço: motor Claude, KB, módulos (Telegram, MCP, WhatsApp, Google, Scheduler, Browser), banco, segredos, API local (`docs/22`) | A janela egui: cliente HTTP/WS do servidor via `will-client`; perfis de conexão; microfone/alto-falante e atalho PTT |
| Residente? | **Sim.** `will-ia` sem argumentos ignora SIGINT/SIGTERM/SIGHUP; só kill forçado derruba. `will-ia serve` (headless) encerra limpo com Ctrl+C/SIGTERM | **Não.** Abriu, conectou, fechou. Fechar a janela (X, Esc) só a **oculta enquanto o processo do app existir** (atalho PTT e ipc continuam); não há "Sair" na janela |
| Bandeja | Só com sessão gráfica (`DISPLAY`/`WAYLAND_DISPLAY`; sempre no Windows/macOS) e sem `--no-tray`: `ksni` no Linux, `tray-icon` nos demais. **Um item: "Abrir will-ia"** (clique no ícone faz o mesmo), que lança o app. Sem "Sair", sem item de voz | Nenhuma |
| Instância única | Trinco `instance.lock` no `data_dir`; segunda execução de `will-ia` só abre o app e sai (`serve` com trinco ocupado falha com erro) | Trinco `app.lock` + TCP em loopback com token (`<config>/will-ia-app/ipc`); segunda execução manda `show` à instância aberta e sai (insiste ~3 s se ela ainda está subindo) |
| CLI | `will-ia [--show] [--no-tray] [--data-dir D] [--file-secrets] [--port N]`, `serve`, `info` (caminhos, URL, token), `show` (abre o app), `screenshot --nav <pagina>` (delega ao app) | `will-ia-app [--show\|--hidden] [--url URL --token T] [--profile nome]`, `show`, `screenshot --nav <pagina>` (→ `<config>/will-ia-app/screenshot.png`), `audio-devices` |
| Descoberta | Grava `<data_dir>/api-port` e `<data_dir>/ui-session.token` | Perfil "Esta máquina" lê esses dois arquivos; perfis remotos em `<config>/will-ia-app/profiles.json` |
| Onde roda | Máquina pessoal (bandeja) **ou** servidor Linux sem tela (`serve`) | Qualquer máquina com desktop, inclusive outra que não a do servidor |

Comportamento para o usuário: o mesmo de antes; a diferença visível é que na máquina pessoal há **dois processos** (servidor na bandeja + app) e, sem servidor alcançável, o app abre na página **Conexão**.

**Autostart do servidor por SO** (o app não precisa de autostart: abre pela bandeja, pelo menu/atalho do SO ou por `will-ia show`):

- **Linux com desktop**: `~/.config/autostart/will-ia-server.desktop` com `Exec=will-ia` (residente + bandeja; a entrada de menu `packaging/linux/will-ia.desktop` é do **app**: `Exec=will-ia-app --show`). Alternativa supervisionada: a user unit `packaging/systemd/will-ia.service` (`ExecStart=/usr/bin/will-ia serve`, sem bandeja; `systemctl --user enable --now will-ia`). "Iniciar com o sistema" em Geral grava a entrada de autostart do servidor.
- **Linux sem tela (servidor)**: ver "Servidor headless" abaixo.
- **Windows**: chave `Run` do registro (`will-platform`) apontando para `will-ia.exe`; o app é aberto pela bandeja. Agendador de Tarefas se quiser reinício automático.
- **macOS**: Login Item (`will-platform`) ou `launchd` agent para `will-ia`; o `.app` distribuído tem `will-ia-app` como executável principal e `will-ia` ao lado em `Contents/MacOS/`, então a bandeja encontra o app na mesma pasta.

### Servidor headless (Linux sem tela, equipe)

`will-ia serve` numa máquina sem sessão gráfica é um servidor de verdade desde a etapa 7 de `docs/26`:

- **Detecção**: `will_platform::has_desktop()` (= `DISPLAY` ou `WAYLAND_DISPLAY` presente; sempre verdadeiro em Windows/macOS). Sem desktop: nenhuma tentativa de bandeja; **Browser CDP lança o Chrome com `--headless=new` mesmo que `browser.headless` esteja desligado** (a página avisa; `docs/25`); **notificações** nativas não são emitidas no servidor: `Event::Notification` e `ApprovalRequested` chegam pelo WS `/api/events` aos apps conectados, que mostram toast/notificação na máquina de cada um (quando a janela está oculta, notificação nativa).
- **Segredos**: sem Secret Service use `--file-secrets` (`<data_dir>/secrets.json`, 0600). Keyring em servidor sem D-Bus costuma falhar e deixa os módulos `Degraded`.
- **Instalação** (`packaging/systemd/will-ia.service`, user unit): `mkdir -p ~/.config/systemd/user && cp will-ia.service ~/.config/systemd/user/`, ajuste `ExecStart=/usr/bin/will-ia serve --file-secrets --bind 0.0.0.0` (ou deixe o `bind` nas configurações), `systemctl --user daemon-reload && systemctl --user enable --now will-ia`, e `loginctl enable-linger $USER` para o serviço sobreviver ao logout. System unit (`/etc/systemd/system`, `User=will`) é equivalente. Chrome/Chromium instalado para o Browser CDP (`agent-browser install` baixa um Chromium headless-friendly).
- **Primeiro acesso**: `will-ia info` mostra URL, token local e a senha inicial do `admin`; o admin entra pelo app (perfil com URL + usuário + senha), troca a senha e cria os membros (`docs/10` Usuários).
- **Exposição segura** (`docs/12` regra 16), do mais simples ao mais flexível:
  1. **Tailscale / WireGuard** (recomendado): servidor com `bind` no IP da VPN (ex.: `100.x.y.z`) e `allow_remote_without_tls = true`, ou TLS autoassinado por cima; só quem está na rede chega à porta.
  2. **TLS embutido**: `network.tls = true` gera `<data_dir>/tls/cert.pem`; o admin copia o PEM (Configurações → Rede ou `GET /api/network/cert`) e cola no perfil de cada app (`ca_pem`). Com certificado da CA interna, aponte `cert_path`/`key_path`.
  3. **nginx/Caddy como proxy reverso** com certificado próprio (Let's Encrypt): `bind = 127.0.0.1` no will-ia, proxy em `https://will.exemplo.com` com `proxy_pass http://127.0.0.1:47300`, **WebSocket habilitado** (`Upgrade`/`Connection` para `/api/events` e `/api/voice/audio`), `client_max_body_size 64m` (uploads) e `proxy_read_timeout` ≥ 10 min (WS de voz). Perfis do app apontam para a URL pública sem `ca_pem`.
  Nunca `bind = 0.0.0.0` sem TLS na internet aberta: o servidor recusa por padrão e volta para loopback.
- **Firewall**: só a porta da API (padrão 47300). MCP (`127.0.0.1`), proxy CDP (`127.0.0.1:9223`) e Chrome (`9222`) continuam em loopback e não devem ser expostos.

### Ferramentas fora do `PATH` (macOS, autostart, systemd)

Um app aberto pelo Finder/launchd, pelo autostart ou por uma unit do systemd recebe um `PATH` mínimo (`/usr/bin:/bin:/usr/sbin:/sbin`), sem Homebrew (`/opt/homebrew/bin`), nvm, volta, fnm, npm global ou pyenv. Por isso `will_platform::which` procura também nessas pastas (`tool_dirs()`), e o processo `claude` recebe `PATH` aumentado (`augmented_path()`), para o script `claude` achar o `node`. Sintoma antes da correção (2026-10-28): Base de conhecimento mostrava "Node.js não encontrado" no macOS mesmo com o Node instalado.

### Serviço residente: difícil de derrubar, instância única

O **servidor** é a peça que mantém tudo de pé (MCP para os processos `claude`, Telegram, voz, agendamentos). Por isso, a partir de 2026-10-28:

- **Não há "Sair"** na bandeja do servidor. A janela do app usa a **barra de título e os botões do próprio SO** (sem barra customizada); **fechar a janela encerra o processo do app** — ele é só o cliente, o servidor continua residente. `will-ia-app` de novo, `will-ia show` ou o ícone da bandeja abrem o app outra vez (já aberto → ipc traz a janela para frente; fechado → o servidor lança o binário ao lado).
- **macOS: o servidor é um app acessório** (ADR-0022). Antes do `NSStatusItem` ele aplica a política `Accessory` (sem ícone no Dock nem no Cmd+Tab) e entrega a thread principal ao run loop do AppKit (`[NSApp run]`), que a bandeja exige para desenhar e receber cliques; a espera do serviço roda na thread `will-wait`. Abrir o app a partir do servidor usa o **LaunchServices** (`will-ia-app show` por ipc + `open -a <bundle>`), e o próprio app não chama `Focus` quando não está ativo (o macOS 14+ nega a auto-ativação e faz o ícone pular): quem o traz para frente é o `open -a` no próprio bundle (`will_platform::macos`).
- **Sinais são ignorados no servidor residente** (`will-ia` sem `serve`): SIGINT (Ctrl+C), SIGTERM e SIGHUP só geram um aviso no log. Derrubar é só com kill forçado (`kill -9 <pid>` no Linux/macOS; "Finalizar tarefa" no Gerenciador de Tarefas do Windows). `will-ia serve` (headless) encerra limpo com Ctrl+C/SIGTERM, porque é o que systemd espera. O app encerra normalmente (não é residente).
- **Filhos morrem junto mesmo no kill forçado**: no Linux cada `claude` recebe `PR_SET_PDEATHSIG = SIGKILL` (`will_platform::process::die_with_parent`); no Windows todos os filhos entram em um Job Object com `KILL_ON_JOB_CLOSE` (`will_platform::process::attach_to_job`), que o SO fecha quando o will-ia some. No macOS fica só o grupo de processos (shutdown limpo). O app lançado pela bandeja **não** é filho supervisionado: sobrevive ao servidor e mostra "sem conexão".
- **Instância única por diretório de dados**: o servidor toma um trinco `instance.lock` no `data_dir` (`flock` exclusivo no Unix, abertura sem compartilhamento no Windows; `will_platform::instance`). Se o trinco está ocupado, `will-ia` só lança o app e sai; `POST /api/ui/show` continua existindo e faz o mesmo. O SO solta o trinco quando o processo morre, inclusive por `kill -9`, então não há trinco órfão. O app tem o seu próprio (`app.lock`, acima).
- O que **não** faz: reiniciar sozinho depois de um kill. Para isso, o autostart no login cobre a reinicialização da máquina; um supervisor (systemd user unit com `Restart=always`, Agendador de Tarefas no Windows) fica a critério do usuário.

| SO | Autostart | Tray | Atalho global | Keyring | Notificação | Seletor de pasta |
|---|---|---|---|---|---|---|
| Windows 10/11 (x64) | chave `Run` do registro (`will-platform`) → `will-ia.exe` | `tray-icon` (Shell_NotifyIcon), no servidor | `global-hotkey` (`RegisterHotKey`), no app | Credential Manager | Toast | `rfd` (IFileDialog), no app |
| macOS 12+ (x64 e arm64) | Login Item (`will-platform`; `launchd` agent como alternativa) → `will-ia` | `tray-icon` (NSStatusItem), no servidor | `global-hotkey` (Carbon `RegisterEventHotKey`), no app; **não** exige Acessibilidade nem Monitoramento de entrada | Keychain | UNUserNotification | `rfd` (NSOpenPanel), no app |
| Linux (GNOME/KDE, x64) | `~/.config/autostart/` (`Exec=will-ia`) ou *user unit* do systemd (`will-ia serve`) | `ksni` (StatusNotifierItem via D-Bus), no servidor | `global-hotkey` em X11, no app; Wayland não suporta (aviso + bandeja/painel como alternativa) | Secret Service (gnome-keyring/kwallet) | libnotify | `rfd` (portal XDG), no app |

Detalhes por SO:

- **Windows**: processos filhos em Job Object (`KILL_ON_JOB_CLOSE`); `python`/`py` ambos liberados; caminho do `claude` normalmente `%APPDATA%\npm\claude.cmd` (spawn via `cmd /C` ou resolver o `.cmd`). **Todo processo filho** (claude, `taskkill`, `--version` de node/python, teste de MCP externo) é criado com `CREATE_NO_WINDOW` via `will_core::process::hide_console`; sem isso o app GUI faz piscar uma janela de console a cada spawn.
- **macOS**: `caffeinate -i` opcional para não dormir com threads ativas (ou `IOPMAssertion`); permissões de microfone e acessibilidade solicitadas no onboarding; notarização necessária para distribuição fora da App Store. Duas builds separadas (x86_64 e aarch64), não universal.
- **Linux**: a bandeja depende de suporte a StatusNotifierItem no desktop (KDE, Cinnamon, XFCE suportam; GNOME exige a extensão AppIndicator; em Cinnamon sem o applet o servidor tenta subir o `xapp-sn-watcher`). Sem `DISPLAY`/`WAYLAND_DISPLAY` o servidor nem tenta a bandeja. `libxkbcommon` e `libwayland`/X11 são carregados **em runtime** pelo app (presentes em qualquer desktop); nenhuma lib de GUI é necessária para compilar; o servidor não carrega nenhuma delas.

## Modo portátil (pendrive)

Além do modo instalado, os binários detectam um **pacote portátil** e passam a guardar tudo ao lado de si (ver `packaging/portable/README.md`):

```
will-ia/
├── will-ia.sh / will-ia.cmd   # lançadores: detectam SO e arquitetura, sobem bin/will-ia-… (servidor) e abrem bin/will-ia-app-… (app)
├── portable                   # marcador (criado automaticamente se faltar)
├── will-ia.db                 # banco SQLite (raiz)
├── data/                      # secrets.json, whatsapp/<conta>/, logs/, cache/, api-port, ui-session.token
├── workspace/                 # (opcional) base de conhecimento levada no pendrive
└── bin/will-ia-<os>-<arch>, bin/will-ia-app-<os>-<arch>   # linux-x86_64, macos-x86_64, macos-aarch64, windows-x86_64.exe
```

Os lançadores chamam o servidor (se já está no ar, a segunda execução só abre o app e sai), esperam `data/api-port` e abrem o app, que conecta em "Esta máquina". Os perfis do app ficam na máquina hospedeira (`<config>/will-ia-app/`), não no pendrive.

**Desde 2026-10-29 o pacote portátil não é mais publicado na release** (`will-ia-<v>-portable.zip` saiu do `release.yml`). O modo continua existindo no código e em `packaging/portable/`: **pode ser montado à mão** com os binários do `.tar.gz`/`.zip` de cada plataforma, renomeados para `bin/will-ia-<os>-<arch>` e `bin/will-ia-app-<os>-<arch>` conforme o `README.md` da pasta.

Regras (`will_core::AppPaths`):

- Detecção: arquivo `portable`, ou `will-ia.db`, ou `data/` + `bin/` na pasta do executável ou na pasta acima dela (layout `bin/`); `WILL_IA_PORTABLE=1` força; `WILL_IA_DATA_DIR` sobrescreve tudo.
- Segredos **em arquivo** (`data/secrets.json`): o keyring da máquina não serve, porque o pendrive muda de máquina. Quem tem o pendrive tem os tokens; proteja-o.
- A pasta da base de conhecimento pode ser **relativa** à raiz do pacote (ex.: `workspace`), para viajar junto; caminhos absolutos continuam válidos (e podem não existir na outra máquina: o motor fica `Degraded` até apontar outra pasta).
- "Iniciar com o sistema" fica desabilitado; o `.mcp.json` da KB é regravado a cada inicialização com a URL/porta atuais do servidor MCP.
- O que precisa existir na máquina hospedeira: o Claude Code CLI (`claude`) autenticável, e opcionalmente Node/Python para scripts da KB.

## Dependências de build

| SO | Toolchain | Libs de sistema em tempo de build |
|---|---|---|
| Linux | Rust estável | `libasound2-dev` (ALSA, exigido pelo `cpal`) e `pkg-config`. Só isso: sem webkit, gtk, Node. |
| Windows | Rust estável (MSVC) | nenhuma |
| macOS | Rust estável + Xcode CLT | nenhuma |

Sem `sudo` para instalar `libasound2-dev`, é possível apontar `PKG_CONFIG_PATH` para um sysroot local com os headers da ALSA (truque usado nesta máquina de desenvolvimento; ver `docs/23`). O normal é `sudo apt install libasound2-dev`.

## Estado da implementação (2026-10-28)

- Dois binários (etapa 3 de `docs/26`): `crates/will-ia` (servidor + bandeja) e `crates/will-app` (app egui, cliente via `crates/will-client`); o antigo `crates/will-desktop` não existe mais. Validado só no Linux (headless pelo `smoke.sh`; GUI sem teste interativo completo). O núcleo (`will-server`) e o servidor rodam em qualquer SO sem libs de GUI.
- `packaging/` tem `linux/will-ia.desktop` (entrada de menu do **app**), `systemd/will-ia.service` (user unit do **servidor**, `serve`), `macos/Info.plist` + `macos/Setup-Info.plist` e `portable/` (lançadores + README, montagem manual). O `ci.yml` compila `-p will-ia -p will-app`; o `release.yml` compila os três binários e publica **um pacote por SO/arquitetura** com o instalador dentro (seção CI/CD).

## Instalador (`will-ia-setup`)

Desde a v0.1.5 cada release traz um **terceiro binário**, o instalador por usuário (`docs/27`, ADR-0021): assistente gráfico (ou `--cli`) que grava os dois binários na pasta do usuário, registra o servidor no login (LaunchAgent / autostart ou user unit / chave Run), cria atalhos e, numa atualização, para o que está rodando, troca os binários e religa, preservando a pasta de dados. É o caminho recomendado na máquina pessoal. Desde 2026-10-29 ele **vem dentro do pacote de cada plataforma** (não há mais download separado do instalador): descompacte e abra `Instalar will-ia.app` / `will-ia-setup.exe` / `./will-ia-setup`. Os binários soltos no mesmo pacote servem para instalação manual/de sistema.

## Ícones

Fonte única em `assets/icon/` (`will-ia.svg` = squircle azul com onda de voz; `tray-template.svg` = só as barras, para a barra de menus do macOS). `assets/icon/gerar.sh` produz os PNGs (16–1024), o `will-ia.icns` e o `will-ia.ico`. Onde cada um entra:

| Onde | Como |
|---|---|
| Janela, barra de tarefas (Windows/Linux), Dock quando é o ícone da janela | `eframe` `with_icon` com o PNG 256 decodificado por `will_platform::icon` (embutido nos binários do app e do instalador) |
| Dock / Finder / Launchpad (macOS) | `Contents/Resources/will-ia.icns` + `CFBundleIconFile` nos dois bundles (`will-ia.app`, `Instalar will-ia.app`); o release copia o `.icns`, e o `will-ia-setup` o grava ao instalar (e dá `touch` no bundle para o Finder renovar o cache) |
| Barra de menus do macOS | `tray-icon` com o PNG *template* 44 px e `with_icon_as_template(true)` (preto/branco conforme o tema) |
| Bandeja do Windows | `tray-icon` com o PNG colorido 32 px |
| Bandeja do Linux (StatusNotifierItem) | `ksni` com pixmaps ARGB 22/32/48 vindos dos PNGs |
| Menu/dock/alt-tab do Linux | `Icon=will-ia` no `.desktop` + PNGs instalados em `~/.local/share/icons/hicolor/<n>x<n>/apps/will-ia.png` pelo `will-ia-setup` (todos os tamanhos; `gtk-update-icon-cache` quando existe); o `.deb` instala em `/usr/share/icons/hicolor/` (48, 128, 256, 512) |
| `.exe` do Windows (Explorer, atalhos, barra de tarefas) | recurso de ícone embutido por `build.rs` + `winresource` (`will-ia.ico`) nos três binários; os `.lnk` do instalador apontam `IconLocation` para o `.exe` do app |

## Empacotamento por SO

Desde 2026-10-29 a release publica **um pacote por SO (e por arquitetura no macOS)**. Todo pacote descompacta numa pasta `will-ia-<versão>-<plataforma>/` com **os três binários** (`will-ia-setup`, `will-ia`, `will-ia-app`; `.exe` no Windows) mais `README.md`, `LICENSE` e `docs/`. O fluxo do usuário é: baixar, descompactar, abrir o instalador.

| SO | Arquivo | Conteúdo | Ferramenta |
|---|---|---|---|
| macOS aarch64 (Apple Silicon) | `will-ia-<versão>-macos-aarch64.zip` | `Instalar will-ia.app` (bundle do instalador: `packaging/macos/Setup-Info.plist`, executável `will-ia-setup`) + `will-ia.app` (`Contents/MacOS/will-ia-app` = `CFBundleExecutable` e `Contents/MacOS/will-ia` ao lado, `packaging/macos/Info.plist`) + `README.md`, `LICENSE`, `docs/` | `zip -r -y` na pasta de staging (preserva bits de execução e a estrutura dos bundles) |
| macOS x86_64 (Intel) | `will-ia-<versão>-macos-x86_64.zip` | idem (build separada, não universal) | idem |
| Windows x86_64 | `will-ia-<versão>-windows-x86_64.zip` | `will-ia-setup.exe`, `will-ia.exe`, `will-ia-app.exe`, `README.md`, `LICENSE`, `docs/` | `7z a -tzip` |
| Linux x86_64 | `will-ia-<versão>-linux-x86_64.tar.gz` | `will-ia-setup`, `will-ia`, `will-ia-app` (executáveis), `README.md`, `LICENSE`, `docs/`, `packaging/linux/` (`.desktop` do app), `packaging/systemd/` (user unit do servidor) | `tar -czf` |
| Linux Debian/Ubuntu | `will-ia_<versão>-1_amd64.deb` | `/usr/bin/{will-ia,will-ia-app,will-ia-setup}`, `.desktop`, user unit, `README` em `/usr/share/doc/will-ia/`; `sudo apt install ./will-ia_<versão>-1_amd64.deb` | `cargo-deb -p will-ia` (assets em `crates/will-ia/Cargo.toml`) |
| todos | `SHA256SUMS.txt` | hashes de todos os arquivos acima | `sha256sum` |

**Não publicados** (desde 2026-10-29): pacote portátil (montagem manual, seção acima), `.app.zip` separados do app e do instalador, binários soltos por plataforma (`will-ia-<plat>`), `.tar.gz` do macOS. Futuro: `.dmg` e notarização no macOS; `.msi` (`cargo-wix`) ou Inno Setup no Windows; AppImage opcional.

Arquivos em `packaging/`:

- `packaging/linux/will-ia.desktop`: entrada de menu do **app** (`Exec=will-ia-app --show`, `StartupWMClass=will-ia`). O autostart do **servidor** é outra entrada em `~/.config/autostart/` com `Exec=will-ia` (gerada por "Iniciar com o sistema").
- `packaging/systemd/will-ia.service` (**user unit** do servidor, não system unit): `ExecStart=/usr/bin/will-ia serve` (headless: sem bandeja, SIGTERM limpo), `Restart=on-failure`, `WantedBy=default.target`. Instalação: `systemctl --user enable --now will-ia`; em servidor sem sessão, `loginctl enable-linger $USER` e `--file-secrets`. Numa máquina com desktop, a alternativa é o autostart da sessão rodar `will-ia` sem argumentos (residente + bandeja).
- `packaging/macos/`: `Info.plist` (`CFBundleExecutable` = `will-ia-app`), ícone `.icns`, script de `.dmg`, Login Item para autostart do servidor; notarização (`notarytool`) quando houver certificado Developer ID.
- `packaging/windows/`: script Inno Setup ou `wix/` para `cargo-wix` (os dois `.exe`); autostart do servidor via `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
- `packaging/portable/`: `will-ia.sh`, `will-ia.cmd`, `README.md` (seção "Modo portátil").

Ícones: `assets/icon.png` como fonte; `.ico`/`.icns` gerados por script. Versão única em `Cargo.toml` do workspace.

## CI/CD (GitHub Actions)

- `.github/workflows/ci.yml`: a cada push/PR em `main` (ignora mudanças só em docs): `cargo fmt --check`, `clippy -D warnings`, `cargo test --workspace` e o smoke headless no Linux (`cargo build -p will-ia -p will-app -p fake-claude` + `scripts/smoke.sh`, que usa `will-ia serve`); `cargo check -p will-ia -p will-app` no Windows e no macOS em paralelo. Um push novo cancela o run anterior.
- `.github/workflows/release.yml`: em tag `v*` (ou manual), numa matriz em paralelo, cada job compila `cargo build --release -p will-ia -p will-app` e **depois** `-p will-setup` com `WILL_SETUP_PAYLOAD_DIR=${{ github.workspace }}/target/<triple>/release` (caminho absoluto; o instalador embute os dois binários recém-compilados), e monta **um pacote**:

| Job | Runner | Target | Artefato |
|---|---|---|---|
| linux-x86_64 | `ubuntu-22.04` | `x86_64-unknown-linux-gnu` | `will-ia-<v>-linux-x86_64.tar.gz` + `will-ia_<v>-1_amd64.deb` (cargo-deb `--no-build`) |
| windows-x86_64 | `windows-latest` | `x86_64-pc-windows-msvc` | `will-ia-<v>-windows-x86_64.zip` |
| macos-x86_64 | `macos-latest` (compilação cruzada para Intel) | `x86_64-apple-darwin` | `will-ia-<v>-macos-x86_64.zip` |
| macos-aarch64 | `macos-latest` (Apple Silicon) | `aarch64-apple-darwin` | `will-ia-<v>-macos-aarch64.zip` |

  O job final `release` só junta os artefatos, gera `SHA256SUMS.txt` e publica a GitHub Release com notas automáticas mais um corpo fixo em português (o que é cada arquivo e como instalar). Conteúdo de cada pacote na seção "Empacotamento por SO".
- Publicar uma versão: `git tag v0.1.0 && git push origin v0.1.0`.
- Assinatura/notarização (macOS) e instalador Windows ficam para depois. Sem notarização, o Gatekeeper mostra "Apple could not verify … is free of malware" (macOS 15 não aceita mais o botão direito → Abrir). Para liberar:
  - pacote da release: `xattr -dr com.apple.quarantine "Instalar will-ia.app" will-ia.app` (dentro da pasta descompactada);
  - binários soltos (ex.: portátil): `xattr -d com.apple.quarantine bin/will-ia-macos-aarch64 bin/will-ia-app-macos-aarch64 && chmod +x …`;
  - ou, depois da primeira tentativa, Ajustes do Sistema › Privacidade e Segurança › "Abrir Mesmo Assim".
  O `packaging/portable/README.md` repete essa instrução. Com um certificado Developer ID, o `release.yml` passa a assinar (`codesign --options runtime`) e notarizar (`notarytool`), e o aviso some.

## Dependências em tempo de execução

| Dependência | Obrigatória | Verificação |
|---|---|---|
| Claude Code CLI (`claude`) autenticado | sim | onboarding + seção Motor |
| Node.js ≥ 18 | não (scripts `tools/`) | informativo |
| Python ≥ 3.10 | não (scripts `tools/`) | informativo |
| Linux: `libasound2` (ALSA), `libxkbcommon`, X11 ou Wayland | sim para o `will-ia-app` (não para o servidor) | presentes em qualquer desktop |
| Linux: D-Bus com StatusNotifierItem | só para a bandeja do servidor `will-ia` (sem ela o servidor segue; `will-ia show` abre o app) | GNOME precisa da extensão AppIndicator |
| Navegador padrão | para OAuth do Google | abre via `will-platform::open_url` |
| Telefone com WhatsApp | para parear | QR/código na janela |

## Logs e diagnóstico

Arquivos diários `will-ia-<data>.log` (e `claude-<data>.log` quando o stream bruto está ligado) com rotação via `tracing-appender`, retenção configurável (14 dias).

## Atualização

MVP: manual (notificação de nova versão via GitHub Releases, link de download). Auto-update assinado fica para depois, quando houver certificado (candidatos: `cargo-dist` com updater ou `self_update`).
