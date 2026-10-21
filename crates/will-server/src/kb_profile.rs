//! Perfil da pessoa na base de conhecimento (`knowledge/pessoa/`): uma árvore de
//! pastas e arquivos Markdown com nomes longos e descritivos (kebab-case), cada um
//! com lacunas `__preencher__`. O cérebro preenche as lacunas organicamente, ao
//! longo das conversas, sem fazer questionário (regras no `CLAUDE.md` da KB).
//!
//! Gerado por dados (não por arquivos soltos) para manter a convenção uniforme e
//! testável: todo caminho é kebab-case, único, e todo arquivo tem lacunas.

/// Marcador de lacuna a preencher.
pub const PLACEHOLDER: &str = "__preencher__";
/// Raiz do perfil dentro da KB.
pub const ROOT: &str = "knowledge/pessoa";

struct Area {
    dir: &'static str,
    title: &'static str,
    files: &'static [FileSpec],
}

struct FileSpec {
    name: &'static str,
    title: &'static str,
    tags: &'static [&'static str],
    sections: &'static [(&'static str, &'static [&'static str])],
}

const AREAS: &[Area] = &[
    Area {
        dir: "01-identidade-e-dados-pessoais",
        title: "Identidade e dados pessoais",
        files: &[
            FileSpec {
                name: "dados-pessoais-nome-nascimento-e-nacionalidade.md",
                title: "Dados pessoais: nome, nascimento e nacionalidade",
                tags: &["identidade"],
                sections: &[
                    (
                        "Identificação",
                        &[
                            "Nome completo",
                            "Como prefere ser chamado(a)",
                            "Pronomes",
                            "Data de nascimento",
                            "Cidade e país de nascimento",
                            "Nacionalidade",
                            "Estado civil",
                            "Idiomas que fala",
                        ],
                    ),
                    (
                        "Assinatura e apresentação",
                        &[
                            "Como se apresenta profissionalmente",
                            "Assinatura padrão de e-mails",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "documentos-cpf-rg-passaporte-e-cnh.md",
                title: "Documentos: CPF, RG, passaporte e CNH",
                tags: &["identidade", "documentos", "sensivel"],
                sections: &[(
                    "Documentos (só se a pessoa fornecer)",
                    &[
                        "CPF",
                        "RG e órgão emissor",
                        "Passaporte, validade e país",
                        "CNH, categoria e validade",
                        "Título de eleitor e zona",
                        "Onde guarda os documentos físicos",
                    ],
                )],
            },
            FileSpec {
                name: "contatos-telefones-emails-e-redes-sociais.md",
                title: "Contatos: telefones, e-mails e redes sociais",
                tags: &["identidade", "contatos"],
                sections: &[
                    (
                        "Telefones",
                        &[
                            "Celular principal",
                            "Celular secundário ou de trabalho",
                            "Telefone fixo",
                        ],
                    ),
                    (
                        "E-mails",
                        &[
                            "E-mail pessoal principal",
                            "E-mail de trabalho",
                            "E-mails secundários",
                        ],
                    ),
                    (
                        "Redes e perfis",
                        &[
                            "LinkedIn",
                            "Instagram",
                            "X / Twitter",
                            "GitHub",
                            "Site pessoal ou blog",
                            "Outros perfis",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "preferencias-de-comunicacao-e-tratamento.md",
                title: "Preferências de comunicação e tratamento",
                tags: &["identidade", "preferencias"],
                sections: &[(
                    "Como gosta de ser atendido(a)",
                    &[
                        "Tom preferido (formal, informal, direto)",
                        "Tamanho de resposta preferido",
                        "Horários em que não quer ser incomodado(a)",
                        "Canal preferido para avisos urgentes",
                        "Canal preferido para coisas não urgentes",
                        "Assuntos que não gosta de discutir",
                        "Palavras ou expressões que usa com frequência",
                    ],
                )],
            },
        ],
    },
    Area {
        dir: "02-familia-e-relacionamentos",
        title: "Família e relacionamentos",
        files: &[
            FileSpec {
                name: "familia-conjuge-e-filhos.md",
                title: "Família: cônjuge e filhos",
                tags: &["familia"],
                sections: &[
                    (
                        "Cônjuge ou parceiro(a)",
                        &[
                            "Nome",
                            "Como se conheceram e desde quando",
                            "Data de nascimento",
                            "Profissão",
                            "Telefone",
                            "Preferências e gostos marcantes",
                        ],
                    ),
                    (
                        "Filhos (um bloco por filho)",
                        &[
                            "Nome",
                            "Data de nascimento",
                            "Escola ou ocupação",
                            "Atividades, gostos e alergias",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "familia-pais-irmaos-e-parentes-proximos.md",
                title: "Família: pais, irmãos e parentes próximos",
                tags: &["familia"],
                sections: &[
                    (
                        "Pais",
                        &[
                            "Mãe: nome, nascimento, onde mora, telefone",
                            "Pai: nome, nascimento, onde mora, telefone",
                        ],
                    ),
                    ("Irmãos", &["Nome, nascimento, onde mora, telefone"]),
                    (
                        "Outros parentes relevantes",
                        &["Avós, tios, primos, sogros: nome e relação"],
                    ),
                ],
            },
            FileSpec {
                name: "amigos-e-circulo-proximo.md",
                title: "Amigos e círculo próximo",
                tags: &["relacionamentos"],
                sections: &[(
                    "Amigos próximos (um bloco por pessoa)",
                    &[
                        "Nome",
                        "Como se conheceram",
                        "Aniversário",
                        "Telefone ou contato",
                        "Assuntos em comum",
                    ],
                )],
            },
            FileSpec {
                name: "datas-importantes-aniversarios-e-comemoracoes.md",
                title: "Datas importantes: aniversários e comemorações",
                tags: &["familia", "datas"],
                sections: &[
                    (
                        "Datas fixas",
                        &[
                            "Aniversário da pessoa",
                            "Aniversário do cônjuge",
                            "Aniversário de casamento ou namoro",
                            "Aniversários dos filhos",
                            "Aniversários dos pais e irmãos",
                            "Outras datas que faz questão de lembrar",
                        ],
                    ),
                    (
                        "Como gosta de comemorar",
                        &[
                            "Presentes que costuma dar",
                            "Lugares preferidos para comemorar",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "animais-de-estimacao.md",
                title: "Animais de estimação",
                tags: &["familia", "pets"],
                sections: &[(
                    "Por animal",
                    &[
                        "Nome e espécie/raça",
                        "Data de nascimento ou idade",
                        "Veterinário e telefone",
                        "Vacinas, remédios e ração",
                        "Pet shop ou hotel de confiança",
                    ],
                )],
            },
        ],
    },
    Area {
        dir: "03-saude-e-bem-estar",
        title: "Saúde e bem-estar",
        files: &[
            FileSpec {
                name: "saude-condicoes-alergias-e-medicamentos.md",
                title: "Saúde: condições, alergias e medicamentos",
                tags: &["saude", "sensivel"],
                sections: &[(
                    "Informações críticas",
                    &[
                        "Tipo sanguíneo",
                        "Alergias (medicamentos, alimentos, outras)",
                        "Condições crônicas",
                        "Medicamentos de uso contínuo, dose e horário",
                        "Cirurgias e internações anteriores",
                        "Restrições alimentares",
                    ],
                )],
            },
            FileSpec {
                name: "saude-medicos-plano-de-saude-e-exames.md",
                title: "Saúde: médicos, plano de saúde e exames",
                tags: &["saude"],
                sections: &[
                    (
                        "Plano de saúde",
                        &[
                            "Operadora e plano",
                            "Número da carteirinha (só se a pessoa fornecer)",
                            "Hospital de referência",
                        ],
                    ),
                    (
                        "Médicos e profissionais (um bloco por especialidade)",
                        &[
                            "Clínico geral: nome, telefone, endereço",
                            "Dentista",
                            "Outros especialistas",
                            "Periodicidade de check-up e último realizado",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "rotina-sono-alimentacao-e-exercicios.md",
                title: "Rotina: sono, alimentação e exercícios",
                tags: &["saude", "rotina"],
                sections: &[
                    (
                        "Sono",
                        &[
                            "Horário em que costuma dormir e acordar",
                            "Qualidade do sono e o que atrapalha",
                        ],
                    ),
                    (
                        "Alimentação",
                        &[
                            "Dieta ou objetivo atual",
                            "Refeições habituais",
                            "O que evita",
                        ],
                    ),
                    (
                        "Atividade física",
                        &[
                            "Modalidades e frequência",
                            "Academia, personal ou grupo",
                            "Metas atuais",
                        ],
                    ),
                ],
            },
        ],
    },
    Area {
        dir: "04-trabalho-e-carreira",
        title: "Trabalho e carreira",
        files: &[
            FileSpec {
                name: "trabalho-atual-empresa-cargo-e-equipe.md",
                title: "Trabalho atual: empresa, cargo e equipe",
                tags: &["trabalho"],
                sections: &[
                    (
                        "Empresa e cargo",
                        &[
                            "Empresa",
                            "Cargo e área",
                            "Desde quando",
                            "Regime (CLT, PJ, sócio) e modelo (presencial, híbrido, remoto)",
                            "Endereço do trabalho",
                            "Horário de trabalho",
                        ],
                    ),
                    (
                        "Pessoas",
                        &[
                            "Gestor(a) direto(a)",
                            "Equipe (nomes e papéis)",
                            "Pares e parceiros frequentes",
                            "Clientes principais",
                        ],
                    ),
                    (
                        "Ferramentas e processos",
                        &[
                            "Ferramentas que usa no trabalho",
                            "Reuniões recorrentes e horários",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "carreira-historico-formacao-e-certificacoes.md",
                title: "Carreira: histórico, formação e certificações",
                tags: &["trabalho", "formacao"],
                sections: &[
                    (
                        "Formação",
                        &[
                            "Graduação: curso, instituição, ano",
                            "Pós-graduação ou cursos longos",
                            "Certificações e validade",
                        ],
                    ),
                    (
                        "Histórico profissional (do mais recente ao mais antigo)",
                        &["Empresa, cargo, período, principais entregas"],
                    ),
                    (
                        "Competências",
                        &[
                            "Habilidades técnicas",
                            "Habilidades de que mais se orgulha",
                            "O que quer aprender",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "projetos-em-andamento-e-metas-profissionais.md",
                title: "Projetos em andamento e metas profissionais",
                tags: &["trabalho", "projetos"],
                sections: &[
                    (
                        "Projetos ativos (um bloco por projeto)",
                        &[
                            "Nome e objetivo",
                            "Prazo e status",
                            "Pessoas envolvidas",
                            "Próximo passo",
                        ],
                    ),
                    (
                        "Metas",
                        &["Metas deste ano", "Meta de carreira em 3 a 5 anos"],
                    ),
                ],
            },
        ],
    },
    Area {
        dir: "05-financas-e-patrimonio",
        title: "Finanças e patrimônio",
        files: &[
            FileSpec {
                name: "financas-bancos-cartoes-e-contas-recorrentes.md",
                title: "Finanças: bancos, cartões e contas recorrentes",
                tags: &["financas", "sensivel"],
                sections: &[
                    (
                        "Bancos e cartões (sem números completos, senhas ou códigos)",
                        &[
                            "Banco principal e tipo de conta",
                            "Outros bancos e corretoras",
                            "Cartões (bandeira, final e dia de vencimento)",
                        ],
                    ),
                    (
                        "Contas recorrentes",
                        &[
                            "Aluguel ou financiamento: valor e dia",
                            "Energia, água, gás, internet: dia de vencimento",
                            "Outras contas fixas",
                        ],
                    ),
                    (
                        "Organização",
                        &[
                            "Dia em que recebe",
                            "Como controla o orçamento (planilha, app)",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "patrimonio-imoveis-veiculos-e-investimentos.md",
                title: "Patrimônio: imóveis, veículos e investimentos",
                tags: &["financas", "patrimonio", "sensivel"],
                sections: &[
                    (
                        "Imóveis",
                        &["Endereço, situação (próprio, financiado, alugado), IPTU"],
                    ),
                    (
                        "Veículos",
                        &["Modelo, ano, placa, seguro e vencimento, IPVA"],
                    ),
                    (
                        "Investimentos (visão geral, sem saldos se não quiser)",
                        &["Onde investe e perfil", "Objetivo dos investimentos"],
                    ),
                ],
            },
            FileSpec {
                name: "assinaturas-e-servicos-contratados.md",
                title: "Assinaturas e serviços contratados",
                tags: &["financas", "assinaturas"],
                sections: &[(
                    "Assinaturas (um bloco por serviço)",
                    &[
                        "Serviço, plano, valor, dia de cobrança, forma de pagamento",
                        "Streaming, música, nuvem, software, academia, clubes",
                    ],
                )],
            },
        ],
    },
    Area {
        dir: "06-casa-e-rotina",
        title: "Casa e rotina",
        files: &[
            FileSpec {
                name: "enderecos-residencia-trabalho-e-outros.md",
                title: "Endereços: residência, trabalho e outros",
                tags: &["casa", "enderecos"],
                sections: &[
                    (
                        "Residência",
                        &[
                            "Endereço completo com CEP",
                            "Referências para entregas",
                            "Desde quando mora",
                            "Nome do condomínio ou portaria e telefone",
                        ],
                    ),
                    (
                        "Outros endereços",
                        &[
                            "Trabalho",
                            "Casa dos pais",
                            "Casa de praia, sítio ou segundo imóvel",
                            "Endereço para entregas quando não está em casa",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "casa-moradia-manutencao-e-prestadores.md",
                title: "Casa: moradia, manutenção e prestadores",
                tags: &["casa", "manutencao"],
                sections: &[
                    (
                        "Moradia",
                        &[
                            "Tipo (casa, apartamento), tamanho, cômodos",
                            "Internet: operadora e plano",
                            "Energia, água e gás: fornecedores",
                        ],
                    ),
                    (
                        "Prestadores de confiança",
                        &[
                            "Eletricista",
                            "Encanador",
                            "Diarista ou empregada doméstica: nome, dias e pagamento",
                            "Síndico ou administradora",
                            "Chaveiro",
                        ],
                    ),
                    (
                        "Manutenções periódicas",
                        &["O que, quando foi a última e quando é a próxima"],
                    ),
                ],
            },
            FileSpec {
                name: "rotina-semanal-horarios-e-compromissos-fixos.md",
                title: "Rotina semanal: horários e compromissos fixos",
                tags: &["rotina"],
                sections: &[
                    (
                        "Dia típico",
                        &[
                            "Horário em que acorda e sai de casa",
                            "Horário de almoço",
                            "Horário em que encerra o trabalho",
                            "Horário em que dorme",
                        ],
                    ),
                    (
                        "Compromissos fixos por dia da semana",
                        &[
                            "Segunda", "Terça", "Quarta", "Quinta", "Sexta", "Sábado", "Domingo",
                        ],
                    ),
                    (
                        "Deslocamentos",
                        &[
                            "Como vai ao trabalho e quanto tempo leva",
                            "Trajetos frequentes",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "veiculos-placa-seguro-e-revisoes.md",
                title: "Veículos: placa, seguro e revisões",
                tags: &["casa", "veiculos"],
                sections: &[(
                    "Por veículo",
                    &[
                        "Modelo, cor, ano e placa",
                        "Seguradora, apólice e telefone de assistência",
                        "Oficina ou concessionária de confiança",
                        "Última revisão e quilometragem",
                        "Próxima revisão, IPVA e licenciamento",
                    ],
                )],
            },
        ],
    },
    Area {
        dir: "07-preferencias-e-gostos",
        title: "Preferências e gostos",
        files: &[
            FileSpec {
                name: "gostos-comida-restaurantes-e-bebidas.md",
                title: "Gostos: comida, restaurantes e bebidas",
                tags: &["preferencias", "comida"],
                sections: &[
                    (
                        "Comida",
                        &[
                            "Pratos favoritos",
                            "Comidas que não come",
                            "Cafés e bebidas do dia a dia",
                            "Bebidas alcoólicas preferidas (ou não bebe)",
                        ],
                    ),
                    (
                        "Lugares",
                        &[
                            "Restaurantes favoritos e o que pede",
                            "Delivery habitual",
                            "Padaria, mercado e feira de costume",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "gostos-musica-filmes-series-livros-e-jogos.md",
                title: "Gostos: música, filmes, séries, livros e jogos",
                tags: &["preferencias", "cultura"],
                sections: &[
                    (
                        "Música",
                        &[
                            "Gêneros e artistas preferidos",
                            "O que ouve para trabalhar",
                            "Shows que gostaria de ver",
                        ],
                    ),
                    (
                        "Filmes e séries",
                        &[
                            "Gêneros e títulos favoritos",
                            "Está assistindo agora",
                            "Lista para ver",
                        ],
                    ),
                    (
                        "Livros e leitura",
                        &[
                            "Gêneros e autores favoritos",
                            "Está lendo agora",
                            "Lista para ler",
                        ],
                    ),
                    (
                        "Jogos e passatempos",
                        &["Jogos, esportes que acompanha, times"],
                    ),
                ],
            },
            FileSpec {
                name: "viagens-destinos-preferidos-e-historico.md",
                title: "Viagens: destinos preferidos e histórico",
                tags: &["preferencias", "viagens"],
                sections: &[
                    (
                        "Como gosta de viajar",
                        &[
                            "Estilo (praia, montanha, cidade, aventura)",
                            "Companhia habitual",
                            "Companhia aérea e programa de milhas",
                            "Assento e hotel preferidos",
                            "Orçamento típico",
                        ],
                    ),
                    (
                        "Histórico e desejos",
                        &[
                            "Viagens marcantes",
                            "Destinos que quer conhecer",
                            "Próxima viagem planejada",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "compras-tamanhos-marcas-e-preferencias.md",
                title: "Compras: tamanhos, marcas e preferências",
                tags: &["preferencias", "compras"],
                sections: &[
                    (
                        "Tamanhos",
                        &["Camisa, calça, sapato", "Anel, pulso (relógio)"],
                    ),
                    (
                        "Marcas e lojas",
                        &[
                            "Roupas e calçados",
                            "Eletrônicos",
                            "Lojas on-line habituais",
                        ],
                    ),
                    (
                        "Presentes",
                        &[
                            "O que gosta de ganhar",
                            "O que não gosta de ganhar",
                            "Ideias de presente já mencionadas",
                        ],
                    ),
                ],
            },
        ],
    },
    Area {
        dir: "08-tecnologia-e-ferramentas",
        title: "Tecnologia e ferramentas",
        files: &[
            FileSpec {
                name: "dispositivos-computadores-celulares-e-contas.md",
                title: "Dispositivos: computadores, celulares e contas",
                tags: &["tecnologia"],
                sections: &[
                    (
                        "Dispositivos",
                        &[
                            "Computador principal: modelo e sistema",
                            "Celular: modelo e sistema",
                            "Tablet, relógio e outros",
                            "Backups: onde e com que frequência",
                        ],
                    ),
                    (
                        "Contas e ecossistema (sem senhas)",
                        &[
                            "Apple ID, conta Google, Microsoft: qual e-mail",
                            "Gerenciador de senhas que usa",
                            "Autenticação em dois fatores: app usado",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "ferramentas-e-servicos-que-usa-no-dia-a-dia.md",
                title: "Ferramentas e serviços que usa no dia a dia",
                tags: &["tecnologia", "ferramentas"],
                sections: &[
                    (
                        "Produtividade",
                        &[
                            "Agenda e e-mail",
                            "Notas e documentos",
                            "Tarefas e projetos",
                            "Mensageria (WhatsApp, Telegram, Slack)",
                        ],
                    ),
                    (
                        "Desenvolvimento e trabalho",
                        &[
                            "Editor, terminal, linguagens",
                            "Repositórios e nuvem",
                            "Serviços pagos de IA",
                        ],
                    ),
                    (
                        "Automação",
                        &["O que já automatizou", "O que gostaria de automatizar"],
                    ),
                ],
            },
        ],
    },
    Area {
        dir: "09-objetivos-e-projetos-pessoais",
        title: "Objetivos e projetos pessoais",
        files: &[
            FileSpec {
                name: "objetivos-de-curto-medio-e-longo-prazo.md",
                title: "Objetivos de curto, médio e longo prazo",
                tags: &["objetivos"],
                sections: &[
                    (
                        "Curto prazo (até 3 meses)",
                        &["Objetivo, por que importa, próximo passo"],
                    ),
                    (
                        "Médio prazo (até 1 ano)",
                        &["Objetivo, por que importa, próximo passo"],
                    ),
                    (
                        "Longo prazo (mais de 1 ano)",
                        &["Objetivo, por que importa, próximo passo"],
                    ),
                    (
                        "Valores e prioridades",
                        &["O que é inegociável", "O que quer mais na vida agora"],
                    ),
                ],
            },
            FileSpec {
                name: "projetos-pessoais-hobbies-e-estudos.md",
                title: "Projetos pessoais, hobbies e estudos",
                tags: &["objetivos", "hobbies"],
                sections: &[
                    (
                        "Hobbies",
                        &[
                            "O que faz por prazer e com que frequência",
                            "Equipamentos e materiais",
                        ],
                    ),
                    (
                        "Estudos",
                        &[
                            "O que está estudando agora",
                            "Cursos ou livros em andamento",
                            "Idiomas e nível",
                        ],
                    ),
                    (
                        "Projetos pessoais (um bloco por projeto)",
                        &["Nome, objetivo, status, próximo passo"],
                    ),
                ],
            },
        ],
    },
    Area {
        dir: "10-contatos-uteis-e-emergencias",
        title: "Contatos úteis e emergências",
        files: &[
            FileSpec {
                name: "contatos-de-emergencia.md",
                title: "Contatos de emergência",
                tags: &["contatos", "emergencia"],
                sections: &[
                    (
                        "Quem avisar em emergência (em ordem)",
                        &[
                            "Nome, relação, telefone",
                            "Segundo contato",
                            "Terceiro contato",
                        ],
                    ),
                    (
                        "Serviços",
                        &[
                            "Hospital ou pronto-socorro de referência",
                            "Médico de confiança",
                            "Seguro ou assistência (carro, casa, viagem)",
                        ],
                    ),
                ],
            },
            FileSpec {
                name: "contatos-profissionais-servicos-e-fornecedores.md",
                title: "Contatos profissionais, serviços e fornecedores",
                tags: &["contatos", "fornecedores"],
                sections: &[
                    (
                        "Profissionais",
                        &[
                            "Contador(a)",
                            "Advogado(a)",
                            "Corretor(a) de seguros",
                            "Gerente do banco",
                            "Despachante",
                        ],
                    ),
                    (
                        "Serviços pessoais",
                        &[
                            "Cabeleireiro(a) ou barbeiro(a)",
                            "Personal, nutricionista, terapeuta",
                            "Costureira, sapateiro, lavanderia",
                        ],
                    ),
                ],
            },
        ],
    },
];

fn render_file(f: &FileSpec) -> String {
    let n_fields: usize = f.sections.iter().map(|(_, fields)| fields.len()).sum();
    let mut out = String::new();
    out.push_str("---\n");
    // título entre aspas: contém ":" (inválido em YAML sem aspas)
    out.push_str(&format!("title: \"{}\"\n", f.title.replace('"', "'")));
    out.push_str(&format!("updated: {PLACEHOLDER}\n"));
    let mut tags = vec!["pessoa"];
    tags.extend_from_slice(f.tags);
    out.push_str(&format!("tags: [{}]\n", tags.join(", ")));
    out.push_str(&format!("completude: 0/{n_fields}\n"));
    out.push_str("---\n\n");
    out.push_str(&format!("# {}\n\n", f.title));
    out.push_str(&format!(
        "> Lacunas marcadas com `{PLACEHOLDER}`. Preencha quando a informação surgir naturalmente; \
         anote a origem e a data ao lado do valor, ex.: `(conversa por voz, 2026-10-21)`. \
         Não invente valores nem apague linhas que ainda não sabe.\n\n"
    ));
    for (section, fields) in f.sections {
        out.push_str(&format!("## {section}\n\n"));
        for field in fields.iter() {
            out.push_str(&format!("- {field}: {PLACEHOLDER}\n"));
        }
        out.push('\n');
    }
    out.push_str(&format!("## Notas e contexto\n\n{PLACEHOLDER}\n\n"));
    out.push_str("## Histórico de atualizações\n\n- (data) — o que mudou e de onde veio\n");
    out
}

fn render_readme() -> String {
    let mut out = String::new();
    out.push_str("# knowledge/pessoa — perfil de quem você atende\n\n");
    out.push_str(&format!(
        "Tudo sobre a vida da pessoa, organizado em pastas e arquivos com **nomes longos e \
         descritivos** (kebab-case): só de listar a pasta você sabe o que há dentro. Cada arquivo \
         tem lacunas `{PLACEHOLDER}` para serem preenchidas **organicamente**, ao longo das conversas.\n\n"
    ));
    out.push_str("## Convenções\n\n");
    out.push_str(&format!(
        "- Troque `{PLACEHOLDER}` pelo valor e anote a origem e a data entre parênteses.\n\
         - Não apague lacunas que ainda não conhece; não invente valores.\n\
         - Atualize `updated` e `completude` (preenchidos/total) no frontmatter e a seção \
         \"Histórico de atualizações\".\n\
         - Nunca registre senhas, PINs, números completos de cartão ou códigos de segurança. \
         Dados sensíveis (saúde, documentos, finanças) só quando a própria pessoa fornecer.\n\
         - Pode criar novos arquivos e pastas seguindo o mesmo padrão quando surgir um tema \
         novo (ex.: `11-filhos-escola-e-atividades/`), e registre aqui no índice.\n\
         - Para responder sobre a pessoa (preferências, família, endereços, rotinas), consulte \
         esta pasta antes de qualquer outra coisa.\n\n"
    ));
    out.push_str("## Como capturar sem ser chato\n\n");
    out.push_str(
        "- Nunca faça questionário. Preencha com o que a pessoa disser espontaneamente, com o que \
         aparecer em anexos, e-mails ou mensagens que ela pediu para você ler, e com o que você \
         inferir com segurança (marque inferências como tal).\n\
         - No máximo **uma** pergunta leve por conversa, e só quando encaixar no assunto: ao marcar \
         um médico, perguntar o plano; ao falar de um aniversário, confirmar a data. Se a pessoa não \
         responder, deixe para outra hora.\n\
         - Priorize o que destrava tarefas (endereços, contatos, horários, preferências) antes de \
         curiosidades.\n\n",
    );
    out.push_str("## Índice\n\n");
    for a in AREAS {
        out.push_str(&format!("### `{}/` — {}\n\n", a.dir, a.title));
        for f in a.files {
            out.push_str(&format!("- `{}` — {}\n", f.name, f.title));
        }
        out.push('\n');
    }
    out
}

/// Arquivos do perfil: caminho relativo à KB + conteúdo.
pub fn profile_files() -> Vec<(String, String)> {
    let mut out = vec![(format!("{ROOT}/README.md"), render_readme())];
    for a in AREAS {
        for f in a.files {
            out.push((format!("{ROOT}/{}/{}", a.dir, f.name), render_file(f)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_kebab(s: &str) -> bool {
        s.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.' || c == '/'
        }) && !s.contains("--")
    }

    #[test]
    fn paths_are_kebab_case_unique_and_descriptive() {
        let files = profile_files();
        assert!(files.len() > 25);
        let mut seen = std::collections::HashSet::new();
        for (p, _) in &files {
            assert!(p.starts_with(ROOT), "{p}");
            let rel = &p[ROOT.len() + 1..];
            assert!(
                is_kebab(&rel.replace("README.md", "readme.md")),
                "não é kebab-case: {p}"
            );
            assert!(seen.insert(p.clone()), "duplicado: {p}");
            let name = p.rsplit('/').next().unwrap_or("");
            assert!(
                name == "README.md" || name.len() >= 20,
                "nome pouco descritivo: {p}"
            );
        }
    }

    #[test]
    fn every_profile_file_has_placeholders_and_frontmatter() {
        for (p, c) in profile_files() {
            assert!(c.contains(PLACEHOLDER), "{p} sem lacunas");
            if !p.ends_with("README.md") {
                assert!(c.starts_with("---\ntitle: \""), "{p} sem frontmatter");
                assert!(c.contains("completude: 0/"), "{p} sem completude");
                assert!(
                    c.contains("## Histórico de atualizações"),
                    "{p} sem histórico"
                );
            }
        }
    }
}
