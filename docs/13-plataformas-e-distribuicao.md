# 13 — Plataformas e Distribuição

## "Serviço" = aplicação de sessão do usuário com autostart

O will-ia precisa de bandeja, áudio, atalho global e keyring, que só existem na **sessão gráfica do usuário**. Por isso ele **não** é um daemon de sistema (systemd system unit, Windows Service, launchd system daemon): é um app de bandeja iniciado no login. Ver [ADR-0010](adr/0010-user-session-app.md) e [ADR-0016](adr/0016-ui-nativa-egui.md) (binário único `will-ia`: app desktop egui com o serviço em processo; `will-ia serve` para headless).

| SO | Autostart | Tray | Atalho global | Keyring | Notificação | Seletor de pasta |
|---|---|---|---|---|---|---|
| Windows 10/11 (x64) | chave `Run` do registro (`will-platform`) | `tray-icon` (Shell_NotifyIcon) | `global-hotkey` (`RegisterHotKey`) | Credential Manager | Toast | `rfd` (IFileDialog) |
| macOS 12+ (x64 e arm64) | Login Item (`will-platform`; `launchd` agent como alternativa) | `tray-icon` (NSStatusItem) | `global-hotkey` (Carbon/CGEvent); exige permissão de Acessibilidade para PTT | Keychain | UNUserNotification | `rfd` (NSOpenPanel) |
| Linux (GNOME/KDE, x64) | `~/.config/autostart/will-ia.desktop` ou *user unit* do systemd | `ksni` (StatusNotifierItem via D-Bus) | `global-hotkey` em X11; Wayland não suporta (aviso + bandeja/painel como alternativa) | Secret Service (gnome-keyring/kwallet) | libnotify | `rfd` (portal XDG) |

Detalhes por SO:

- **Windows**: processos filhos em Job Object (`KILL_ON_JOB_CLOSE`); `python`/`py` ambos liberados; caminho do `claude` normalmente `%APPDATA%\npm\claude.cmd` (spawn via `cmd /C` ou resolver o `.cmd`).
- **macOS**: `caffeinate -i` opcional para não dormir com threads ativas (ou `IOPMAssertion`); permissões de microfone e acessibilidade solicitadas no onboarding; notarização necessária para distribuição fora da App Store. Duas builds separadas (x86_64 e aarch64), não universal.
- **Linux**: a bandeja depende de suporte a StatusNotifierItem no desktop (KDE, Cinnamon, XFCE suportam; GNOME exige a extensão AppIndicator). `libxkbcommon` e `libwayland`/X11 são carregados **em runtime** (presentes em qualquer desktop); nenhuma lib de GUI é necessária para compilar.

## Modo portátil (pendrive)

Além do modo instalado, o binário detecta um **pacote portátil** e passa a guardar tudo ao lado de si (ver `packaging/portable/README.md`):

```
will-ia/
├── will-ia.sh / will-ia.cmd   # lançadores: detectam SO e arquitetura e chamam bin/
├── portable                   # marcador (criado automaticamente se faltar)
├── will-ia.db                 # banco SQLite (raiz)
├── data/                      # secrets.json, whatsapp/<conta>/, logs/, cache/, api-port, ui-session.token
├── workspace/                 # (opcional) base de conhecimento levada no pendrive
└── bin/will-ia-<os>-<arch>    # linux-x86_64, linux-aarch64, macos-x86_64, macos-aarch64, windows-x86_64.exe
```

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

## Estado da implementação (2026-10-20)

- `crates/will-desktop` (egui/eframe) implementado conforme a ADR-0016 (validado só no Linux, sem teste interativo ainda); a UI React e a casca Tauri foram removidas. O núcleo (`will-server`) roda em qualquer SO sem libs de GUI.
- `packaging/` já tem `linux/will-ia.desktop`, `systemd/will-ia.service` (user unit) e `portable/` (lançadores `will-ia.sh`/`will-ia.cmd` + README). Bundles por SO e os workflows do GitHub Actions **ainda não existem**: estão documentados como planejados e marcados **a fazer**.

## Empacotamento por SO (a fazer)

| SO | Artefatos | Ferramenta |
|---|---|---|
| Linux x86_64 | `will-ia-<versão>-linux-x86_64.tar.gz` (binário + `packaging/linux/will-ia.desktop` + ícone + `packaging/systemd/will-ia.service`), `.deb` (`cargo-deb`), AppImage opcional | `cargo-deb`; AppImage com `linuxdeploy` |
| Windows x86_64 | `.zip` (binário) e instalador (`Inno Setup` ou `cargo-wix` → `.msi`) com opção de autostart via chave `Run` | `cargo-wix` ou script Inno Setup em `packaging/windows/` |
| macOS x86_64 | `will-ia.app` em `.dmg` | `cargo-bundle` ou script em `packaging/macos/` |
| macOS aarch64 | `will-ia.app` em `.dmg` (build separada) | idem |

Arquivos planejados em `packaging/`:

- `packaging/linux/will-ia.desktop`: entrada de menu/autostart (`Exec=will-ia`, `Exec=will-ia --show` para a ação "Abrir").
- `packaging/systemd/will-ia.service` (**user unit**, não system unit):

  ```ini
  [Unit]
  Description=will-ia (assistente pessoal: serviço + bandeja)
  After=graphical-session.target
  PartOf=graphical-session.target

  [Service]
  ExecStart=%h/.local/bin/will-ia
  Restart=on-failure

  [Install]
  WantedBy=graphical-session.target
  ```

  Instalação: `systemctl --user enable --now will-ia`. Como roda dentro da sessão gráfica, tem acesso ao D-Bus (bandeja, keyring), ao áudio e ao display. Em servidores sem sessão gráfica, use `ExecStart=... serve` (headless).
- `packaging/macos/`: `Info.plist`, ícone `.icns`, script de `.dmg`, Login Item para autostart; notarização (`notarytool`) quando houver certificado Developer ID.
- `packaging/windows/`: script Inno Setup ou `wix/` para `cargo-wix`; autostart via `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.

Ícones: `assets/icon.png` como fonte; `.ico`/`.icns` gerados por script. Versão única em `Cargo.toml` do workspace.

## CI/CD (GitHub Actions, a fazer)

Opção A: `cargo-dist` (gera `release.yml`, archives e instaladores por target). Opção B: workflow manual. Em ambos os casos, matriz **em paralelo** com os quatro targets:

| Job | Runner | Target |
|---|---|---|
| linux-x86_64 | `ubuntu-22.04` (glibc mais antiga = mais portátil) | `x86_64-unknown-linux-gnu` |
| windows-x86_64 | `windows-latest` | `x86_64-pc-windows-msvc` |
| macos-x86_64 | `macos-13` (Intel) ou `macos-latest` com `--target` | `x86_64-apple-darwin` |
| macos-aarch64 | `macos-latest` (Apple Silicon) | `aarch64-apple-darwin` |

- `ci.yml` (push/PR): na matriz acima, `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `cargo deny check`; no Linux, `sudo apt-get install -y libasound2-dev pkg-config` antes do build.
- `release.yml` (tag `v*`): mesma matriz → `cargo build --release -p will-desktop` → empacota conforme a tabela acima → assina quando há segredos (certificado Authenticode; Apple ID + certificado Developer ID para notarização) → publica a Release com `SHA256SUMS`.

## Dependências em tempo de execução

| Dependência | Obrigatória | Verificação |
|---|---|---|
| Claude Code CLI (`claude`) autenticado | sim | onboarding + seção Motor |
| Node.js ≥ 18 | não (scripts `tools/`) | informativo |
| Python ≥ 3.10 | não (scripts `tools/`) | informativo |
| Linux: `libasound2` (ALSA), `libxkbcommon`, X11 ou Wayland, D-Bus com StatusNotifierItem | sim para o app (não para `serve`) | presentes em qualquer desktop; GNOME precisa da extensão AppIndicator para a bandeja |
| Navegador padrão | para OAuth do Google | abre via `will-platform::open_url` |
| Telefone com WhatsApp | para parear | QR/código na janela |

## Logs e diagnóstico

Arquivos diários `will-ia-<data>.log` (e `claude-<data>.log` quando o stream bruto está ligado) com rotação via `tracing-appender`, retenção configurável (14 dias).

## Atualização

MVP: manual (notificação de nova versão via GitHub Releases, link de download). Auto-update assinado fica para depois, quando houver certificado (candidatos: `cargo-dist` com updater ou `self_update`).
