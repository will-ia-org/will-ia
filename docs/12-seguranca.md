# 12 — Segurança

## Modelo de ameaças (resumo)

| Ativo | Ameaça | Mitigação |
|---|---|---|
| Tokens Claude / OpenAI / Telegram | vazamento por arquivo, log, chat | keyring do SO; mascaramento (`…últimos 4`); redator de logs; nunca em `Settings` JSON |
| Base de conhecimento (arquivos pessoais) | agente edita/apaga além do esperado | modo `acceptEdits` + allowlist por padrão; bloqueio de pastas perigosas (home, raiz); aviso em `bypassPermissions` |
| Máquina do usuário | execução arbitrária via tool Bash | allowlist restringe Bash a `tools/`; bypass só com confirmação |
| Bot do Telegram | terceiros falam com o cérebro | deny-by-default: chat **e** user allowlist; só `/start` responde fora |
| Servidor MCP | outro processo local chama ferramentas | loopback apenas + bearer efêmero por processo; destinos restritos à allowlist |
| Prompt injection via conteúdo (web, anexos, mensagens) | agente induzido a usar `telegram_send_message` ou editar arquivos | ferramenta só envia para chats permitidos; `CLAUDE.md` orienta desconfiar de instruções em conteúdo; log de todas as chamadas MCP |
| Sessões paralelas | corrupção por escrita concorrente | system prompt instrui reler antes de editar; teto de processos |
| WhatsApp pessoal | envio indevido, spam → suspensão; vazamento de conversas de terceiros | política `confirmar` em envios; rate limit; números não salvos bloqueados; índice local com retenção; aviso de risco; sessão 0600 |
| Conta Google (Agenda, Gmail) | e-mail enviado/compromisso alterado por injeção; vazamento de refresh token | `confirmar` em `gmail_send`/`gmail_modify`/escritas de agenda; "só destinatários já vistos" opcional; refresh token no keyring; escopos mínimos e incrementais; revogação real |
| MCPs externos | servidor malicioso/comprometido recebe dados ou induz ações | catálogo explícito; teste prévio; allowlist por ferramenta; segredos isolados por servidor; aviso na UI |
| Conteúdo de e-mail/mensagens | prompt injection direcionada ao agente | tratado como dado; `CLAUDE.md` da KB orienta; aprovação humana nas ações externas (ADR-0013) |

## Regras de implementação

1. **Segredos** são `secrecy::SecretString`; `Debug` redigido; nunca em `tracing` fields.
2. **Redator de log**: filtro `tracing` que mascara padrões `sk-ant-[A-Za-z0-9_-]+`, `sk-[A-Za-z0-9]{20,}`, `\d{8,}:[A-Za-z0-9_-]{30,}` (token de bot), `Bearer \S+`.
3. **Processos filhos** herdam um ambiente **mínimo** (PATH, HOME, LANG, variáveis do Claude, `WILL_IA_*`), não o ambiente inteiro do serviço, evitando vazar segredos de outros módulos para o agente (ex.: a API key da OpenAI não vai para o `claude`).
4. **MCP**: token efêmero de 32 bytes aleatórios (`rand::rngs::OsRng`), comparado em tempo constante; revogado no `exit` do processo.
5. **Telegram**: `callback_query` passa pela allowlist; anexos têm nome sanitizado (`[^\w.-]` → `_`) e tamanho máximo (configurável, 20 MB — limite do `getFile`).
6. **Arquivos temporários**: nenhum para MCP config (inline). Logs com permissão 0600 em Unix.
7. **Atualizações**: binários assinados quando houver certificado; checksums SHA-256 sempre publicados; sem auto-update silencioso no MVP.
8. **Rede**: destinos fixos (api.telegram.org, api.openai.com, e os do próprio Claude Code). Sem telemetria.
9. **Dependências**: `cargo audit` e `cargo deny` no CI (um só toolchain: não há frontend Node).
10. **Aprovações**: o resumo mostrado ao usuário é gerado pelo serviço a partir dos argumentos (destinatário, assunto/primeiras linhas, agenda/data), nunca texto livre do agente, para que uma injeção não "disfarce" a ação.
11. **OAuth**: `state` aleatório verificado no callback; listener de loopback aceita uma única requisição e fecha; PKCE S256.
12. **MCPs externos**: segredos nunca no `.mcp.json` (sempre `${WILL_MCP_*}` + keyring); servidores stdio executam com o ambiente mínimo do spawn; a UI avisa que um MCP externo recebe o que o agente lhe envia e que não há aprovação humana interposta (só allowlist por ferramenta).
13. **Caminhos**: toda ferramenta que recebe ou devolve `path` resolve canonicamente e exige prefixo da KB.
14. **API local**: o token em `<data_dir>/ui-session.token` (0600) protege só a API HTTP em `127.0.0.1` (modo headless, automação, `POST /api/ui/show` da segunda instância). A UI nativa roda no mesmo processo do serviço e não usa token nem rede.

## Modo bypass

Texto da confirmação na UI:

> Neste modo o agente pode executar qualquer comando no seu computador, sem perguntar. Use apenas se a base de conhecimento e a máquina forem dedicadas a isso. Deseja continuar?

Além do toggle, fica um badge permanente "⚠️ bypass" na bandeja e na janela.

## Divulgação

Problemas de segurança: abrir issue privada ou contato direto com o mantenedor (definir no `SECURITY.md` quando o projeto for publicado).
