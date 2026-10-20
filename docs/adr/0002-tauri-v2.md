# ADR-0002 — Tauri v2 para bandeja e UI de configurações

**Status:** Aceita · **Data:** 2026-10-19

## Contexto
Precisamos de ícone de bandeja nos três SOs e uma janela de configurações rica (menus laterais, formulários, listas com streaming). Alternativas: egui/eframe, iced, tray + UI no navegador.

## Decisão
Tauri v2: core Rust, tray nativo, janela com WebView (React + TypeScript + Tailwind), plugins oficiais para autostart, single-instance, global-shortcut, notification, dialog.

## Consequências
- (+) UI de formulários produtiva e bonita; binário pequeno; plugins resolvem integrações de SO.
- (+) Toda lógica fica em crates Rust puros; o app Tauri é só casca (IPC + tray).
- (−) Dependência de WebView do sistema (WebView2 no Windows, webkit2gtk no Linux); dois toolchains (Rust + Node para o frontend).
- (−) Atalho global em Wayland é limitado (problema de plataforma, não do Tauri).
