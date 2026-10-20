# ADRs — Registros de Decisão Arquitetural

Formato: contexto → decisão → consequências. Numeração sequencial; nunca editar uma ADR aceita, criar outra que a substitui.

| # | Título | Status |
|---|---|---|
| [0001](0001-rust.md) | Rust como linguagem do serviço | Aceita |
| [0002](0002-tauri-v2.md) | Tauri v2 para bandeja e UI de configurações | Substituída pela 0016 |
| [0003](0003-claude-code-headless.md) | Claude Code CLI headless como motor, processo vivo por thread | Aceita |
| [0004](0004-mcp-streamable-http.md) | Servidor MCP via Streamable HTTP em localhost | Aceita |
| [0005](0005-sqlite-keyring.md) | SQLite para estado e keyring para segredos | Aceita |
| [0006](0006-realtime-websocket.md) | OpenAI Realtime via WebSocket | Aceita |
| [0007](0007-token-ativo-unico.md) | Vários tokens Claude, um ativo por vez, troca sem downtime | Aceita |
| [0008](0008-telegram-teloxide.md) | Telegram com teloxide (sem dispatcher), modelo do projeto de referência | Aceita |
| [0009](0009-kb-sem-git.md) | Base de conhecimento não versionada pelo serviço | Aceita |
| [0010](0010-user-session-app.md) | App de sessão do usuário com autostart, não daemon de sistema | Aceita |
| [0011](0011-whatsapp-rust-tools-only.md) | WhatsApp via `whatsapp-rust`, exposto só como ferramentas MCP | Aceita |
| [0012](0012-google-oauth-rest.md) | Google (Agenda e Gmail) com OAuth Client do usuário e REST direto | Aceita |
| [0013](0013-politicas-de-aprovacao-mcp.md) | Políticas de aprovação para ferramentas MCP com efeito externo | Aceita |
| [0014](0014-mcp-json-gerado.md) | `.mcp.json` da KB gerado como união de MCPs internos e externos | Aceita |
| [0015](0015-ui-http-local-tauri-casca.md) | UI servida por HTTP local; Tauri como casca fina | Substituída pela 0016 |
| [0016](0016-ui-nativa-egui.md) | UI nativa com egui/eframe; binário único em modo daemon (`will-ia`, `--show`, `serve`) | Aceita |
