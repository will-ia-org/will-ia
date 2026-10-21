# 10 — Configurações (UI e esquema)

Janela nativa (egui/eframe, crate `will-desktop`) com navegação lateral: páginas Dashboard, Threads, Aprovações, Logs e Configurações (seções abaixo). Cada seção mapeia para um sub-struct de `Settings`. Segredos não ficam em `Settings`: ficam no keyring e `Settings` guarda apenas referências (`secret_ref`).

## Seções e campos

### Geral
| Campo | Tipo | Padrão |
|---|---|---|
| Iniciar com o sistema | bool | true |
| Idioma da UI | `pt-BR` \| `en` | `pt-BR` |
| Nível de log | `error`…`trace` | `info` |
| Notificações nativas | bool | true |
| Tema | `system` \| `light` \| `dark` | `system` |

### Motor Claude
| Campo | Tipo | Padrão |
|---|---|---|
| Caminho do binário `claude` | path (auto-detectado) | `claude` no PATH |
| Tokens de subscription | lista `{ id, rótulo, secret_ref, ativo, status, verificado_em, erro }` | — |
| Modelo | string opcional (alias ou nome) | vazio (padrão do CLI) |
| Modo de permissão | `acceptEdits` \| `bypassPermissions` | `acceptEdits` |
| Ferramentas permitidas (acceptEdits) | lista de strings | allowlist padrão do doc 05 |
| Processos vivos (máx.) | int 1–20 | 5 |
| Timeout de ociosidade | minutos | 10 |
| Orçamento máx. por turno (USD) | float opcional | vazio |
| Effort | `low`…`max` opcional | vazio |
| Incluir mensagens parciais | bool | false |
| Gravar stream bruto em log | bool | false |
| Texto adicional de system prompt | textarea | modelo padrão |

Ações: **Adicionar token** (abre instrução `claude setup-token`), **Testar**, **Tornar ativo** (testa → aplica → derruba processos vivos → mostra quantas threads voltarão com `--resume`), **Remover**.

### Base de Conhecimento
| Campo | Tipo |
|---|---|
| Pasta | path (diálogo nativo de pasta, `rfd`) |
| Status | existe / gravável / tem `CLAUDE.md` / nuvem sincronizada / `node` e `python` detectados |
| Ações | Abrir no explorador · Criar estrutura inicial · Recarregar comandos (`.claude/commands`) |

### Voz (Realtime)
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | false |
| OpenAI API key | secret | — |
| Modelo Realtime | string | `gpt-realtime-mini` |
| Voz | enum | `alloy` |
| Modo de ativação | `toggle` \| `push_to_talk` | `toggle` |
| Atalho global | string em formato próprio (modificadores + tecla, gravador de tecla) | `Ctrl+Shift+Space` (`Cmd` no macOS) |
| Detecção de turno | `server_vad` \| `semantic_vad` | `server_vad` |
| Dispositivo de entrada / saída | enum (dispositivos enumerados pelo `cpal`) | padrão do sistema |
| Instruções da persona | textarea | modelo padrão |
| Transcrever e salvar na thread | bool | true |
| Limite de caracteres falados do cérebro | int | 1500 |
| Limite de minutos/dia | int opcional | vazio |

Painel de sessão na mesma seção: **Iniciar/Parar**, botão **segurar para falar** (PTT), estado (ouvindo / pensando / falando), nível do microfone e transcrições recentes. O indicador flutuante (`docs/07`) acompanha a sessão.

### Telegram
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | false |
| Token do bot | secret | — |
| Chats permitidos | lista de ids (int64) | — |
| Usuários permitidos | lista de ids (int64) | — |
| Destino padrão (chat/tópico) | par opcional | primeiro chat |
| Anunciar no boot | bool | true |
| Não autorizados | `ignorar` \| `responder_aviso` | `ignorar` |
| Formato da resposta final | `texto` \| `html` | `texto` |
| Resumo em áudio | bool + mín. chars + voz + velocidade | false / 120 / `alloy` / 1.5 |
| Transcrever voice notes | bool | true (se houver OpenAI key) |
| Pasta de anexos | path relativa à KB | `inbox/telegram` |

Ações: **Testar bot** (`getMe`), **Descobrir IDs** (instrução `/start`).

### MCP
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | true |
| Porta | int (0 = aleatória) | 0 |
| Ferramentas | toggles por ferramenta | `telegram_send_message` on, `notify_user` on, demais off |

### WhatsApp
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | false |
| Contas (`accounts`) | lista `{ id, label, phone, enabled, created_at }` mantida pelo serviço; cada uma com estado desconectado / aguardando QR ou código / conectado (número, nome) / degradado | `[]` |
| Ações por conta | Parear por QR · Parear por código · Reconectar · Desvincular (opções: apagar índice, remover conta) · Renomear · Ativar/desativar | — |
| Adicionar conta | cria uma conta nova e inicia o pareamento (rótulo opcional; padrão "Conta N") | — |
| Retenção do índice | dias | 90 |
| Rate limit de envio | msgs / minutos | 20 / 10 |
| Permitir números não salvos | bool | false |
| Enviar recibo de leitura em `mark_read` | bool | false |

### Conta Google
| Campo | Tipo |
|---|---|
| `client_id` | texto (valida `*.apps.googleusercontent.com`) |
| `client_secret` | secret |
| Passo a passo | painel recolhível com os 5 passos (inclui "Publicar app") |
| Contas conectadas | lista: e-mail, escopos, status, [Reautorizar] [Desconectar] |
| Ação | Conectar conta Google |

### Agenda
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | false |
| Agendas por conta | multi-check (nome, cor, papel) + botão Atualizar lista | nenhuma |
| Agenda padrão para criação | select | primeira com escrita |
| Permitir criar/editar | bool | false |
| Permitir excluir | bool | false |
| Enviar convites a participantes | bool | false |
| Fuso padrão | auto / manual | auto |
| Janela máxima por consulta | dias | 62 |

### Gmail
| Campo | Tipo | Padrão |
|---|---|---|
| Habilitar | bool | false |
| Contas usadas | multi-check | todas |
| Permitir envio | bool | true (política confirmar) |
| Permitir modificar | bool | false |
| Só destinatários já vistos | bool | false |
| Rate limit de envio | por hora | 30 |
| Tamanho máx. de corpo | KB | 64 |
| Pasta de anexos / limite | path, MB | `inbox/gmail`, 25 |

### MCPs externos
| Campo | Tipo |
|---|---|
| Lista de servidores | nome, tipo (`stdio`/`http`/`sse`), status do teste, habilitado, [Editar] [Testar] [Remover] |
| Formulário | nome, `command`/`args`/`env`/`cwd` ou `url`/`headers`; cada valor de env/header com flag **segredo** |
| Liberar ferramentas automaticamente | bool por servidor (+ lista opcional de ferramentas nomeadas) |
| Ações | Adicionar servidor · Importar do Claude Code · Colar JSON · **Ver `.mcp.json` gerado** (somente leitura) |
| Herdar MCPs globais do Claude Code | bool (desliga `--strict-mcp-config`) |

### MCP (complemento)
Tabela de ferramentas agrupada por família com coluna **Política** (`auto` / `confirmar` / `desligada`) e timeout de aprovação (min, padrão 5). Painel **Aprovações pendentes** com ✅/❌ e detalhe dos argumentos.

### Dashboard
Status dos módulos, base de conhecimento, motor (processos vivos, token ativo) e aprovações pendentes.

### Threads
Lista com origem, título, status (🟢🟡⚪❌), última atividade, custo acumulado. Ações por thread: abrir, renomear, reiniciar, zerar, interromper, excluir (com opção de apagar transcrição). Chat com streaming e envio mid-turn: as mensagens enviadas durante um turno aparecem agrupadas ao balão inicial (cantos colados, hora só no último) e não geram "resposta" própria; o servidor grava o turno absorvido com `error_kind = "merged"` e o id do turno pai, e a API expõe `merged_into`. Cada balão traz um rodapé discreto: tempo relativo ("há 5 min"; o hover mostra a data exata), botão "copiar" e, na resposta, um ícone "i" que abre um popover com modelo, duração, custo, início/fim, sessão e id do turno.

### Aprovações
Pendentes como cartões (ferramenta, contagem regressiva, resumo gerado pelo serviço, quem pediu, argumentos em chave/valor com rótulos legíveis e JSON completo em "Detalhes", botões Aprovar/Negar) e histórico em tabela com filtros por texto e decisão (quando, ferramenta, resumo, decisão, por onde foi decidida).

### Logs
Visor estilo terminal (monoespaçado, uma linha por registro: hora · nível · módulo · mensagem, mesmas cores nos dois temas) com histórico do banco + cauda ao vivo; filtro de texto com destaque, nível mínimo, módulo, seguir, quebrar linhas, data, pausar ao vivo, copiar visíveis, limpar tela e abrir a pasta de logs.

## Esquema (Rust)

```rust
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Settings {
    pub schema_version: u32,
    pub general: General,
    pub engine: EngineSettings,
    pub kb: KbSettings,
    pub realtime: RealtimeSettings,
    pub telegram: TelegramSettings,
    pub mcp: McpSettings,
    pub whatsapp: WhatsAppSettings,
    pub google: GoogleSettings,      // client + contas (refs)
    pub calendar: CalendarSettings,
    pub gmail: GmailSettings,
    pub external_mcps: Vec<ExternalMcpServer>,
}
```

- Persistido na tabela `settings` (uma linha JSON + `schema_version`) — mais simples de migrar que colunas.
- Validação com `validator`/manual antes de salvar; erros retornados por campo para a UI.
- Migrações de schema em código (`migrate(v_from) -> Settings`).

## Aplicação a quente

`SettingsChanged { section }` → Orchestrator decide: `general` (sem reinício), `engine` (aplica nos próximos spawns; troca de token derruba vivos), `kb` (derruba todos os processos; nova pasta vale no próximo spawn), `realtime`/`telegram`/`mcp`/`whatsapp`/`google` (stop + start do módulo); mudanças em `calendar`/`gmail` que exigem escopo novo disparam reautorização incremental.

## Onboarding (primeira execução)

Assistente em 3 passos: (1) token do Claude + teste; (2) pasta da KB + criar estrutura; (3) opcional: Telegram e Voz. Ao final, abre uma thread de exemplo na UI.

## Interface

egui/eframe em `crates/will-desktop` (ADR-0016). Modo imediato: cada página lê um snapshot de `Arc<AppState>` e dispara ações no runtime tokio; respostas chegam por canais e `request_repaint` dirigido pelos eventos do `EventBus`. Formulários validam espelhando o backend (erros por campo). Sem lógica de negócio na UI. Diálogos nativos: pasta (`rfd`), QR do WhatsApp desenhado na própria janela.

## Ajuda, idioma e estado dos módulos

**Ajuda "?".** Títulos de página (`ui::heading_help`) e cartões de seção (`ui::section_help`) podem levar um ícone "?" que abre um modal com introdução, passo a passo numerado, bloco "No terminal" (com Copiar), notas (dúvidas e erros comuns) e links. O conteúdo é estático em `crates/will-desktop/src/help.rs` (uma constante `Help` por assunto: tokens e execução do motor, KB, Telegram, voz, WhatsApp, servidor MCP, políticas, MCPs externos, Google). Nada disso ocupa espaço na página fechada.

**Idioma (i18n).** Sem crates externas: os textos da UI ficam em pt-BR no código e são a própria chave. `i18n::tr("Salvar")` devolve o texto no idioma corrente; `i18n::trf("Criados: {}", &[&n])` preenche marcadores `{}` em ordem. A tabela `i18n::TABLE` guarda os pares `(pt-BR, en)`; chave ausente devolve o pt-BR. O idioma vem de Geral › Idioma (`settings.general.language`) e o app chama `set_language` a cada quadro a partir do snapshot, então salvar a seção troca a UI inteira no quadro seguinte (exceções: o menu da bandeja em Windows/macOS é construído na inicialização e só atualiza o item de voz). Testes garantem chaves únicas e mesma quantidade de `{}` nos dois idiomas. Não se traduzem ids, nomes de nível de log, URLs, modelos e textos técnicos copiáveis (ex.: `export …`).

**Estado dos módulos** (`will_core::ModuleStatus`, bolinhas do Início e do menu lateral; rótulo em `ui::module_label`):

| Cor | Estado | Significado |
|---|---|---|
| verde | `Running` → "pronto" | habilitado, configurado e operacional, mesmo ocioso (motor: token ativo + KB ok; voz: habilitada + chave; Google: conta conectada; Telegram: polling no ar; WhatsApp: conta conectada; MCP: ouvindo). O Início acrescenta detalhes ("N processo(s) vivo(s)", "em sessão", "N conta(s)") |
| azul | `Starting` → "iniciando" | transição |
| amarelo | `Degraded(motivo)` → "atenção · motivo" | habilitado, mas falta algo ou algo falha; o motivo é sempre mostrado |
| vermelho | `Error(motivo)` → "erro · motivo" | falhou ao subir ou erro fatal |
| cinza | `Disabled` → "desligado" / `Stopped` → "parado" | desligado nas configurações / parado explicitamente. "Ocioso mas pronto" **não** é cinza: é verde |
