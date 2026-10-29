# ADR-0022 — macOS: servidor como app acessório e ativação do app pelo LaunchServices

- **Data:** 2026-10-29
- **Status:** aceito
- **Relacionados:** ADR-0019 (cliente/servidor), ADR-0021 (instalador), `docs/13`, `docs/27`

## Contexto

Na instalação macOS (`~/Applications/will-ia.app`, servidor `will-ia` e app `will-ia-app` no mesmo
bundle, servidor sob LaunchAgent) o Will via dois problemas:

1. **O servidor aparecia no Dock** ("executando em segundo plano" no clique direito). O `tray-icon`
   cria o `NSApplication` com a política padrão (*Regular*) e o processo está dentro de um bundle com
   `LSUIElement=false`; o `lsappinfo` listava o `will-ia` como `type="Foreground"`. Pior: a thread
   principal do servidor ficava presa no `block_on` do tokio, **sem run loop do AppKit** — e o
   `tray-icon` exige esse run loop para desenhar o `NSStatusItem` e despachar cliques. O ícone que
   "devia estar na bandeja" não funcionava de fato.
2. **O app ficava "pulando" no Dock**, às vezes sem parar. O caminho de "mostrar janela"
   (`UiCommand::Show`, vindo da bandeja/`POST /api/ui/show`/segunda execução) mandava
   `ViewportCommand::Focus`, que no winit vira `activateIgnoringOtherApps:`. Desde o macOS 14 a
   ativação é **cooperativa**: um app que não está na frente não consegue se ativar; o sistema nega e
   sinaliza com o ícone pulando (pedido de atenção), que persiste até o usuário ativar o app pelo
   Dock. Como o servidor lançava o binário direto (`Command::new(will-ia-app)`), nem o lançamento
   passava pelo LaunchServices.

## Decisão

- **Servidor = app acessório.** No macOS, antes de criar o `NSStatusItem`,
  `will_platform::macos::become_accessory()` aplica `NSApplicationActivationPolicy::Accessory`
  (sem Dock, sem Cmd+Tab; só barra de menus). Em seguida a thread principal entra em
  `will_platform::macos::run_main_loop()` (`[NSApp run]`) e a espera do serviço vai para a thread
  `will-wait`, que encerra o processo se o serviço cair. Sem bandeja (`--no-tray`, `serve`) nada muda.
- **Ativar/lançar o app pelo LaunchServices.** `launcher::open_app` no macOS, quando o binário está
  num `.app`: pede a janela por ipc (`will-ia-app show`, processo sem `NSApplication`, sem efeito no
  Dock) e chama `open -a <bundle>` — o LaunchServices ativa a instância que já roda (sem novo
  processo) ou lança o app. Testado: `open -a`/`open -b dev.will.will-ia` ativa o **app** mesmo com o
  servidor registrado sob o mesmo bundle id.
- **O app não se ativa sozinho.** `show_window` no macOS manda `Visible`/`Minimized(false)` e, se não
  está ativo, `will_platform::macos::activate_self()` (`open -a` no próprio bundle); `Focus` só quando
  já está ativo (então é inofensivo) ou fora de um bundle (desenvolvimento).
- `ProcessType` do LaunchAgent continua `Interactive`: o servidor faz áudio em tempo real (voz/TTS) e
  sobe o Chrome; `Background` rebaixaria a prioridade de CPU/IO desses caminhos.

## Consequências

- O Dock mostra só o app, como um aplicativo comum; o servidor vive na barra de menus com o menu
  "Abrir will-ia" funcionando.
- `will_platform` ganha dependência de `objc2` + `objc2-app-kit` (API segura; sem `unsafe` novo).
- Não foi preciso separar o servidor em outro bundle; se um dia a ambiguidade de bundle id causar
  problema, a saída é um helper bundle `LSUIElement` dentro de `Contents/Library/`.
- Notificações nativas (`notify-rust`) não lançam nem ativam o app; seguem como estão.
