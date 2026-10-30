# 03 — Requisitos Não Funcionais

Convenção: `RNF-<categoria>-<nº>`.

## Plataforma e compatibilidade

| ID | Requisito |
|---|---|
| RNF-PLAT-01 | Suportar Windows 10/11 (x64), macOS 12+ (x64 e arm64) e Linux com glibc 2.31+ (x64), desktops GNOME, KDE e derivados. |
| RNF-PLAT-02 | Um único código-base Rust; diferenças de plataforma isoladas atrás de traits/módulos `cfg(target_os)`. |
| RNF-PLAT-03 | Dependência externa obrigatória: Claude Code CLI instalado e autenticável. Dependências opcionais: Node.js e Python para scripts da pasta `tools/`. |
| RNF-PLAT-04 | Linux: funcionar com Wayland e X11; tray via StatusNotifierItem (`ksni`, D-Bus; GNOME exige a extensão AppIndicator). Atalho global pode ter limitações em Wayland e deve degradar com aviso claro. |
| RNF-PLAT-05 | O app compila sem libs de GUI em tempo de build (X11/Wayland carregados em runtime); no Linux a única dependência de compilação além do toolchain Rust é `libasound2-dev` (cpal). Um único toolchain (Rust), sem Node/WebView. |

## Desempenho

| ID | Requisito |
|---|---|
| RNF-PERF-01 | Consumo em repouso: < 80 MB de RAM e < 1% de CPU com todos os módulos habilitados mas ociosos. |
| RNF-PERF-02 | Latência de abertura da sessão de voz (atalho → microfone ativo e WebSocket pronto): < 1,5 s em rede normal. |
| RNF-PERF-03 | Latência de encaminhamento de mensagem Telegram → spawn do processo `claude`: < 500 ms (excluindo o tempo do próprio Claude). |
| RNF-PERF-04 | Streaming de saída do Claude Code deve ser repassado à UI em < 100 ms por evento. |
| RNF-PERF-05 | O áudio deve ser processado em thread dedicada de tempo real (cpal), sem alocações no callback de áudio. |

## Confiabilidade

| ID | Requisito |
|---|---|
| RNF-REL-01 | Falha em um módulo (ex.: Telegram sem rede) não derruba o serviço nem os demais módulos. Cada módulo tem supervisor com backoff exponencial de reconexão. |
| RNF-REL-02 | Processos `claude` órfãos devem ser encerrados ao sair do serviço (process group / job object no Windows). |
| RNF-REL-03 | Estado de threads deve sobreviver a reinícios: ao subir, threads voltam como `idle`; turnos interrompidos são marcados como `interrupted`. |
| RNF-REL-04 | Escrita no SQLite em modo WAL com transações curtas; nunca bloquear a UI aguardando I/O. |
| RNF-REL-05 | Mensagens Telegram recebidas durante indisponibilidade do motor devem ficar na fila e ser processadas depois (sem perda), com limite de fila por tópico. |

## Segurança

| ID | Requisito |
|---|---|
| RNF-SEC-01 | Segredos (tokens Claude, OpenAI, Telegram) armazenados no keyring do SO (Credential Manager, Secret Service) ou, no macOS, num cofre cifrado cuja chave-mestra é um único item do Keychain (ADR-0024). Nunca em texto plano em disco nem em logs. |
| RNF-SEC-02 | Servidor MCP escuta apenas em `127.0.0.1`; cada requisição exige bearer token efêmero por spawn. |
| RNF-SEC-03 | Telegram: deny-by-default. Sem allowlist configurada, o módulo não inicia. |
| RNF-SEC-04 | O modo `bypassPermissions` do Claude Code exige confirmação explícita na UI com aviso de risco. |
| RNF-SEC-05 | Logs redigem automaticamente padrões de segredo (`sk-…`, tokens de bot, bearer). |
| RNF-SEC-06 | O serviço nunca executa conteúdo da base de conhecimento diretamente; apenas o Claude Code o faz, dentro das permissões configuradas. |
| RNF-SEC-08 | Ferramentas MCP com efeito externo (enviar mensagem/e-mail, alterar agenda) exigem aprovação humana por padrão; conteúdo lido de e-mails/mensagens é tratado como dado não confiável. |
| RNF-SEC-09 | Sessão do WhatsApp e refresh tokens do Google nunca saem da máquina; arquivos de sessão com permissão 0600; revogação limpa keyring e disco. |
| RNF-SEC-07 | Builds de release assinados (Authenticode no Windows, notarização no macOS) quando certificados estiverem disponíveis; checksums publicados sempre. |

## Usabilidade

| ID | Requisito |
|---|---|
| RNF-UX-01 | Configuração inicial completa (token Claude + pasta KB) em menos de 2 minutos via assistente de primeira execução. |
| RNF-UX-02 | Toda mensagem de erro deve dizer o que aconteceu e o que fazer. |
| RNF-UX-03 | A UI segue o tema do sistema (claro/escuro). |
| RNF-UX-04 | Idioma padrão pt-BR, com infraestrutura de i18n para inglês. |

## Manutenibilidade

| ID | Requisito |
|---|---|
| RNF-MNT-01 | Cargo workspace com crates por domínio; o crate de UI (`will-desktop`, egui) não contém lógica de negócio: lê snapshots do estado e dispara ações no serviço. |
| RNF-MNT-02 | `cargo clippy -- -D warnings` e `cargo fmt --check` obrigatórios no CI. |
| RNF-MNT-03 | Cobertura de testes unitários nos crates de domínio (parser de stream-json, roteamento de threads, mapeamento Telegram) e testes de integração com binário `claude` falso (mock). |
| RNF-MNT-04 | Toda decisão arquitetural relevante documentada como ADR em `docs/adr/`. |
| RNF-MNT-05 | Versionamento semântico; changelog gerado a partir de commits convencionais. |

## Observabilidade

| ID | Requisito |
|---|---|
| RNF-OBS-01 | `tracing` com spans por turno (`thread_id`, `turn_id`, `origin`) e por chamada MCP. |
| RNF-OBS-02 | Métricas básicas em memória expostas na UI: turnos/hora, custo acumulado por thread, erros por módulo. |

## Privacidade

| ID | Requisito |
|---|---|
| RNF-PRIV-01 | Telemetria **opt-in e desligada por padrão** (`docs/29`): só com `telemetry.enabled` o serviço envia logs/spans redigidos ao endpoint OTLP configurado (Axiom ou outro). Fora isso, os únicos destinos de rede são: Anthropic (via Claude Code), OpenAI (Realtime), Telegram, servidores do WhatsApp (se pareado) e APIs do Google (se conectado). |
| RNF-PRIV-03 | O índice local do WhatsApp e anexos baixados (WhatsApp/Gmail) ficam em disco local com retenção configurável e são apagados ao desvincular/desconectar, se o usuário optar. |
| RNF-PRIV-02 | Áudio da sessão de voz não é gravado em disco por padrão; transcrições são salvas apenas se habilitado. |
