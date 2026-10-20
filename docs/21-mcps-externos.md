# 21 — MCPs externos e o `.mcp.json` da base de conhecimento

Dois tipos de servidor MCP chegam aos agentes:

| Tipo | Quem serve | Exemplos |
|---|---|---|
| **Interno** | o próprio will-ia (`will_ia`, ver [09](09-servidor-mcp.md)) | `telegram_*`, `whatsapp_*`, `calendar_*`, `gmail_*`, `notify_user` |
| **Externo** | o servidor do terceiro (processo local via stdio ou endpoint HTTP remoto) | Trello, GitHub, Notion, Linear, Playwright, filesystem, qualquer MCP |

O will-ia **não faz proxy** dos externos: ele apenas os **cataloga** (seção de configurações), **testa** e **escreve a união de todos** no arquivo `.mcp.json` da base de conhecimento, que é o formato de configuração de MCP por projeto que o Claude Code lê.

## Por que `.mcp.json` na KB

- É o mecanismo nativo do Claude Code para MCPs de projeto: funciona para os processos `claude -p` do serviço **e** para quem abrir um `claude` interativo na pasta.
- Fica visível e auditável pelo usuário, ao lado do `CLAUDE.md`.
- Um só lugar para a união: nossos servidores + externos.

Ver [ADR-0014](adr/0014-mcp-json-gerado.md).

## Formato gerado

```json
{
  "mcpServers": {
    "will_ia": {
      "type": "http",
      "url": "http://127.0.0.1:47321/mcp",
      "headers": { "Authorization": "Bearer ${WILL_IA_MCP_TOKEN}" }
    },
    "trello": {
      "type": "http",
      "url": "https://mcp.trello.com/mcp",
      "headers": { "Authorization": "Bearer ${WILL_MCP_TRELLO_TOKEN}" }
    },
    "github": {
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": { "GITHUB_PERSONAL_ACCESS_TOKEN": "${WILL_MCP_GITHUB_PAT}" }
    },
    "filesystem-docs": {
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/will/Documentos"]
    }
  }
}
```

Pontos-chave:

- **Segredos nunca vão para o arquivo.** O Claude Code expande `${VAR}` em `url`, `headers`, `env`, `command` e `args`. O serviço grava referências e **injeta as variáveis no ambiente de cada processo `claude`** a partir do keyring. Quem abrir o arquivo vê só `${WILL_MCP_TRELLO_TOKEN}`.
- **Identidade por thread preservada.** O bearer do `will_ia` é `${WILL_IA_MCP_TOKEN}`, definido por spawn; cada processo tem o seu (ver [09](09-servidor-mcp.md)). Um arquivo compartilhado, tokens distintos.
- Servidores **desabilitados** no painel não entram no arquivo.
- A porta do `will_ia` muda a cada execução se for aleatória; o serviço regrava o arquivo na inicialização e sempre que o catálogo muda. Os processos vivos não são afetados (já carregaram); novos spawns leem a versão nova.

## Como o Claude Code recebe o arquivo

Em modo headless, servidores de `.mcp.json` de projeto podem exigir aprovação do usuário na primeira vez. Para não depender disso, o engine passa o mesmo arquivo explicitamente:

```bash
claude -p … --mcp-config <kb>/.mcp.json [--strict-mcp-config]
```

e, para sessões interativas na pasta funcionarem sem perguntas, escreve em `<kb>/.claude/settings.local.json`:

```json
{ "enableAllProjectMcpServers": true }
```

`--strict-mcp-config` (padrão ligado) ignora MCPs configurados globalmente no `~/.claude.json` do usuário; o toggle "Herdar MCPs globais do Claude Code" desliga isso. Há também um botão **Importar do Claude Code** que lê o `~/.claude.json` (escopo user) e oferece trazer os servidores de lá para o catálogo.

## Seção "MCPs externos" na UI

Lista de servidores com: nome, tipo, status do último teste (✅ n ferramentas / ❌ erro / ⏳), toggle habilitado, ações **Editar**, **Testar**, **Remover**, e botões **Adicionar servidor**, **Importar do Claude Code**, **Colar JSON** (aceita o bloco `mcpServers` que os projetos costumam publicar no README).

Formulário por tipo:

| Campo | stdio | http | sse (legado) |
|---|---|---|---|
| Nome (`[a-zA-Z0-9_-]`, único) | ✓ | ✓ | ✓ |
| `command` | ✓ | | |
| `args[]` | ✓ | | |
| `env{}` | ✓ | | |
| `url` | | ✓ | ✓ |
| `headers{}` | | ✓ | ✓ |
| `cwd` (opcional) | ✓ | | |
| Habilitado | ✓ | ✓ | ✓ |
| Liberar automaticamente as ferramentas (`mcp__<nome>__*` na allowlist do modo `acceptEdits`) | ✓ | ✓ | ✓ |
| Política padrão das ferramentas deste servidor (informativa, ver abaixo) | ✓ | ✓ | ✓ |

Valores de `env`/`headers` marcados como **segredo** são guardados no keyring com `secret_ref = mcp:<nome>:<CHAVE>` e emitidos no arquivo como `${WILL_MCP_<NOME>_<CHAVE>}`. Valores não secretos vão literais.

**Testar**: o serviço instancia um cliente MCP (`rmcp` client) com a mesma definição, expande as variáveis a partir do keyring, faz `initialize` + `tools/list` e mostra nome do servidor, versão e lista de ferramentas. Para stdio, verifica também que `command` existe no PATH (ex.: `npx`, `uvx`, `docker`) e mostra o stderr em caso de falha.

## Políticas de aprovação e MCPs externos

As políticas `auto` / `confirmar` / `desligada` ([ADR-0013](adr/0013-politicas-de-aprovacao-mcp.md)) são aplicadas **dentro do nosso servidor**. Em servidores externos o Claude Code fala direto com eles, então o serviço não consegue interpor aprovação. O controle disponível é o do próprio Claude Code:

- **Modo `acceptEdits`**: ferramentas externas só executam se estiverem em `--allowedTools` (`mcp__<nome>__*` ou `mcp__<nome>__<tool>`), caso contrário são negadas (`--permission-prompts none`). O toggle "Liberar automaticamente" controla isso; o usuário pode liberar só algumas ferramentas por nome.
- **Modo `bypassPermissions`**: tudo liberado.
- Para ter aprovação humana em um externo, o backlog prevê um **modo proxy opcional** em que o `will_ia` reexpõe as ferramentas de um externo sob seu próprio namespace com políticas. Fora do MVP.

## Ordem de precedência e conflitos

- Nome duplicado entre externo e `will_ia` é rejeitado na UI.
- Se o usuário editar `.mcp.json` à mão: na próxima regeneração o serviço detecta divergência (hash do último conteúdo gerado) e, em vez de sobrescrever silenciosamente, **importa** servidores desconhecidos para o catálogo (como habilitados) e avisa na UI. Segredos literais encontrados no arquivo são movidos para o keyring e substituídos por `${…}`.
- Arquivo inexistente ou inválido → regenerado.

## Variáveis injetadas por spawn (resumo)

| Variável | Origem |
|---|---|
| `WILL_IA_MCP_TOKEN` | bearer efêmero da thread |
| `WILL_IA_MCP_URL` | URL atual do `will_ia` |
| `WILL_MCP_<NOME>_<CHAVE>` | segredos dos externos, do keyring |
| demais `WILL_IA_*` | ver [05](05-motor-claude-code.md) |

Servidores stdio são filhos do processo `claude`, não do serviço; morrem com ele graças ao grupo de processos / Job Object.

## Testes

- Geração do arquivo a partir de catálogos (habilitados/desabilitados, segredos → `${}`), idempotência e hash.
- Importação de `.mcp.json` editado à mão e de `~/.claude.json`.
- Teste de conexão com um servidor MCP de exemplo (stdio via `fake-mcp` e http via o próprio `will_ia`).
