# assets/icon — ícone do will-ia

Fonte: `will-ia.svg` (squircle azul com a onda de voz em 5 barras) e `tray-template.svg` (só as barras,
preto com alfa, para a barra de menus do macOS como *template* e para a bandeja do Linux/Windows em tamanhos pequenos).

`./gerar.sh` (macOS: Chrome headless + `sips` + `iconutil` + `python3`) regenera tudo a partir dos SVGs:

| Arquivo | Uso |
|---|---|
| `will-ia-{16,22,32,44,48,64,128,256,512,1024}.png` | janela/Dock/barra de tarefas (eframe), bandeja colorida (Windows/Linux), ícones `hicolor` instalados pelo `will-ia-setup` no Linux, `.deb` |
| `will-ia.icns` | `will-ia.app` e `Instalar will-ia.app` (`CFBundleIconFile`), copiado pelo release e pelo instalador |
| `will-ia.ico` | recurso dos `.exe` do Windows (`build.rs` + `winresource`); atalhos `.lnk` apontam para o `.exe` |
| `tray-template-{22,32,44,64}.png` | barra de menus do macOS (`with_icon_as_template`) |

Os binários embutem os PNGs via `will_platform::icon` (`include_bytes!`), então nada depende de arquivo no disco em runtime.
