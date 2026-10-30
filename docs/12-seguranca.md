# 12 — Segurança

## Modelo de ameaças (resumo)

| Ativo | Ameaça | Mitigação |
|---|---|---|
| Tokens Claude / OpenAI / Telegram | vazamento por arquivo, log, chat | keyring do SO (macOS: cofre cifrado `secrets.enc` + chave-mestra em `secrets.key` 0600, ADR-0024 + adendo — proteção = usuário do SO + FileVault, sem Keychain); mascaramento (`…últimos 4`); redator de logs; nunca em `Settings` JSON |
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
| API do servidor (app remoto, scripts) | quem alcança a porta fala com o agente, lê a KB e muda configurações; força bruta de senha; sessão roubada; escuta na rede (token/senha em claro); servidor falso (MITM) | contas com senha (argon2id) e papéis `admin`/`membro`; token de sessão aleatório guardado só por sha256, 30 dias; rate limit de login por IP; token local da API só para quem tem o disco; rotas de configuração exigem admin; **loopback por padrão**; fora do loopback só com TLS (autoassinado fixado no app ou certificado próprio) ou opt-in explícito atrás de VPN |
| Navegador do agente (Browser CDP) | outro processo local controla o navegador pela porta CDP; páginas injetam instruções; agente age na web em nome do usuário | proxy e navegador só em loopback; perfil **dedicado** separado do pessoal; conteúdo de página é dado (regra 11); `Bash(agent-browser:*)` desligável; só o navegador lançado pelo serviço é fechado por ele |

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
14. **API do servidor e contas** (`docs/22` "Autenticação", `docs/26` etapa 4):
    - **Senhas** com Argon2id (parâmetros padrão da crate `argon2`, sal aleatório, PHC string em `users.password_hash`); nunca comparadas em texto; mínimo 8 caracteres (`check_password_strength`). O hash nunca sai pela API (`UserRecord` o omite na serialização).
    - **Sessões**: token de 32 bytes aleatórios (hex) entregue uma única vez no login; o banco guarda só `sha256(token)`, então vazar o SQLite não entrega sessões válidas. Validade fixa de 30 dias (sem renovação, para revogação previsível); `logout` apaga a linha; desativar o usuário ou redefinir sua senha apaga todas as suas sessões; expiradas são apagadas no uso e na subida.
    - **Rate limit de login**: `LoginLimiter` em memória, 10 tentativas por IP em 15 minutos → 429; zera no login bem-sucedido. Mensagem de erro única ("usuário ou senha inválidos") para não revelar se o usuário existe.
    - **Token local da API** (`<data_dir>/ui-session.token`, 0600): continua sendo o bearer de quem tem acesso ao disco do servidor (app em loopback, `smoke.sh`, automações) e vale como **admin implícito** (`username: local`). Só circula em `127.0.0.1`; nunca deve ser colado num perfil remoto do app (para isso existem contas). Será restrito a loopback explicitamente na etapa 5 (`bind`).
    - **Senha inicial do admin**: gerada aleatoriamente (16 caracteres, alfabeto sem ambíguos) na primeira subida, gravada em `<data_dir>/admin-initial-password.txt` com 0600 e exibida por `will-ia info`; o log registra só o caminho. O arquivo é removido na primeira troca de senha, que é obrigatória (`must_change_password`). Nunca embutir senha padrão.
    - **Papéis**: `admin_gate` nega (403) a membros as rotas de configuração (settings, segredos, tokens do motor, token de scripts, políticas, MCPs externos, usuários, scaffold da KB, instalação do Browser, conexão Google, pareamento/desvinculação do WhatsApp). Um admin não consegue desativar nem rebaixar a própria conta. Usuários são **desativados**, nunca apagados, para a autoria histórica continuar resolvível.
    - **Autoria**: `turns.author` e `approvals.decided_via = "ui:<username>"` registram quem fez o quê; são rótulos de auditoria, não controle de acesso (threads são compartilhadas por decisão, ADR-0019).
    - Perfis do app guardam o token de sessão **em texto** em `<config>/will-ia-app/profiles.json` (0600 no Unix); quem tem a conta do SO do usuário tem a sessão. Keyring do app fica para depois; mitigação hoje: `logout` na página Conexão e revogação pelo admin (`PATCH /api/users/{id}` com `password` ou `active:false`).
15. **Proxy CDP** (`docs/25`, ADR-0018): escuta só em `127.0.0.1:<listen_port>` e lança o navegador só se o upstream também é loopback. O proxy **não autentica**, exatamente como o CDP do Chrome em `--remote-debugging-port`: qualquer processo do mesmo usuário pode controlar aquele navegador. Por isso o navegador lançado usa um **perfil dedicado** (`<data_dir>/browser-profile`), nunca o perfil pessoal (cookies, senhas), e `--remote-allow-origins=*` só vale para ele. O proxy não interpreta comandos; o log de tráfego fica em memória (prévias de 400 caracteres) e é dado não confiável. Nenhum segredo do serviço vai para o navegador nem para os trabalhos de instalação (`npm`/`npx`/`agent-browser`).
16. **Rede** (`docs/26` etapa 5, `settings.network`, `will-ia --bind`):
    - **Loopback é o padrão** (`bind = 127.0.0.1`). O servidor só escuta fora do loopback se `tls = true` **ou** `allow_remote_without_tls = true`; caso contrário **volta sozinho para 127.0.0.1** e registra erro em `/api/logs` (módulo `network`). Nunca se expõe por acidente.
    - **TLS embutido** (rustls via `axum-server`): certificado próprio (`cert_path`/`key_path`, ex.: emitido pela CA interna da equipe) ou **autoassinado** gerado na primeira subida com TLS (`rcgen`, SANs `localhost` + hostname + IP do `bind`, `<data_dir>/tls/cert.pem` e `key.pem` 0600). Se o TLS pedido falha (arquivo ausente/ilegível), o servidor **serve HTTP** e registra erro: no `bind` remoto isso significa ficar em claro, então o admin deve conferir `GET /api/health` (`scheme`) depois de ligar TLS.
    - **Fixação no app**: o certificado autoassinado não é confiável pelo SO; o admin copia o PEM (`GET /api/network/cert`, Configurações → Rede) e cola no perfil do app (`ca_pem`), que o usa como única raiz confiável (`reqwest::add_root_certificate` e o mesmo `ClientConfig` no WebSocket). A impressão digital SHA-256 do DER aparece em `/api/health` para conferência visual antes de entrar. Trocar o certificado = atualizar o PEM nos perfis.
    - **`insecure` (aceitar qualquer certificado) é só para testes**: desliga a verificação no HTTP e no WS; o app avisa em vermelho na Conexão. Nunca em produção.
    - **`allow_remote_without_tls`** só faz sentido atrás de túnel/VPN que já cifra (Tailscale, WireGuard) ou de um proxy reverso que termina TLS em `127.0.0.1`; em qualquer outra rede o token de sessão e a senha passam em claro.
    - **Token local da API em rede exposta**: continua aceito de qualquer IP que alcance a porta e vale como admin. Recomendação: em servidor exposto, use contas (membros para a equipe, admin só para quem administra), mantenha `ui-session.token` 0600, e não copie esse token para perfis remotos. Restringir o token local a conexões vindas de `127.0.0.1` é melhoria futura.
    - **Sem CORS**: a API não serve navegadores; sem `Access-Control-*` um site malicioso não consegue ler respostas (e sem cookies não há CSRF; o bearer vem do app). Limite de tentativas no login continua por IP; atrás de proxy reverso configure-o para limitar também.

## Modo bypass

Texto da confirmação na UI:

> Neste modo o agente pode executar qualquer comando no seu computador, sem perguntar. Use apenas se a base de conhecimento e a máquina forem dedicadas a isso. Deseja continuar?

Além do toggle, fica um badge permanente "⚠️ bypass" na bandeja e na janela.

## Divulgação

Problemas de segurança: abrir issue privada ou contato direto com o mantenedor (definir no `SECURITY.md` quando o projeto for publicado).
