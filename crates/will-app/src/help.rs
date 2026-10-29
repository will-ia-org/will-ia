//! Textos de ajuda (passo a passo, notas, links) mostrados no modal do ícone "?".
//!
//! Nada daqui ocupa espaço nas páginas: cada seção que precisa de orientação
//! longa aponta para um `Help` e a UI desenha só o ícone.
//!
//! Idioma: os textos ficam em pt-BR (chave de tradução) e a tradução para
//! inglês de cada título, intro, passo, nota e rótulo de link está em
//! `crate::i18n::TABLE`; quem renderiza o `Help` passa cada campo por
//! `i18n::tr` (as `&'static str` daqui são chaves válidas para `tr`).

use crate::ui::Help;

// ---------- Motor Claude ----------

pub const ENGINE_TOKENS: Help = Help {
    title: "Token de assinatura do Claude",
    intro: "O will-ia não usa API key da Anthropic: ele roda o Claude Code CLI com um token da sua assinatura (Pro/Max), gerado pelo próprio CLI. O token vai para o cofre de segredos e entra no processo como CLAUDE_CODE_OAUTH_TOKEN.",
    steps: &[
        ("Instalar o Claude Code CLI", "Precisa de Node.js 18 ou mais novo. No terminal: npm i -g @anthropic-ai/claude-code. Confira com claude --version. Em Execução › Binário claude, clique em Verificar."),
        ("Gerar o token", "No terminal rode claude setup-token. O navegador abre para você entrar na conta da assinatura; ao autorizar, o terminal mostra um token longo começando com sk-ant-oat01-."),
        ("Cadastrar aqui", "Copie o token inteiro, cole no campo abaixo, dê um rótulo (ex.: \"conta pessoal\") e salve. O valor nunca aparece de novo: só os últimos 4 caracteres."),
        ("Testar e ativar", "Testar abre um processo descartável e confirma que o token funciona. Ativar aplica a todos os processos: os vivos são derrubados e voltam com --resume na próxima mensagem."),
    ],
    notes: &[
        ("Só um token fica ativo", "Cadastre vários (contas diferentes) e alterne quando um atingir o limite de uso da assinatura."),
        ("Token expirou ou foi revogado", "O status vira \"erro\" e as threads deixam de responder. Gere outro com claude setup-token e ative."),
        ("Segurança", "O token nunca é gravado em settings, logs, .mcp.json ou chat. Em modo portátil ele fica cifrado no arquivo de segredos ao lado do binário."),
    ],
    code: "npm i -g @anthropic-ai/claude-code\nclaude --version\nclaude setup-token",
    links: &[
        ("Claude Code: instalação", "https://docs.anthropic.com/en/docs/claude-code/setup"),
        ("Node.js", "https://nodejs.org/"),
    ],
};

pub const ENGINE_THREAD_SUMMARY: Help = Help {
    title: "Resumo automático das threads",
    intro: "Cada thread ganha uma linha de prévia, em itálico sob o título (na lista e no cabeçalho da conversa), com o que está sendo tratado por último. O serviço gera essa linha sozinho, em segundo plano, a cada N turnos concluídos, pedindo a um modelo de chat da OpenAI um resumo dos últimos N turnos.",
    steps: &[
        ("Pré-requisito", "Chave da OpenAI cadastrada em Voz › Configuração (o módulo de voz não precisa estar habilitado). Sem chave, nada é gerado e nenhum erro aparece."),
        ("Ajustar", "A cada quantos turnos define a frequência e o tamanho da janela resumida. O Prompt controla estilo e tamanho; Tamanho máximo corta o texto guardado."),
    ],
    notes: &[
        ("Custo", "Uma chamada curta por janela (tokens dos últimos N turnos); entra na página Custos como thread_summary, estimado."),
        ("Quando atualiza", "Só depois de um turno concluído com resposta; turnos com erro não contam nem entram no resumo. Threads antigas ganham resumo no próximo turno que fechar a janela."),
        ("Ver inteiro", "Na lista, o resumo completo aparece no tooltip da thread; no cabeçalho, ao passar o mouse."),
    ],
    code: "",
    links: &[],
};

pub const ENGINE_EXECUTION: Help = Help {
    title: "Como o motor executa o Claude",
    intro: "Cada thread é um processo claude -p headless, vivo enquanto conversa e aposentado quando fica ocioso ou passa do teto. A sessão é preservada: o processo volta com --resume sem perder o histórico.",
    steps: &[],
    notes: &[
        ("Modo de permissão", "acceptEdits (recomendado): o Claude edita arquivos da base e só roda o que estiver em Ferramentas permitidas; o resto é negado, porque em modo headless não há como perguntar. bypassPermissions: executa qualquer comando sem confirmação (--dangerously-skip-permissions). Use só numa base isolada e com as ferramentas MCP de envio em \"confirmar\"."),
        ("Mudou o modo e nada aconteceu?", "A troca vale para processos novos. Threads com processo vivo continuam no modo antigo até serem aposentadas (ociosidade) ou reiniciadas na página Threads."),
        ("Ferramentas permitidas", "Uma por linha, na sintaxe do Claude Code: Read, Edit, Write, Bash(npm *), Bash(git status), WebFetch(domain:docs.exemplo.com). As ferramentas do servidor MCP entram automaticamente como mcp__will_ia__*."),
        ("Processos vivos e ociosidade", "Acima do máximo, o processo menos usado é aposentado (sessão mantida). Ociosidade encerra processos sem mensagens; a thread continua na lista como dormente."),
        ("Orçamento e timeout do turno", "O orçamento em USD limita o que um turno pode gastar; o timeout encerra turnos travados. Ambos aparecem como erro na thread, não derrubam o serviço."),
        ("Herdar MCPs globais", "Desligado, o processo roda com --strict-mcp-config e só enxerga o .mcp.json da base (will_ia + MCPs externos cadastrados aqui)."),
    ],
    code: "",
    links: &[("Claude Code: permissões e allowlist", "https://docs.anthropic.com/en/docs/claude-code/settings")],
};

// ---------- Base de conhecimento ----------

pub const KB: Help = Help {
    title: "Base de conhecimento",
    intro: "É a pasta onde o Claude Code roda. Tudo o que o agente sabe sobre você e tudo o que ele produz mora aqui, em arquivos de texto que você pode abrir, editar e versionar como quiser. O serviço só cria o esqueleto inicial e gera o .mcp.json; nunca roda git nem apaga nada.",
    steps: &[
        ("Escolher a pasta", "Qualquer pasta vazia ou já existente (ex.: ~/will-kb). Em modo portátil pode ser relativa à raiz do pendrive. Mudar a pasta derruba os processos vivos."),
        ("Criar estrutura inicial", "Gera, sem sobrescrever o que já existe: CLAUDE.md (regras do agente), .claude/agents/ (pesquisador, bibliotecário, ferramenteiro), .claude/settings.json, knowledge/ (conhecimento curado), knowledge/pessoa/ (perfil seu com lacunas __preencher__), inbox/ (anexos recebidos), tools/ (scripts Node/Python do agente) e .gitignore."),
        ("Conferir o status", "A seção Status mostra se a pasta existe, é gravável, tem CLAUDE.md e .mcp.json, e se Node.js e Python foram encontrados (opcionais, para scripts em tools/)."),
    ],
    notes: &[
        ("Perfil da pessoa (knowledge/pessoa/)", "Arquivos com campos em branco (nome, endereço, saúde, família, trabalho, finanças, preferências…). O agente vai preenchendo aos poucos, conforme as conversas, sem ficar perguntando; pode criar arquivos novos no mesmo padrão."),
        ("Rodar de novo é seguro", "\"Criar estrutura inicial\" só cria o que falta. Se o CLAUDE.md já existia, apenas a seção \"Perfil da pessoa\" é acrescentada, uma vez."),
        (".mcp.json", "Gerado pelo serviço com o servidor will_ia e os MCPs externos; segredos entram como ${VAR}, nunca em texto. Edições manuais são importadas, não sobrescritas."),
        ("Versionar com git", "Recomendado, mas é você quem roda: git init na pasta, commits quando quiser. O .gitignore gerado já exclui inbox/ e settings.local.json."),
    ],
    code: "",
    links: &[("Claude Code: CLAUDE.md e memória", "https://docs.anthropic.com/en/docs/claude-code/memory")],
};

// ---------- Telegram ----------

pub const TELEGRAM: Help = Help {
    title: "Configurar o bot do Telegram",
    intro: "O bot conversa num supergrupo com Tópicos: cada tópico é uma thread independente do cérebro. Ele é deny-by-default: só responde a chats E usuários que estiverem nas listas de permissão; fora disso, só o comando /start responde.",
    steps: &[
        ("Criar o bot no @BotFather", "No Telegram, abra @BotFather e mande /newbot. Escolha um nome e um username terminado em \"bot\". Ele devolve o token (formato 123456789:AAxxxx…). Copie."),
        ("Colar o token aqui", "Em Configuração › Token do bot, cole e salve. Clique em Testar: deve aparecer o @username do bot. O token vai para o cofre de segredos."),
        ("Criar o grupo com Tópicos", "Crie um grupo novo, adicione o bot como membro. Em Editar grupo, ligue \"Tópicos\" (o grupo vira supergrupo). Dê um nome a ele, ex.: \"Will\"."),
        ("Promover o bot a administrador", "Editar grupo › Administradores › adicione o bot com a permissão \"Gerenciar tópicos\". Como admin ele lê todas as mensagens (não precisa mexer no /setprivacy do BotFather)."),
        ("Habilitar o módulo", "Em Configuração, ligue Habilitar e salve. O bot começa a escutar (long polling)."),
        ("Descobrir os IDs", "No grupo, mande /start. O bot responde com o chat ID (negativo, começa com -100) e o seu user ID. Faça o mesmo no privado com o bot se quiser conversar por lá também."),
        ("Preencher as listas e salvar", "Cole o chat ID em Chats permitidos e o seu user ID em Usuários permitidos. Salve. A partir daí o bot responde no grupo e nos tópicos."),
        ("Criar tópicos", "Mande /topico Nome no grupo: o bot cria o tópico e saúda nele. Cada tópico é uma conversa paralela com o cérebro; o \"General\" também vale."),
    ],
    notes: &[
        ("O bot não responde", "Confira: módulo habilitado e salvo, token testado, chat ID (com o sinal negativo) e user ID nas listas, bot administrador do grupo. Mande /start: ele diz se o chat/usuário está liberado."),
        ("Tópicos no chat privado não funcionam", "O \"modo de tópicos\" do BotFather para chats privados está quebrado na Bot API. O paralelismo usa o supergrupo com fórum."),
        ("Comandos úteis", "/start (IDs e situação), /topico <nome> (novo tópico), /apagar (dentro de um tópico: apaga thread e tópico), /limpar (lista/remove threads dormentes). Os commands de .claude/commands/ da base também aparecem no menu."),
        ("Anexos e áudios", "Arquivos vão para <kb>/inbox/telegram/ e o caminho é passado ao agente. Mensagens de voz são transcritas com a chave da OpenAI (seção Voz) e tratadas como texto."),
        ("Aprovações pelo Telegram", "Ferramentas com política \"confirmar\" podem ser aprovadas por botões na mensagem do bot, além da UI e da notificação."),
    ],
    code: "",
    links: &[
        ("@BotFather", "https://t.me/BotFather"),
        ("Telegram: tópicos em grupos", "https://telegram.org/blog/topics-in-groups-collectible-usernames"),
    ],
};

// ---------- Voz ----------

pub const VOICE_SESSION: Help = Help {
    title: "Como usar a voz",
    intro: "A voz usa o OpenAI Realtime com o microfone e o alto-falante deste computador. Perguntas rápidas são respondidas pela própria voz; tarefas e tudo que depende da sua base de conhecimento vão para o cérebro (Claude) pela ferramenta ask_brain e a resposta é lida em voz alta.",
    steps: &[
        ("Ligar a sessão", "Clique em Iniciar (ou use o atalho global). O indicador flutuante mostra o estado: ouvindo, pensando, falando."),
        ("Falar", "No modo Alternar, o microfone fica aberto e o servidor detecta quando você terminou. No modo Push-to-talk, segure o botão ou o atalho enquanto fala; \"Enviar agora\" força o fim do turno."),
        ("Threads por voz", "Peça \"nova conversa sobre X\" e a voz cria uma thread com título a partir do pedido; peça \"continue na thread Y\" para retomar uma existente (ela lista as threads com list_threads)."),
    ],
    notes: &[
        ("O assistente se corta sozinho", "Com alto-falante, deixe \"Interromper por voz\" desligado: o microfone fica mudo enquanto ele fala. Ligue só com fone de ouvido."),
        ("Não detecta a minha fala", "Baixe a Sensibilidade do detector (0.3 é mais sensível) ou troque o microfone em Configuração e use Testar dispositivos."),
        ("Atalho global", "Ex.: Ctrl+Shift+Space. Em Linux com Wayland o atalho global não funciona; use o botão."),
        ("Custo", "Cada turno de voz é estimado pela tabela de preços e aparece na página Custos."),
    ],
    code: "",
    links: &[],
};

pub const VOICE_CONFIG: Help = Help {
    title: "Chave da OpenAI para a voz",
    intro: "A voz (OpenAI Realtime) e a transcrição de áudios do Telegram usam uma API key da sua conta na plataforma da OpenAI (é cobrada por uso, separada da assinatura do ChatGPT).",
    steps: &[
        ("Criar a conta na plataforma", "Entre em platform.openai.com com sua conta OpenAI. Em Settings › Billing adicione créditos ou um cartão: sem saldo a sessão de voz falha com erro de cota."),
        ("Gerar a chave", "Em API keys clique em \"Create new secret key\", dê um nome (ex.: will-ia) e copie o valor (começa com sk-). Ele só é mostrado uma vez."),
        ("Colar aqui", "Em OpenAI API key, cole e salve. A chave vai para o cofre de segredos e nunca aparece de novo inteira."),
        ("Habilitar e testar", "Ligue Habilitar, salve, e em Sessão de voz clique em Iniciar. Use Testar dispositivos para conferir microfone e saída antes."),
    ],
    notes: &[
        ("Modelos", "gpt-realtime para a conversa; gpt-4o-mini-transcribe para transcrever áudios do Telegram. Troque só se a OpenAI publicar modelos novos."),
        ("Voz e persona", "A voz (alloy, ballad, shimmer…) e as Instruções da persona definem como o assistente fala; o conteúdo vem do cérebro."),
        ("Limite de caracteres falados", "Respostas longas do cérebro são resumidas pela voz até esse tamanho; o texto completo fica na thread."),
    ],
    code: "",
    links: &[
        ("OpenAI: API keys", "https://platform.openai.com/api-keys"),
        ("OpenAI: Billing", "https://platform.openai.com/settings/organization/billing/overview"),
    ],
};

pub const VOICE_SPEAK: Help = Help {
    title: "Ouvir um resumo falado das respostas",
    intro: "Em Threads, cada resposta do agente ganha um botão \"ouvir\" ao lado de \"copiar\". Ao clicar, o servidor pede a um modelo de chat da OpenAI um roteiro fluido (frases corridas, sem código, links, IDs ou marcação) e sintetiza esse roteiro em voz; o app toca no dispositivo de saída escolhido acima. Clique de novo para parar.",
    steps: &[
        ("Pré-requisitos", "Chave da OpenAI cadastrada e módulo de voz habilitado (a sessão de voz não precisa estar ativa). Sem isso o botão não aparece."),
        ("Ajustar o roteiro", "Edite o Prompt do resumo para mudar tamanho, tom ou o que deve ser omitido. Restaurar padrão volta ao texto original."),
        ("Voz e velocidade", "Por padrão usa a mesma voz da sessão de voz; escolha outra e ajuste a velocidade se preferir."),
    ],
    notes: &[
        ("Custo", "Duas chamadas por clique: o resumo (tokens do modelo de chat) e o TTS (por caractere do roteiro). Ambas entram na página Custos como voice_summary e voice_tts, estimadas."),
        ("Uma leitura por vez", "Começar outra resposta interrompe a anterior. O roteiro gerado aparece ao passar o mouse sobre o botão."),
        ("Ouvir de novo não custa", "O roteiro e o áudio ficam guardados por resposta (memória e disco do app). Clicar de novo toca na hora, sem chamar a OpenAI."),
        ("Trocar de modelo", "Pode usar gpt-5-nano, gpt-4.1-mini, tts-1 etc. O servidor ajusta os parâmetros por família (temperatura, esforço de raciocínio, instruções do TTS) e, se a OpenAI recusar algum, remove e repete sozinho."),
        ("Telegram", "O áudio das respostas no Telegram usa o prompt padrão deste resumo, com a voz e a velocidade configuradas na seção Telegram."),
    ],
    code: "",
    links: &[("OpenAI: text to speech", "https://platform.openai.com/docs/guides/text-to-speech")],
};

pub const VOICE_PRICING: Help = Help {
    title: "Estimativa de custo da voz",
    intro: "A OpenAI não devolve o valor em dinheiro em cada resposta, só a contagem de tokens (texto e áudio, entrada, saída e cache). O app multiplica esses tokens pela tabela abaixo e registra na página Custos.",
    steps: &[],
    notes: &[
        ("Valores padrão", "São os preços públicos do gpt-realtime por 1 milhão de tokens na data da versão. Confira na página de preços da OpenAI e ajuste quando mudarem."),
        ("É só estimativa", "O valor real é o da fatura na OpenAI. Diferenças pequenas são normais (arredondamentos, cache)."),
    ],
    code: "",
    links: &[("OpenAI: preços", "https://openai.com/api/pricing/")],
};

// ---------- WhatsApp ----------

pub const WHATSAPP: Help = Help {
    title: "Parear o WhatsApp",
    intro: "O will-ia conecta o seu WhatsApp pessoal como um \"aparelho conectado\" (igual ao WhatsApp Web) e expõe aos agentes só ferramentas whatsapp_* (listar conversas, ler, buscar, enviar com confirmação). Mensagens recebidas não criam threads: são dados para o agente consultar quando você pedir.",
    steps: &[
        ("Habilitar o módulo", "Em Configuração, ligue Habilitar módulo e salve."),
        ("Nova conta por QR", "Clique em \"+ Nova conta por QR\". No telefone: WhatsApp › Configurações › Aparelhos conectados › Conectar aparelho, e aponte a câmera para o código na tela."),
        ("Ou por número", "Clique em \"Conectar com número\", digite o telefone com DDI (só dígitos, ex.: 5511999990000). No telefone: Aparelhos conectados › Conectar com número de telefone, e digite o código mostrado."),
        ("Aguardar a sincronização", "A conta aparece como conectada e o índice local começa a receber as conversas. Dê um rótulo (ex.: \"pessoal\", \"trabalho\") para distinguir várias contas."),
    ],
    notes: &[
        ("Protocolo não oficial", "Usa o protocolo do WhatsApp Web por engenharia reversa (whatsapp-rust). Funciona, mas o WhatsApp pode mudar algo ou, em casos raros, restringir a conta. Use com moderação e respeite os limites de envio."),
        ("Envio sempre com confirmação", "whatsapp_send nasce com política \"confirmar\": você aprova cada envio na UI, notificação ou Telegram, vendo o destinatário e o texto."),
        ("Índice e retenção", "As mensagens ficam num índice local (SQLite com busca de texto) por N dias; depois são apagadas. Nada sai desta máquina além das chamadas ao WhatsApp."),
        ("Desconectar", "Faz logout remoto (o telefone deixa de listar o aparelho) e apaga a sessão local. Para voltar, pareie de novo."),
    ],
    code: "",
    links: &[("WhatsApp: aparelhos conectados", "https://faq.whatsapp.com/1317564962315842")],
};

// ---------- Servidor MCP ----------

pub const MCP_SERVER: Help = Help {
    title: "Servidor MCP local",
    intro: "É um servidor MCP (Streamable HTTP) só em 127.0.0.1 que entrega aos agentes as ferramentas do will-ia: telegram_*, whatsapp_*, calendar_*, gmail_*, notify_user. Cada processo do Claude recebe um bearer efêmero, então cada chamada sabe de qual thread veio.",
    steps: &[
        ("Token para scripts", "Além dos tokens por processo, existe um token fixo para você usar em scripts e chamadas HTTP de fora das threads. Ele persiste entre reinicializações e fica no cofre de segredos."),
        ("Copiar como export", "Copia WILL_IA_MCP_URL e WILL_IA_MCP_TOKEN prontos para colar num terminal. A URL muda a cada execução quando a porta é 0 (aleatória); fixe uma porta se preferir URL estável."),
        ("Chamar o servidor", "Use o header Authorization: Bearer <token>. O exemplo abaixo faz o handshake initialize; depois dele, tools/list e tools/call funcionam normalmente."),
    ],
    notes: &[
        ("Chamadas com o token de scripts", "Não têm thread de origem: ferramentas que respondem \"na thread atual\" (ex.: telegram_reply) não se aplicam; as demais funcionam e passam pelas mesmas políticas e aprovações."),
        ("Regenerar", "Invalida o token anterior na hora. Scripts que o usavam precisam do novo valor."),
        ("Porta", "0 = aleatória a cada execução (recomendado para evitar conflito); o .mcp.json da base é regravado com a URL atual, então os agentes sempre acham o servidor."),
        ("Timeout de aprovação", "Pedido de confirmação sem resposta é negado após esse tempo, e a ferramenta devolve o erro ao agente."),
    ],
    code: "eval \"$(xclip -o 2>/dev/null || wl-paste)\"   # ou cole o export copiado\ncurl -s \"$WILL_IA_MCP_URL\" \\\n  -H \"Authorization: Bearer $WILL_IA_MCP_TOKEN\" \\\n  -H 'Content-Type: application/json' \\\n  -H 'Accept: application/json, text/event-stream' \\\n  -d '{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{},\"clientInfo\":{\"name\":\"script\",\"version\":\"0\"}}}'",
    links: &[("Especificação MCP (Streamable HTTP)", "https://modelcontextprotocol.io/specification/2025-03-26/basic/transports")],
};

pub const MCP_TOOLS: Help = Help {
    title: "Políticas das ferramentas",
    intro: "Cada ferramenta do will-ia tem uma política que decide o que acontece quando um agente a chama.",
    steps: &[],
    notes: &[
        ("auto", "Executa na hora, sem perguntar. Bom para leitura: listar e-mails, buscar mensagens, ver a agenda."),
        ("confirmar", "Cria um pedido de aprovação com um resumo gerado pelo serviço (nunca pelo agente): destinatário, assunto, texto. Você aprova ou nega na página Aprovações, na notificação do sistema ou por botões no Telegram. Sem resposta no tempo limite, é negada."),
        ("desligada", "A ferramenta nem é exposta ao agente."),
        ("Padrões", "Tudo que tem efeito externo (enviar mensagem/e-mail, criar/alterar/apagar evento, modificar e-mail) nasce em \"confirmar\". Leituras nascem em \"auto\"."),
        ("Allowlists", "Mesmo em auto, envios só vão para destinos conhecidos: chats da allowlist do Telegram, contatos já vistos no WhatsApp, endereços que já apareceram em e-mails."),
    ],
    code: "",
    links: &[],
};

// ---------- MCPs externos ----------

pub const EXTERNAL_MCP: Help = Help {
    title: "MCPs externos",
    intro: "Servidores MCP de terceiros (GitHub, Notion, bancos de dados, navegador…) que você quer dar aos agentes. O will-ia escreve todos eles, junto com o will_ia, no .mcp.json da base de conhecimento; o Claude Code os carrega em cada thread.",
    steps: &[
        ("Adicionar manualmente", "Clique em Adicionar. Tipo stdio: comando + argumentos (ex.: npx -y @modelcontextprotocol/server-github). Tipo http: a URL do servidor remoto e, se preciso, headers."),
        ("Importar do Claude Code", "\"Importar do Claude (~/.claude.json)\" traz os servidores já configurados no escopo de usuário do seu Claude Code."),
        ("Importar JSON de um README", "\"Importar JSON\" aceita o bloco {\"mcpServers\": {...}} que os projetos publicam na documentação. Valores literais em env/headers viram texto; marque como segredo depois."),
        ("Segredos", "No editor, marque \"segredo\" em tokens e senhas: o valor vai para o cofre e o processo recebe ${WILL_MCP_<NOME>_<CHAVE>}, injetado na hora do spawn. O .mcp.json nunca contém o valor."),
        ("Salvar e testar", "Ao salvar, o .mcp.json é regravado. Threads novas já enxergam o servidor; as vivas, depois de reiniciadas. Em Base de conhecimento › Ver .mcp.json dá para conferir o resultado."),
    ],
    notes: &[
        ("Liberar ferramentas automaticamente", "Ligado, as ferramentas do servidor entram na allowlist do Claude Code (mcp__<nome>__*) e rodam sem perguntar. Desligado, em modo acceptEdits o Claude nega o uso, porque headless não há prompt. Restrinja em Ferramentas permitidas se quiser só algumas."),
        ("Edições manuais no .mcp.json", "São importadas para o catálogo, não sobrescritas. Mas segredos literais escritos à mão continuam em texto puro: prefira cadastrar aqui."),
        ("Herdar MCPs globais", "Em Motor Claude › Execução, \"Herdar MCPs globais do Claude\" decide se os servidores do ~/.claude.json também entram, além dos daqui."),
    ],
    code: "",
    links: &[
        ("Claude Code: MCP", "https://docs.anthropic.com/en/docs/claude-code/mcp"),
        ("Catálogo de servidores MCP", "https://github.com/modelcontextprotocol/servers"),
    ],
};

pub const EXTERNAL_EDITOR: Help = Help {
    title: "Editor de servidor MCP",
    intro: "",
    steps: &[],
    notes: &[
        ("stdio", "O will-ia não executa o servidor: o Claude Code o inicia a partir do .mcp.json com o comando e os argumentos daqui. Um argumento por linha. Pasta de trabalho é opcional."),
        ("http", "Servidor remoto (Streamable HTTP). Use Headers para autenticação, ex.: Authorization = Bearer <token>."),
        ("Variáveis de ambiente e headers", "Marque \"segredo\" para tokens: o valor vai para o cofre e o processo recebe ${WILL_MCP_<NOME>_<CHAVE>} na hora do spawn. Valores não marcados ficam em texto no .mcp.json."),
        ("Ferramentas permitidas", "Uma por linha; vazio = todas as ferramentas do servidor. Só tem efeito com \"Liberar ferramentas automaticamente\" ligado."),
    ],
    code: "",
    links: &[],
};

// ---------- Google ----------

pub const GOOGLE_OAUTH: Help = Help {
    title: "Criar o cliente OAuth do Google",
    intro: "O will-ia não embute credenciais do Google: você cria um cliente OAuth no seu próprio projeto do Google Cloud e cola o ID e a chave aqui. A autorização usa PKCE com redirecionamento para 127.0.0.1, sem servidor nosso no meio.",
    steps: &[
        ("Projeto", "Em console.cloud.google.com, no topo, selecione ou crie um projeto (ex.: \"will-ia\")."),
        ("Ativar as APIs", "Menu › APIs e serviços › Biblioteca. Procure e ative \"Google Calendar API\" e \"Gmail API\"."),
        ("Tela de consentimento OAuth", "APIs e serviços › Tela de permissão OAuth (ou \"Google Auth Platform › Branding\"). Tipo de usuário: Externo. Preencha nome do app e e-mails de suporte. Em \"Público-alvo\" (Test users) adicione o seu próprio e-mail do Google."),
        ("Publicar o app", "Ainda na tela de consentimento, clique em \"Publicar aplicativo\". Em modo Teste o Google expira o refresh token em 7 dias e você teria de reautorizar toda semana. Não é preciso enviar para verificação: use normalmente com o aviso de \"app não verificado\" (clique em Avançado › Acessar)."),
        ("Criar o cliente", "APIs e serviços › Credenciais › Criar credenciais › ID do cliente OAuth. Tipo de aplicativo: App para computador (Desktop app). Dê um nome e clique em Criar."),
        ("Copiar e colar aqui", "Na janela que abre, copie o \"ID do cliente\" (termina em .apps.googleusercontent.com) e a \"Chave secreta do cliente\" (começa com GOCSPX-). Cole nos campos client_id e client_secret e salve."),
        ("Conectar", "Clique em \"Conectar conta\", escolha os escopos e autorize no navegador. A conta aparece na lista ao concluir. Reautorizar amplia os escopos depois."),
    ],
    notes: &[
        ("Erro \"redirect_uri_mismatch\" ao autorizar", "O cliente foi criado como \"Aplicativo da Web\" em vez de \"App para computador\". Só o tipo computador aceita o redirecionamento http://127.0.0.1 com porta aleatória sem cadastro. Resolva de um dos dois jeitos: crie um cliente novo do tipo App para computador (recomendado), ou fixe a Porta do callback e cadastre exatamente a URI mostrada em \"URIs de redirecionamento autorizados\" do cliente Web."),
        ("Aviso \"O Google não verificou este app\"", "Normal para um app seu, não verificado. Clique em Avançado › Acessar <nome do app>. Nenhum dado sai do seu computador: o cliente é seu."),
        ("Conta parou de funcionar depois de uma semana", "O app ainda está em modo Teste no Google Cloud. Publique (passo 4) e reautorize a conta."),
        ("Várias contas", "Repita Conectar conta para cada uma (pessoal, trabalho). As ferramentas recebem o e-mail da conta a usar."),
    ],
    code: "",
    links: &[
        ("Google Cloud: credenciais", "https://console.cloud.google.com/apis/credentials"),
        ("Google Cloud: biblioteca de APIs", "https://console.cloud.google.com/apis/library"),
        ("Google Cloud: tela de consentimento", "https://console.cloud.google.com/apis/credentials/consent"),
    ],
};

pub const GOOGLE_CALENDAR: Help = Help {
    title: "Agenda",
    intro: "Ferramentas calendar_* para os agentes: listar eventos por período, buscar e, se permitido, criar, alterar e apagar. Eventos recebidos não criam threads: são dados consultados quando você pede.",
    steps: &[],
    notes: &[
        ("Quais agendas", "Clique em Atualizar agendas para carregar as agendas das contas conectadas e marque as que o agente pode consultar. A agenda padrão recebe os eventos criados sem agenda explícita."),
        ("Criar e editar", "Exige o escopo \"Agenda (criar/editar)\" na autorização da conta (use Reautorizar se conectou só com leitura). As ferramentas de escrita nascem com política \"confirmar\"."),
        ("Janela máxima por consulta", "Limita o período que uma única chamada pode varrer, para evitar respostas enormes."),
    ],
    code: "",
    links: &[],
};

pub const GOOGLE_DRIVE: Help = Help {
    title: "Drive",
    intro: "Ferramentas drive_* para os agentes: listar pastas, buscar (sintaxe q do Drive), ler metadados, baixar (Docs/Planilhas/Apresentações saem exportados) e, se permitido, criar pastas, enviar/criar/renomear/mover arquivos, lixeira e compartilhamento. O conteúdo dos arquivos é dado não confiável: nunca vira instrução.",
    steps: &[],
    notes: &[
        ("Escopos", "Drive (ler) = drive.readonly; Drive (ler e escrever) = drive completo, necessário para mexer em arquivos que já existiam."),
        ("Contas", "Nenhuma marcada = todas as contas conectadas com escopo de Drive. Marque para restringir."),
        ("Escrever", "drive_upload, drive_create_file, drive_update e drive_trash nascem em \"confirmar\"; drive_create_folder é automático."),
        ("Compartilhar", "drive_share dá acesso a terceiros e fica desligado por padrão; exclusão definitiva também."),
        ("Downloads", "Vão para a pasta configurada dentro da base de conhecimento, sem sobrescrever (sufixo numérico), respeitando o limite de MB."),
    ],
    code: "",
    links: &[("Drive: sintaxe de busca", "https://developers.google.com/drive/api/guides/search-files")],
};

pub const MACOS_PERMISSIONS: Help = Help {
    title: "Permissões do macOS",
    intro: "O will-ia pede tudo de uma vez na primeira abertura depois de instalar: microfone (voz) e notificações pelo app; Arquivos e Pastas pelo servidor, ao tocar na base de conhecimento. O instalador assina os binários com uma identidade estável, então a concessão continua valendo depois de atualizar.",
    steps: &[],
    notes: &[
        ("Microfone", "Só o app usa (voz). Negado = Ajustes → Privacidade e Segurança → Microfone → will-ia."),
        ("Notificações", "Servidor e app notificam em nome do will-ia.app; o sistema não expõe o estado, por isso o botão de teste."),
        ("Arquivos e Pastas", "Pastas como Documentos, Mesa, Downloads e iCloud exigem permissão por app. O servidor (e os processos claude filhos dele) é quem acessa a base de conhecimento; se negado, o log mostra \"Operation not permitted\"."),
        ("Rede local", "Só aparece se o servidor escutar fora do loopback e falar com outros IPs da rede; não há como pedir antes."),
        ("Acessibilidade", "Não é necessária: o atalho global usa RegisterEventHotKey, que dispensa Monitoramento de entrada."),
    ],
    code: "",
    links: &[],
};

pub const GOOGLE_GMAIL: Help = Help {
    title: "Gmail",
    intro: "Ferramentas gmail_* para os agentes: listar e buscar (com a sintaxe de busca do Gmail), ler, e, se permitido, arquivar/marcar/rotular e enviar. E-mails recebidos nunca viram instrução: são dados não confiáveis que o agente lê quando você pede.",
    steps: &[],
    notes: &[
        ("Contas", "Nenhuma marcada = todas as contas conectadas com escopo de Gmail. Marque para restringir."),
        ("Arquivado e não lida", "Arquivado = sem o label INBOX; não lida = com o label UNREAD. As ferramentas já devolvem esses booleanos."),
        ("Modificar", "gmail_modify (arquivar, marcar lida, rotular) exige o escopo \"Gmail (arquivar/marcar/rotular)\" e nasce em \"confirmar\"."),
        ("Enviar", "gmail_send só aceita destinatários que já apareceram em e-mails lidos ou enviados (allowlist implícita) e sempre pede confirmação com o resumo gerado pelo serviço."),
    ],
    code: "",
    links: &[("Gmail: operadores de busca", "https://support.google.com/mail/answer/7190")],
};

// ---------- Agendamentos ----------

pub const SCHEDULER: Help = Help {
    title: "Agendamentos",
    intro: "Lembretes e tarefas programados: na hora marcada o serviço entrega a mensagem na thread dona como se você a tivesse escrito, com um cabeçalho que avisa o agente de que é um disparo. Podem ser criados aqui ou pelo próprio agente, durante uma conversa, com as ferramentas schedule_*.",
    steps: &[
        ("Pelo agente", "Diga na conversa algo como \"todo dia às 8h me mande o resumo da agenda\" ou \"amanhã às 15h me lembre de ligar para o dentista\". O agente chama schedule_create na própria thread; schedule_list, schedule_update e schedule_delete ajustam ou cancelam."),
        ("Pela UI", "Clique em Novo agendamento: nome, mensagem (o que o agente deve fazer ao disparar), recorrente (cron) ou único (data e hora), fuso opcional e a thread dona."),
        ("Cron de 5 campos", "minuto hora dia-do-mês mês dia-da-semana. Exemplos: 0 8 * * * (todo dia às 08:00); 30 18 * * 1-5 (dias úteis às 18:30); 0 9 1 * * (dia 1 de cada mês às 09:00); */15 9-17 * * * (a cada 15 min das 9h às 17h). A prévia ao lado do campo mostra a descrição e as próximas ocorrências."),
        ("Único", "Data e hora no formato AAAA-MM-DDTHH:MM, no fuso do agendamento (ou ISO-8601 com fuso). Depois de disparar, o agendamento fica pausado com o motivo \"concluído\"."),
    ],
    notes: &[
        ("Disparos perdidos são ignorados", "Se o serviço estava desligado na hora, o disparo não é recuperado: o próximo é calculado a partir de agora. Para o que já passou, use Disparar agora."),
        ("Thread apagada", "O agendamento guarda a origem e a sessão do Claude da thread. Se ela for apagada, o disparo recria a thread com a mesma sessão (--resume) no mesmo canal/tópico."),
        ("Pausar e retomar", "O interruptor do cartão pausa sem apagar. O scheduler também pausa sozinho um único já concluído ou um cron sem próxima ocorrência; o motivo aparece em amarelo."),
        ("Limites", "Em Configurações › Agendamentos: número máximo de agendamentos ativos e intervalo mínimo entre disparos do mesmo cron (* * * * * é rejeitado)."),
    ],
    code: "",
    links: &[("Sintaxe cron (crontab.guru)", "https://crontab.guru/")],
};

// ---------- Browser CDP ----------

pub const BROWSER: Help = Help {
    title: "Browser CDP",
    intro: "O will-ia expõe aos agentes um proxy local do Chrome DevTools Protocol. O agente usa o CLI agent-browser, que fala com o proxy em 127.0.0.1:<porta do proxy>; o proxy repassa tudo ao navegador real (por padrão 127.0.0.1:9222) e, se ninguém responde lá, sobe o navegador sozinho. Você não configura nada no agente: cada processo do claude já recebe a variável AGENT_BROWSER_CDP apontando para o proxy.",
    steps: &[
        ("Instalar o agent-browser", "Precisa de Node.js 18+. Em Instalação, clique em \"Instalar tudo\": roda npm i -g agent-browser e, ao terminar, copia a skill para a base de conhecimento. Se não tiver Chrome, use \"Baixar Chromium do agent-browser\"."),
        ("Skill na base de conhecimento", "A skill fica em <kb>/.claude/skills/agent-browser/SKILL.md e ensina o agente a usar o CLI (abrir páginas, clicar, preencher, capturar tela). Sem ela o agente até consegue usar o CLI, mas não sabe que ele existe."),
        ("Testar", "Em Testes: \"Testar navegador\" consulta /json/version direto no navegador; \"Abrir aba de teste pelo proxy\" passa pelo proxy e valida a reescrita das URLs e o fallback de lançamento; \"Testar agent-browser de ponta a ponta\" roda o CLI como o agente faria."),
        ("Acompanhar", "Em Tráfego CDP você vê, ao vivo, cada comando que o agente manda (→) e cada resposta ou evento que o navegador devolve (←). Clique numa linha para ver o conteúdo inteiro."),
    ],
    notes: &[
        ("Portas 9223 → 9222", "O proxy escuta em 9223 (porta do proxy) e fala com o navegador em 9222 (porta do navegador, o --remote-debugging-port do Chrome). Mude as duas nas configurações se já estiverem em uso."),
        ("Perfil dedicado (Chrome 136+)", "O Chrome ignora --remote-debugging-port no perfil padrão. Por isso o lançamento automático usa sempre um perfil próprio (<data_dir>/browser-profile, ou a Pasta do perfil que você escolher). Logins feitos nesse perfil ficam guardados entre execuções."),
        ("Usar um navegador já aberto", "Abra o seu Chrome com --remote-debugging-port=9222 --user-data-dir=<pasta> e o proxy o encontra; nesse caso \"Fechar navegador\" não se aplica (só fechamos o que nós lançamos)."),
        ("Headless", "Ligado, o navegador roda sem janela. Desligado, você vê o agente navegar; útil para depurar e para logins com 2FA."),
        ("Ferramenta permitida", "\"Liberar o CLI agent-browser\" adiciona Bash(agent-browser:*) às ferramentas permitidas do claude, para o agente rodar o CLI sem pedir permissão em modo acceptEdits."),
    ],
    code: "npm i -g agent-browser\nagent-browser install        # Chromium do agent-browser (opcional)\nagent-browser --version",
    links: &[
        ("agent-browser (GitHub)", "https://github.com/vercel-labs/agent-browser"),
        ("Chrome: remote debugging port e perfil", "https://developer.chrome.com/blog/remote-debugging-port"),
    ],
};

// ---------- Usuários ----------

pub const USERS: Help = Help {
    title: "Usuários",
    intro: "Contas de acesso a este servidor will-ia. Cada pessoa entra com usuário e senha no app; o login identifica quem escreveu cada mensagem, quem aprovou o quê e quem gastou quanto, e permite revogar o acesso de alguém sem mexer nos demais. As threads são compartilhadas: todo mundo vê as mesmas conversas.",
    steps: &[
        ("Primeiro acesso", "Na primeira subida o servidor cria o usuário admin com uma senha aleatória, impressa no log e em will-ia info. Ao entrar com ela, o app exige a troca por uma senha sua."),
        ("Criar uma conta", "Novo usuário: nome de usuário curto (vira o autor nas threads), nome, e-mail opcional, senha inicial e papel. Entregue a senha inicial por um canal seguro; com \"Exigir troca no primeiro acesso\" a pessoa define a própria senha ao entrar."),
        ("Papéis", "Administrador mexe em configurações, segredos, tokens do motor, políticas, MCPs externos e usuários. Membro conversa, aprova, agenda, usa a voz e vê logs e custos; as páginas de configuração nem aparecem no menu dele."),
        ("Revogar acesso", "Desativar encerra as sessões abertas e impede novos logins; o histórico da pessoa fica. Reativar devolve o acesso. Redefinir senha, em Editar, serve para quem esqueceu a sua."),
    ],
    notes: &[
        ("Token local da API", "O app na mesma máquina do servidor pode entrar com o token de will-ia info, sem conta: aparece como \"local\" e é administrador. Scripts e automações usam o mesmo token."),
        ("Sessões", "Cada login no app gera um token de sessão guardado no perfil de conexão. Sair da conta (página Conexão) invalida esse token no servidor."),
        ("Não dá para desativar a si mesmo", "Para não trancar o servidor sem administrador. Crie outro administrador antes, se precisar."),
    ],
    code: "will-ia info",
    links: &[],
};
