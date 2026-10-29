# 27 — Instalador (`will-ia-setup`)

Terceiro binário do projeto, ao lado do servidor (`will-ia`) e do app (`will-ia-app`): um
**assistente de instalação/atualização por usuário** (sem sudo/UAC) para Windows, Linux e macOS.
Ele **não configura nada do produto** (Telegram, WhatsApp, Google, Claude…): isso é feito dentro
do app. Ele só coloca os binários no lugar, registra o servidor para subir com o sistema, cria
atalhos e, numa atualização, para o que está rodando, troca os binários e religa — **preservando
os dados**. Decisão: [ADR-0021](adr/0021-instalador-por-usuario.md).

Crate: `crates/will-setup` (bin `will-ia-setup`). Módulos: `plan.rs` (detecção, sem efeitos),
`os.rs` (tudo que é específico de SO), `install.rs` (sequência de passos, comum à GUI e à CLI),
`gui.rs` (assistente egui), `cli.rs` (modo texto), `payload.rs` + `build.rs` (binários embutidos).

## Payload embutido

O `build.rs` copia `will-ia` e `will-ia-app` (com `.exe` no Windows) da pasta apontada por
**`WILL_SETUP_PAYLOAD_DIR`** para o `OUT_DIR` e o `payload.rs` os embute com `include_bytes!`. É por
isso que um só arquivo instala tudo (≈ 65 MB). Sem a variável, o instalador compila com payload
vazio (CI, `cargo check`) e em execução avisa "compilado SEM os binários" e não instala.

```bash
cargo build --release -p will-ia -p will-app
WILL_SETUP_PAYLOAD_DIR="$PWD/target/release" cargo build --release -p will-setup   # caminho ABSOLUTO (o build.rs roda em crates/will-setup)
# → target/release/will-ia-setup (≈ 75 MB; 9 MB = sem payload, confira o aviso do build)
```

A versão instalada é a do workspace no momento do build (`CARGO_PKG_VERSION`), a mesma dos dois
binários embutidos: o `release.yml` compila os três no mesmo job.

## Onde instala (por usuário)

| | macOS | Linux | Windows |
|---|---|---|---|
| Binários | `~/Applications/will-ia.app/Contents/MacOS/{will-ia-app,will-ia}` (+ `Info.plist` do release) | `~/.local/bin/{will-ia,will-ia-app}` | `%LOCALAPPDATA%\Programs\will-ia\{will-ia.exe,will-ia-app.exe}` |
| Início com o sistema | LaunchAgent `~/Library/LaunchAgents/dev.will.will-ia.plist` (`RunAtLoad`, `KeepAlive`, `ProcessType Interactive`, `PATH` com Homebrew/nvm/`~/.local/bin`, logs em `<data_dir>/logs/launchd.*.log`) | com sessão gráfica: `~/.config/autostart/will-ia.desktop` (`Exec=<bin>/will-ia`, residente + bandeja); sem tela: user unit `~/.config/systemd/user/will-ia.service` (`will-ia serve`, `Restart=on-failure`, `enable`) | valor `will-ia` em `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` |
| Atalhos | symlink `~/Desktop/will-ia.app` (Launchpad já vê `~/Applications`) | `~/.local/share/applications/will-ia.desktop` (menu) + cópia na Área de Trabalho (`xdg-user-dir DESKTOP`, executável, `gio … metadata::trusted`) | `.lnk` no Menu Iniciar (`%APPDATA%\…\Start Menu\Programs`) e na Área de Trabalho (PowerShell `WScript.Shell`) |
| Dados (nunca tocados) | `~/Library/Application Support/dev.will.will-ia` | `~/.local/share/will-ia` | `%APPDATA%\will\will-ia\data` |

A pasta de instalação pode ser trocada em "Avançado" (GUI) ou `--dir` (CLI). O `data_dir` é o do
`will_core::AppPaths` (`directories::ProjectDirs("dev","will","will-ia")`; `WILL_IA_DATA_DIR` sobrepõe).

## Fluxo do assistente

1. **Boas-vindas** — o que vai acontecer; versão instalada → nova.
2. **Verificações** (`Plan::detect`, só leitura): SO/arquitetura e se há sessão gráfica; payload
   presente e tamanho; pasta de instalação gravável (cria e escreve um arquivo de prova); instalação
   existente e versão (`<bin> --version`, 5 s de limite); dados existentes e tamanho do banco; serviço
   registrado/em execução; processos `will-ia`/`will-ia-app` vivos (`pgrep -x` / `tasklist`);
   **instalação manual anterior** (LaunchAgent `com.will.will-ia` da máquina de origem); avisos
   (downgrade, reparo da mesma versão, sem permissão).
3. **Opções** — iniciar com o sistema; atalho na área de trabalho; iniciar o servidor ao terminar;
   abrir o app ao terminar. (Remoção: apagar também os dados, com aviso de irreversível.)
4. **Instalação** (`install::install`, thread própria, log ao vivo):
   1. parar: `launchctl bootout` do LaunchAgent (novo e legado) / `systemctl --user stop`; encerrar
      `will-ia-app` e `will-ia`; o servidor residente **ignora SIGTERM**, então após 5 s vai
      `pkill -9` / `taskkill /F`; espera sumirem;
   2. gravar: cada binário vai para `<nome>.new` (0755) e é renomeado por cima (no Windows remove
      antes, com tentativas, porque exe em uso não pode ser sobrescrito); no macOS monta/atualiza o
      `.app` com o `Info.plist` do release e tira a quarentena do bundle;
   3. serviço: escreve o LaunchAgent/unit/autostart/Run e carrega; no macOS **absorve** o LaunchAgent
      manual antigo (bootout + renomeia para `.plist.bak.<data>`) e remove o Login Item "will-ia"
      que o app criava via `auto-launch` (evita dois servidores no login — o segundo só abriria o app);
   4. atalhos; 5. manifesto `<data_dir>/install-manifest.json` (versão, caminhos, serviço, data);
   6. iniciar: `launchctl kickstart -k` / `systemctl --user restart` / processo destacado; espera
      `GET /api/health` na porta de `<data_dir>/api-port` (até 40 s) e abre o app (`will-ia-app --show`).
5. **Concluído** — resumo, URL da API local e o log completo.

Nada da pasta de dados é apagado em nenhum passo da instalação. `--uninstall` remove serviço,
atalhos, binários e manifesto e **preserva os dados**; `--purge` apaga a pasta de dados também.

## CLI

```
will-ia-setup                      # assistente gráfico (se há sessão gráfica)
will-ia-setup --cli                # mesmas perguntas, no terminal
will-ia-setup --yes                # instala com os padrões, sem perguntar (servidor headless, automação)
will-ia-setup --status [--json]    # só mostra o que foi detectado
will-ia-setup --uninstall [--purge] [--yes]
flags: --dir <pasta> --no-autostart --no-shortcut --no-start --no-open
```
Sem `DISPLAY`/`WAYLAND_DISPLAY` (Linux servidor) o modo texto é automático. No Windows o binário é
`windows_subsystem = "windows"` (sem console preto na GUI) e, em modo texto, religa o console do pai
(`will_platform::attach_parent_console`). Códigos de saída: 0 ok · 1 cancelado · 2 não dá para
instalar · 3 falhou · 4 sem janela.

## Interação com o app

A seção Geral → "Iniciar com o sistema" do app usa `auto-launch` (Login Item no macOS, `.desktop`
no Linux, chave Run no Windows). No Linux e no Windows são **os mesmos** artefatos que o instalador
escreve (compatíveis). No macOS seriam dois mecanismos; por isso, quando existe
`<data_dir>/install-manifest.json`, o app **não aplica** o `auto-launch` e mostra que o início com o
sistema é gerenciado pelo instalador.

## Release

`release.yml` compila `will-ia` + `will-ia-app` e, no mesmo job, `will-setup` com
`WILL_SETUP_PAYLOAD_DIR=target/<triple>/release`. Artefatos: `will-ia-setup-<versão>-linux-x86_64`,
`will-ia-setup-<versão>-windows-x86_64.exe` e `will-ia-setup-<versão>-macos-{x86_64,aarch64}.app.zip`
("Instalar will-ia.app", `packaging/macos/Setup-Info.plist`). Não assinado/notarizado: no macOS o
usuário libera em Privacidade e Segurança ou `xattr -dr com.apple.quarantine "Instalar will-ia.app"`;
no Windows o SmartScreen pede "Executar assim mesmo".

## Limites conhecidos

- Sem ícone próprio ainda (`Icon=will-ia` no Linux não resolve; `.lnk` usa o ícone do exe).
- Não há rollback automático: a versão anterior não é guardada (os dados ficam intactos e qualquer
  instalador anterior reinstala a versão dele).
- Instalação é **por usuário**; instalação de sistema (`/usr/bin`, `Program Files`, serviço de sistema)
  continua pelo `.deb`/tar.gz manual.
- Remover o Login Item no macOS usa `osascript` em System Events e pode disparar o pedido de
  permissão de Automação uma vez; falha é ignorada.
