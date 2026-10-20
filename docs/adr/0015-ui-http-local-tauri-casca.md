# ADR-0015 — UI servida por HTTP local; Tauri como casca fina

**Status:** Aceita · **Data:** 2026-10-19 · Complementa a ADR-0002

## Contexto
A ADR-0002 previa a UI em WebView do Tauri com comandos IPC. Na implementação, a máquina de desenvolvimento não tem as bibliotecas de sistema do Tauri (webkit2gtk-dev, gtk3-dev) nem `sudo`, e o áudio nativo (cpal) exigiria ALSA. Também é desejável testar o serviço em máquinas sem GUI e no navegador.

## Decisão
O crate `will-server` sobe um servidor HTTP em `127.0.0.1:<porta>` que serve a UI React embutida (rust-embed de `ui/dist`) e a API/WS (`docs/22`), protegidos por um token de sessão local. A **mesma UI** roda no navegador ou dentro da janela do Tauri. O app Tauri (`apps/will-desktop`) é uma casca: inicia o `will-server` no processo, cria a bandeja, abre a janela na URL local, registra atalho global e autostart. O áudio da voz é capturado/reproduzido pela própria UI (Web Audio) e relé via WebSocket; `cpal` não é usado.

## Consequências
- (+) Testável em qualquer máquina com `cargo run -p will-server -- serve --open`; sem dependências de GUI para o núcleo.
- (+) Uma única UI e um único contrato (REST/WS); Tauri acrescenta só integrações de desktop.
- (−) A casca Tauri ainda **não foi compilada** no ambiente de desenvolvimento (falta de libs); o código segue a API do Tauri v2 e precisa ser validado numa máquina com os pré-requisitos.
- (−) Atalho global sem janela aberta depende do Tauri (ou de um futuro daemon de hotkey); no navegador a voz é acionada pela página.
