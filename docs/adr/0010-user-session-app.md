# ADR-0010 — App de sessão do usuário com autostart, não daemon de sistema

**Status:** Aceita · **Data:** 2026-10-19

## Contexto
"Rodar como serviço" pode significar daemon de sistema (systemd/Windows Service/launchd daemon) ou app de bandeja iniciado no login. Bandeja, microfone/alto-falante, atalho global e keyring só existem na sessão gráfica do usuário.

## Decisão
Aplicação de sessão do usuário com autostart (Login Item / chave Run / `.desktop` autostart), instância única, sem janela principal (apenas tray).

## Consequências
- (+) Acesso a áudio, bandeja, keyring e notificações sem gambiarras; a referência já rodava assim (launchd agent com `caffeinate`).
- (−) Não roda sem login gráfico. Para servidores headless, um modo `--headless` (sem tray/voz, só Telegram + motor) fica no backlog.
