# ADR-0019 — Separação cliente/servidor: app egui como cliente HTTP/WS de um servidor residente

**Status:** Aceita · **Data:** 2026-10-28 · Complementa a ADR-0016 (UI nativa egui) e a ADR-0010 (app de sessão do usuário)

## Contexto

Hoje `will-ia` é um binário único: a janela egui e o serviço vivem no mesmo processo e a UI chama os handlers da API local **em processo**. O usuário quer (1) um servidor sempre ativo, possivelmente Linux sem tela, para uma equipe inteira conversar com o mesmo agente sobre a mesma base de conhecimento; (2) instâncias pessoais; (3) o mesmo app de desktop alternando entre eles. Exigência: o comportamento para quem usa não muda em nada.

Alternativas avaliadas:

| Opção | Veredito |
|---|---|
| gRPC (tonic) entre app e servidor | Rejeitado agora: exigiria redefinir ~80 operações em protobuf e HTTP/2 fim a fim atrás de proxies; a API HTTP/WS já existe (`docs/22`), é usada pela própria UI e atende chat, eventos e áudio. Pode ser adicionado depois como gateway. |
| Manter o modo "tudo em um processo" além do modo cliente | Rejeitado: dois caminhos de código na UI, divergência inevitável. O app é sempre cliente, inclusive em loopback. |
| Identidade única compartilhada para a equipe | Rejeitado: sem rastro de quem pediu/aprovou/gastou e sem como revogar uma pessoa. |
| Threads privadas por usuário | Adiado: o objetivo é um agente da equipe; threads são compartilhadas, mas cada turno/aprovação/custo tem autor. |
| Voz só no servidor | Rejeitado: perderia a voz nos apps remotos. Áudio trafega pelo WS que já existe para o headless. |

## Decisão

1. **Dois binários**: `will-ia` (servidor residente: serviço, API, bandeja quando há desktop, headless quando não há) e `will-ia-app` (egui, cliente HTTP/2 + WebSocket, perfis de conexão, one-shot).
2. **Contrato = API local de hoje** (`docs/22`) mais rotas novas: autenticação/usuários, upload de anexos, leitura de arquivos da KB, navegação de pastas do servidor. Um crate `will-client` tipado encapsula o acesso; a UI não importa `will-server`.
3. **Contas por pessoa, threads compartilhadas**: usuário + senha (argon2), token de sessão, papéis `admin`/`membro`, `user_id` como autor em turnos, aprovações, custos, logs e agendamentos. Primeiro `admin` criado na primeira subida com senha impressa no log/`will-ia info`, troca obrigatória.
4. **Rede**: `bind` loopback por padrão; TLS opcional embutido (rustls, certificado próprio ou autoassinado) com fixação de impressão digital no app; rate limit em login/upload.
5. **Voz**: microfone/alto-falante no app, sessão OpenAI no servidor, áudio pelo WS; **uma sessão por usuário** com teto configurável e custo por usuário.
6. **Responsabilidades de desktop**: bandeja e autostart são do servidor (quando há desktop); atalho PTT, diálogos de arquivo, toasts e notificações nativas são do app. Notificações do serviço chegam aos apps pelo barramento de eventos.
7. **Servidor sem tela**: Chrome do Browser CDP headless forçado; segredos em arquivo quando não há keyring; instalação como serviço de usuário/sistema documentada.

## Consequências

- Um só caminho de código para a UI; o modo loopback é o mesmo do remoto, então "funciona local" implica "funciona remoto" (exceto latência).
- A UI ganha estados de rede (carregando, desconectado, reconectando) e perde diálogos nativos de pasta para caminhos do servidor.
- Banco ganha `users`/`sessions` e `user_id` em tabelas existentes (migração aditiva, colunas opcionais).
- O `RealtimeManager` deixa de ser singleton.
- Mais superfície de segurança; mitigada por papéis, TLS opcional, loopback por padrão e rate limit.
- Pacotes e CI passam a produzir dois binários por SO. Plano detalhado e impactos em `docs/26`.
