# ADR-0018 — Navegação pelo agent-browser via skill + env; proxy CDP local com lançamento do navegador sob demanda

**Status:** Aceita · **Data:** 2026-10-28

## Contexto

O usuário quer que o agente navegue na web de verdade (abrir páginas, preencher formulários, ler conteúdo autenticado, tirar screenshots), não só `WebFetch`. O Claude Code já tem um braço maduro para isso: o CLI **agent-browser** (vercel-labs), em Rust, que fala o Chrome DevTools Protocol com um navegador real, e vem com uma skill (`SKILL.md`) que ensina o agente a usá-lo. Ele aceita `--cdp <porta|url>` ou `AGENT_BROWSER_CDP`, descobre o WebSocket em `GET /json/version` e conecta.

O que não resolve sozinho:

- **Ninguém escutando**: o agent-browser precisa de um Chrome com `--remote-debugging-port` já aberto (ou lança o próprio Chromium headless, sem o perfil/estado que o usuário quer). Se a porta não responde, o comando do agente falha e ele tenta "consertar" (abrir navegador por conta própria, instalar coisas), o que é imprevisível.
- **Chrome 136+** ignora a depuração remota no perfil padrão: é preciso `--user-data-dir` dedicado.
- **Descoberta**: o Chrome só aceita `Host` IP/`localhost` e devolve URLs com a própria porta; qualquer intermediário precisa reescrevê-las.
- **Visibilidade**: o usuário quer ver o que o agente faz no navegador (log de comandos CDP) e instalar/atualizar o CLI pela UI, sem terminal.
- Regras do projeto: nada de canal novo (regra 10), loopback apenas (regra 4), conteúdo de página é dado não confiável (regra 11), filhos morrem com o pai (regra 8, serviço residente).

Alternativas avaliadas:

| Opção | Veredito |
|---|---|
| Ferramentas MCP `browser_*` próprias (`browser_open`, `browser_click`…, implementando CDP no will-ia) | Rejeitado: reimplementa o que o agent-browser já faz (snapshots acessíveis, refs, formulários, sessões), ficaria sempre atrás; o agente já conhece a skill do CLI |
| agent-browser via skill + `AGENT_BROWSER_CDP` + `Bash(agent-browser:*)` | **Escolhido**: zero configuração do lado do agente; o serviço só garante a porta |
| Deixar o agente lidar com a porta (sem proxy; só documentar `--cdp 9222`) | Rejeitado: primeira chamada falha sem navegador aberto; agente abre navegador no perfil errado; nenhuma observabilidade |
| Lançar o Chrome na partida do serviço e manter sempre aberto | Rejeitado: navegador aberto o tempo todo sem uso; headless/visível mudaria o comportamento na partida; não resolve a reescrita de descoberta |
| **Proxy em loopback com fallback de lançamento** (escuta em 9223, repassa ao 9222, lança se preciso) | **Escolhido** |

## Decisão

1. **`will-browser` é um módulo de ferramentas + UI**, não um canal nem um `ToolProvider`: não cria threads e não expõe ferramentas MCP. O agente usa o CLI `agent-browser` por `Bash`, guiado pela skill.
2. **Env + allowlist no spawn do `claude`** (`engine_cfg`): `AGENT_BROWSER_CDP=http://127.0.0.1:<listen_port>` sempre que o módulo está habilitado, e `Bash(agent-browser:*)` na allowlist de `acceptEdits` quando `allow_agent_browser_tool` está ligado. O agente nunca passa `--cdp`.
3. **Proxy CDP local** (axum + tokio-tungstenite) em `127.0.0.1:<listen_port>` (padrão 9223) → `upstream_host:upstream_port` (padrão `127.0.0.1:9222`): repassa HTTP com `Host` em loopback e reescreve `host:porta` nas respostas `/json*`; repassa WebSocket nos dois sentidos e registra as mensagens de texto num anel em memória (`traffic_capacity`, padrão 500) para a página Browser CDP.
4. **Fallback de lançamento** antes de qualquer repasse: se o upstream não responde e `auto_launch` está ligado (e o upstream é loopback), lança o navegador com `--remote-debugging-port`, `--user-data-dir=<data_dir>/browser-profile` (dedicado), `--no-first-run`, `--no-default-browser-check`, `--remote-allow-origins=*`, `--headless=new` opcional, `extra_args` e `about:blank`; espera até `launch_wait_secs`; um trinco garante um único lançamento sob concorrência. Erros voltam como `502` com mensagem em pt-BR que o agente pode repassar.
5. **Detecção por SO** (Chrome, Chromium, Brave, Edge e o Chromium do `agent-browser doctor --json`) com `browser_path` manual opcional; `node`/`npm`/`npx`/`agent-browser` procurados também em nvm/volta/fnm/npm global.
6. **Instalação pela UI** como trabalhos em segundo plano com saída acompanhada: `npm install -g agent-browser@latest`, `agent-browser install`, e a skill copiada de `agent-browser skills path` para `<kb>/.claude/skills/agent-browser/SKILL.md` (fallback `npx -y skills add vercel-labs/agent-browser -a claude-code -y` na KB). É a única escrita do módulo na KB e só acontece por clique.
7. **Processos lançados seguem o serviço residente**: `hide_console`, `die_with_parent`, `attach_to_job`, grupo de processos próprio; `close_browser` só fecha o que nós lançamos (`close_browser_on_stop` opt-in).
8. **Segurança**: só loopback; o proxy não autentica (mesmo modelo do CDP local); perfil dedicado isola cookies/senhas do perfil pessoal; conteúdo de página é dado (regra 11); o módulo nunca recebe nem injeta segredos.

## Consequências

- (+) O agente navega "de graça": a porta sempre responde, o primeiro comando só demora alguns segundos; nenhuma configuração na KB além da skill.
- (+) Observabilidade: tráfego CDP ao vivo, status do navegador, instalação e testes de ponta a ponta na UI.
- (+) Zero código de automação de navegador no will-ia; o agent-browser evolui sozinho (`npm install -g agent-browser@latest` pela UI).
- (−) Dependência de Node/npm para instalar o CLI (o serviço continua sem Node; só o usuário precisa dele). Mitigação: detecção ampla e mensagens acionáveis.
- (−) Sem aprovação humana sobre o que o agente faz na web (ações passam por `Bash`, não por ferramentas MCP; ADR-0013 não se aplica). Mitigação: `allow_agent_browser_tool` desligável, perfil dedicado sem as sessões pessoais do usuário, orientação na KB.
- (−) Qualquer processo local do usuário pode falar com 9223/9222, como em qualquer uso do CDP; aceito por ser loopback e perfil dedicado.
- (−) Chrome em uso pelo usuário no perfil padrão não é controlável (limitação do Chrome 136+); o agente trabalha no perfil dedicado, onde o usuário pode fazer login nos sites que quiser expor ao agente.
- Se no futuro houver necessidade de aprovação por ação de navegação, isso exige nova ADR (provavelmente ferramentas MCP `browser_*` sobre o mesmo proxy).
