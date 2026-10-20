# ADR-0016 — UI nativa com egui/eframe; binário único em modo daemon

**Status:** Aceita · **Data:** 2026-10-20 · **Substitui** a ADR-0002 (Tauri) e a ADR-0015 (UI web servida por HTTP)

## Contexto

O will-ia é uma ferramenta **local**: roda o tempo todo na sessão do usuário (servidor MCP para os processos `claude`, Telegram, voz), com ícone na bandeja. O usuário pediu explicitamente **um app desktop nativo**, não uma página no navegador nem um WebView: a tela de configuração, a gestão de threads, o QR do WhatsApp e o indicador de voz devem ser janelas do próprio aplicativo, a voz deve falar/ouvir pelo áudio do app, e o pacote distribuído deve ser um binário por SO (Windows, macOS Intel + ARM, Linux).

Alternativas avaliadas:

| Opção | Nativo? | Deps de sistema (Linux) | Toolchain | Veredito |
|---|---|---|---|---|
| Tauri v2 + React | WebView | webkit2gtk, gtk3, appindicator (dev libs para compilar) | Rust + Node | Rejeitado: WebView, dois toolchains, libs de sistema |
| **egui/eframe** | janela nativa (winit) + render GL/wgpu | nenhuma em compile-time (X11/Wayland carregados em runtime) | Rust | **Escolhido** |
| Slint | janela nativa + DSL `.slint` | nenhuma | Rust + DSL | Bom, mas licença (GPL/royalty-free) e DSL extra |
| iced | janela nativa (wgpu) | nenhuma | Rust | Compila mais devagar; menos widgets prontos para formulários densos |
| fltk-rs / libui | widgets do SO | X11 dev headers, cmake | Rust + C | Visual datado; build frágil |

## Decisão

1. **GUI em `egui`/`eframe`** no crate `crates/will-desktop` (binário `will-ia`). Modo imediato: a tela lê um *snapshot* do estado (`Arc<AppState>` em processo) e dispara ações assíncronas no runtime tokio; respostas voltam por canais e um `request_repaint`. Sem lógica de negócio na UI.
2. **Binário único, três modos**:
   - `will-ia` (sem argumentos): sobe o serviço **em processo** (storage, módulos, MCP, API local) + ícone na bandeja + janela principal (oculta se o onboarding já foi feito; `--show` força). Fechar a janela só a esconde; "Sair" fica na bandeja.
   - `will-ia` executado de novo com o serviço no ar: pede à instância ativa para mostrar a janela (`POST /api/ui/show`, token local) e sai.
   - `will-ia serve` (headless): só serviço + API local, para servidores, testes (`scripts/smoke.sh`) e systemd sem sessão gráfica.
3. **Áudio no app**: captura e reprodução com `cpal` (PCM16 mono 24 kHz, reamostragem linear); o `RealtimeManager` recebe `audio_in` e publica `AudioOut` diretamente, sem WebSocket. O endpoint WS `/api/voice/audio` continua existindo para o modo headless/clientes externos.
4. **Indicador flutuante de voz**: *viewport* secundário do egui (sem decoração, sempre no topo, transparente) mostrando estado (`ouvindo`, `falando`, `pensando`) e nível do microfone; aparece quando a sessão de voz está ativa.
5. **Bandeja**: `tray-icon` no Windows/macOS; no Linux, `ksni` (StatusNotifierItem via D-Bus, Rust puro). Menu: Abrir, Voz (alternar), Sair.
6. **Atalho global** (`global-hotkey`) para alternar/PTT de voz onde o SO permite (Windows, macOS, X11; Wayland não).
7. **Diálogo de pasta** com `rfd` (portal XDG no Linux; nativo nos demais).
8. A **API HTTP local** (`docs/22`) deixa de servir UI e passa a ser contrato interno/automação: usada pelo modo headless, pelo smoke test e pela segunda instância. A UI React (`ui/`) e a casca Tauri (`apps/will-desktop`) são **removidas**.

## Consequências

- (+) Um só toolchain (Rust), um só binário por SO, sem WebView nem Node; compila em máquina sem libs de GUI (Linux precisa apenas de `libasound2-dev` para o `cpal` em tempo de build).
- (+) Voz, bandeja e atalho vivem no mesmo processo que o serviço: sem relé de áudio, sem token de sessão para a UI.
- (+) Indicador flutuante e QR do WhatsApp são janelas nativas.
- (−) Visual do egui é consistente entre SOs, não "idêntico ao SO". Aceito: é uma ferramenta de produtividade.
- (−) egui redesenha quando há eventos; páginas com streaming usam `request_repaint` dirigido por eventos do barramento, não polling.
- (−) Bandeja no Linux depende de suporte a StatusNotifierItem no desktop (GNOME precisa da extensão AppIndicator; KDE, Cinnamon, XFCE suportam).
- Distribuição: `docs/13` passa a descrever `cargo-dist`/GitHub Actions com matriz Linux x64, Windows x64, macOS x64 e arm64; o Linux pode rodar como *user unit* do systemd (`packaging/systemd/will-ia.service`), que tem acesso à sessão gráfica e ao áudio.
