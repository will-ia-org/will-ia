# Diagnóstico — módulo `will-browser` (proxy CDP) não anexa ao Chrome da máquina do Will

**Data:** 2026-10-28 · **Máquina:** macOS (Darwin 27), Chrome 156.0.8078.48, will-ia (LaunchAgent `com.will.will-ia`)
**Sintoma reportado pelo agente (`agent-browser`):**
```
✗ All CDP discovery methods failed for 127.0.0.1:9223:
  /json/version: Invalid /json/version response: expected value at line 1 column 1;
  /json/list:    Invalid /json/list response: expected value at line 1 column 1;
  WebSocket:     WebSocket connect failed at ws://127.0.0.1:9223/devtools/browser: HTTP error: 502 Bad Gateway
```

## 1. Resumo executivo

O proxy CDP do will-ia (`crates/will-browser`, escuta em `127.0.0.1:9223`, upstream `localhost:9222`) **só reconhece um navegador se `GET /json/version` no upstream devolver HTTP 2xx com JSON**. Nesta máquina a porta 9222 (IPv4) é ocupada por um **Chrome do perfil principal do usuário com depuração remota ligada por outro app (Google Antigravity)**, e esse Chrome **responde 404 em todo `/json/*`** porque foi aberto **sem `--remote-allow-origins`**. O WebSocket CDP desse mesmo Chrome **funciona normalmente** (é como o Playwright MCP operava aqui, lendo o `DevToolsActivePort`). Resultado: o probe falha, o proxy declara "nenhum navegador", e:

1. com `auto_launch` ligado, lançou um Chrome dedicado na **mesma porta 9222**; como o IPv4 já estava ocupado, esse Chrome subiu em **`[::1]:9222` (IPv6)** — e o probe, que resolve `localhost` → `127.0.0.1` primeiro, continua batendo no Chrome errado;
2. com `auto_launch` desligado (estado atual), o proxy responde **`502 text/plain`** em `/json/version`, que o `agent-browser` tenta parsear como JSON → o erro acima.

Em outras máquinas funciona porque lá a 9222 está livre (o will-ia lança o próprio Chrome, com `--remote-allow-origins=*`, e o `/json/version` responde) ou o Chrome foi aberto manualmente com as flags certas.

## 2. Evidências coletadas

### 2.1 Dois Chromes na 9222, um por família de loopback
```
$ lsof -iTCP:9222 -iTCP:9223 -sTCP:LISTEN -n -P
Google   52660  127.0.0.1:9222   ← Chrome real (perfil principal), aberto pelo launchd SEM flags de linha de comando
Google    7683  [::1]:9222       ← Chrome lançado pelo will-ia (perfil dedicado)
will-ia   7024  127.0.0.1:9223   ← proxy

$ ps -o command= -p 7683 | tr ' ' '\n' | grep '^--'
--remote-debugging-port=9222
--user-data-dir=/Users/will/Library/Application Support/dev.will.will-ia/browser-profile
--no-first-run
--no-default-browser-check
--remote-allow-origins=*
```

### 2.2 O Chrome real responde 404 no HTTP de descoberta, mas tem WebSocket válido
```
$ curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:9222/json/version
404
$ curl -s -o /dev/null -w '%{http_code}\n' -H 'Origin: http://x' http://127.0.0.1:9222/json/version
404

$ cat "$HOME/Library/Application Support/Google/Chrome/DevToolsActivePort"
9222
/devtools/browser/d783d62e-6345-457d-83b5-89663bcb25a7
```
Três processos `playwright-mcp` antigos ainda vivos nesta máquina conectam exatamente assim:
`--cdp-endpoint ws://127.0.0.1:9222/devtools/browser/<token>` — ou seja, o WebSocket desse Chrome aceita clientes CDP
(clientes CDP não enviam `Origin`; o bloqueio do Chrome sem `--remote-allow-origins` atinge só os endpoints HTTP `/json/*`).

### 2.3 O Chrome dedicado do will-ia responde perfeitamente — no IPv6
```
$ curl -s -g 'http://[::1]:9222/json/version'
{ "Browser": "Chrome/156.0.8078.48", "Protocol-Version": "1.3", "webSocketDebuggerUrl": "ws://localhost:9222/devtools/browser/…", … }
```

### 2.4 `localhost` resolve pros dois endereços; o probe usa o primeiro
```
$ python3 -c "import socket; print(sorted({a[4][0] for a in socket.getaddrinfo('localhost', 9222)}))"
['127.0.0.1', '::1']
```

### 2.5 Estado e configuração do módulo
```
GET /api/settings → browser:
  listen_port 9223 · upstream_host "localhost" · upstream_port 9222 · auto_launch false · headless false · user_data_dir null

GET /api/status → browser:
  state "degraded", detail "nenhum navegador respondendo em localhost:9222 e lançamento automático desligado"

$ curl -s -w ' [http %{http_code}]' http://127.0.0.1:9223/json/version
will-ia Browser CDP: nenhum navegador respondendo em localhost:9222 e lançamento automático desligado [http 502]
```
Log do módulo em 2026-10-28 19:30–19:39 mostra 4 conexões `cdp websocket aberto path=/devtools/browser/57b80b80-…` — token diferente do perfil real → naquele momento a 9222 IPv4 estava livre e o will-ia anexou ao Chrome dele. Funcionou. Depois o Chrome real (Antigravity) tomou a porta.

## 3. Causas-raiz (no código, `crates/will-browser/src/lib.rs`)

| # | Onde | Problema |
|---|---|---|
| C1 | `probe_upstream()` | Única forma de detectar navegador = `GET /json/version` com `status().is_success()`. Chrome com depuração ligada mas sem `--remote-allow-origins` (caso do perfil principal, Chrome ≥ 111) devolve **404** em `/json/*` e o módulo conclui que não há navegador, embora o WebSocket CDP esteja operacional. **Não há fallback** por `DevToolsActivePort` nem por handshake WebSocket. |
| C2 | `BrowserSettings::upstream_host = "localhost"` + `launch_browser()` | `localhost` é dual-stack. O probe (reqwest) resolve pra `127.0.0.1` primeiro; o Chrome lançado se liga em `::1` quando o IPv4 está ocupado. O módulo passa a sondar um Chrome (IPv4, 404) e ignorar o que ele mesmo lançou (IPv6, 200). Além disso `launch_browser()` **não verifica se a porta está livre** antes de lançar nem lê o `DevToolsActivePort` do perfil dedicado pra saber onde o Chrome realmente subiu. |
| C3 | `proxy_handler()` → `ensure_upstream()` falha | Responde `502` com corpo **text/plain** ("will-ia Browser CDP: …"). O `agent-browser` chama `/json/version` e `/json/list` esperando JSON → "expected value at line 1 column 1"; a mensagem útil do proxy nunca chega ao usuário/agente. |
| C4 | `rewrite_discovery()` | Substitui `"{upstream_host}:{upstream_port}"` e `"{h}:{port}"` por `127.0.0.1:{listen}`. Se o upstream for `[::1]` ou o Chrome anunciar `ws://localhost:9222/…` enquanto o host configurado é outro, a URL não é reescrita e o cliente tenta ir direto no 9222 (fora do proxy). |
| C5 | Semântica de "anexar" | O proxy abre **uma conexão WebSocket upstream por conexão de cliente** (`pump`). No perfil principal do Chrome, cada conexão nova dispara o diálogo **"Allow remote debugging?"** (consentimento por conexão, Chrome recente). Se um dia o módulo anexar ao Chrome real, isso vira 1 prompt por sessão do `agent-browser`. Documentar/tratar (conexão upstream única multiplexada ou orientação ao usuário). |

## 4. Correções propostas

### P1 — Descoberta resiliente do upstream (resolve C1)
Em `ensure_upstream()`, antes de concluir "nenhum navegador":
1. `GET /json/version` (como hoje).
2. Se 404/erro **mas a porta aceita TCP**: tentar ler `DevToolsActivePort` em perfis conhecidos — o perfil dedicado (`<data_dir>/browser-profile`) e, opcionalmente, o perfil padrão do SO (macOS `~/Library/Application Support/Google/Chrome/`, Linux `~/.config/google-chrome/`, Windows `%LOCALAPPDATA%\Google\Chrome\User Data\`) quando `attach_to_user_profile` estiver ligado. O arquivo traz `porta\n/devtools/browser/<token>`.
3. Validar com um **handshake WebSocket** em `ws://<host>:<porta>/devtools/browser/<token>` e um `Browser.getVersion` (fecha em seguida). Guardar o `webSocketDebuggerUrl` descoberto.
4. Sintetizar as respostas de `/json/version` e `/json/list` **no proxy** a partir de `Browser.getVersion` / `Target.getTargets` quando o upstream HTTP não existir — assim o `agent-browser` (que depende de `/json/version`) funciona mesmo com Chrome "só WebSocket".

### P2 — Endereço e porta determinísticos (resolve C2)
- Padrão `upstream_host = "127.0.0.1"` (não `localhost`); se o usuário quiser IPv6, que escreva `[::1]`.
- Antes de `launch_browser()`: checar se `upstream_port` está livre em **IPv4 e IPv6**. Se ocupada por processo que **não** responde ao probe/handshake → falhar rápido com mensagem acionável ("porta 9222 ocupada por `Google Chrome` PID 52660 que não aceita descoberta CDP; configure outra `upstream_port` ou habilite anexar ao perfil do usuário") em vez de lançar um segundo Chrome.
- Após lançar, ler `<user_data_dir>/DevToolsActivePort` e usar **a porta/token reais** (Chrome pode mudar o bind), em vez de assumir `upstream_port`.
- Opcional: `upstream_port = 0` → escolher porta livre automaticamente e persistir.

### P3 — Erros do proxy em JSON e com status coerente (resolve C3)
- Em `/json/*`, quando `ensure_upstream()` falha: responder `503` com `{"error":"…","hint":"…"}` (JSON) — o `agent-browser` mostra a mensagem em vez de "expected value".
- Em upgrade WebSocket falho: fechar com código 1011 e motivo com a mesma mensagem.
- Expor a mesma mensagem em `GET /api/status` (já existe) e na UI com botão "Lançar navegador agora" / "Trocar porta".

### P4 — Reescrita de descoberta robusta (resolve C4)
Reescrever qualquer `ws://<host>:<upstream_port>/` (host = `localhost`, `127.0.0.1`, `[::1]`, `upstream_host`) para `ws://127.0.0.1:<listen_port>/`, e também `webSocketDebuggerUrl` sintetizado em P1.

### P5 — Modo "anexar ao Chrome do usuário" (resolve C5, opcional)
Flag `attach_user_profile` (padrão off). Quando ligado e o Chrome real estiver com depuração ativa, usar P1 para anexar. Documentar que o Chrome mostrará "Allow remote debugging?" a cada conexão nova; mitigação: o proxy manter **uma única** conexão upstream por navegador e multiplexar sessões (`Target.attachToTarget` com `flatten: true`) — mais trabalho, pode ficar pra depois.

## 5. Como reproduzir (macOS, 5 min)
```bash
# 1) Chrome do perfil principal com depuração ligada mas sem allow-origins (simula o Antigravity):
#    abra o Chrome normal e ligue "Permitir depuração remota" (chrome://inspect/#devices → ou o app que liga isso),
#    ou: /Applications/Google\ Chrome.app/Contents/MacOS/Google\ Chrome --remote-debugging-port=9222   (sem --remote-allow-origins)
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:9222/json/version     # → 404
cat ~/Library/Application\ Support/Google/Chrome/DevToolsActivePort               # → 9222 + /devtools/browser/<token>  (WS funciona)

# 2) will-ia com upstream localhost:9222 e auto_launch on → lança 2º Chrome, que cai em [::1]:9222
lsof -iTCP:9222 -sTCP:LISTEN -n -P                                              # → 127.0.0.1 (real) e [::1] (dedicado)
curl -s -g 'http://[::1]:9222/json/version'                                      # → 200 JSON (ninguém usa)

# 3) agent-browser (AGENT_BROWSER_CDP=http://127.0.0.1:9223) → "All CDP discovery methods failed … 502"
curl -s -w ' [%{http_code}]' http://127.0.0.1:9223/json/version                  # → texto puro, 502
```

## 6. Critérios de aceite
- [ ] Com um Chrome que responde 404 em `/json/*` mas tem `DevToolsActivePort` válido, `GET http://127.0.0.1:9223/json/version` devolve JSON com `webSocketDebuggerUrl` apontando pro proxy, e `agent-browser snapshot` funciona.
- [ ] Com a `upstream_port` ocupada por um processo não-CDP, o módulo **não** lança um segundo Chrome: vai pra `Degraded` com mensagem citando o PID/porta e a ação sugerida.
- [ ] Chrome lançado pelo módulo tem porta/token lidos do `DevToolsActivePort` do perfil dedicado; funciona mesmo que ele suba em `::1`.
- [ ] Toda falha de `/json/*` no proxy responde JSON (`503 {"error": …}`); o `agent-browser` exibe a mensagem do will-ia.
- [ ] Padrão `upstream_host` = `127.0.0.1`; teste unitário cobrindo reescrita de `ws://localhost|127.0.0.1|[::1]:9222/…`.

## 7. Workaround imediato (sem código)
Nas configurações do will-ia: `upstream_host = 127.0.0.1`, `upstream_port = 9224` (porta livre), `auto_launch = true`; encerrar o Chrome dedicado preso em `[::1]:9222` (PID 7683). O módulo lança o Chrome dele na 9224 e o `agent-browser` volta a funcionar (perfil dedicado: logins precisam ser refeitos nele).

## 8. Correção implementada (2026-10-28, mesma data — ADR-0020)

Em `crates/will-browser` (`src/lib.rs` + novo `src/discover.rs`):

| Causa | O que mudou |
|---|---|
| C1 | `probe_upstream` → depois do HTTP, se a porta aceita TCP: lê `DevToolsActivePort` do perfil dedicado e (com `attach_existing`, padrão `true`) dos perfis do usuário (Chrome/Chromium/Brave/Edge por SO), valida com `Browser.getVersion` por uma **conexão de controle única** (reaproveitada; evita repetir "Allow remote debugging?"). Nesse modo o proxy **sintetiza** `/json/version`, `/json/list`, `/json/new`, `/json/activate/<id>`, `/json/close/<id>` via `Target.*`/`Browser.*`. |
| C2 | `localhost` vira candidatos `127.0.0.1` **e** `[::1]` (IPv4 primeiro); a autoridade que respondeu fica em `Upstream.authority` e é usada no repasse HTTP e no `pump` do WebSocket. Porta ocupada por algo não reconhecido → **não lança** outro navegador: `Degraded` com mensagem acionável (trocar `upstream_port` ou abrir com `--remote-allow-origins=*`). |
| C3 | Falhas em `/json/*` respondem `503`/`502` **JSON** `{"error","upstream","hint"}`; WebSocket falho → 503 texto. |
| C4 | `discover::rewrite_authorities` reescreve `ws://{localhost|127.0.0.1|[::1]|host configurado|host que respondeu}:<porta>` → `127.0.0.1:<listen>`. |
| C5 | Documentado: anexar ao perfil do usuário pode exibir o diálogo do Chrome a cada conexão nova do `agent-browser`; a conexão de controle do proxy é única. Multiplexação fica como backlog. |

Status: `upstream_authority`, `upstream_source` (`http` / `DevToolsActivePort (<perfil>)`), `upstream_http_discovery`.
Testes: `tests/proxy.rs` → `attaches_via_devtools_active_port_when_http_is_404`, `busy_port_without_cdp_discovery_does_not_launch`; unitários em `discover.rs`.
