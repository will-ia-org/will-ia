# Política de Privacidade — will-ia

_Última atualização: 2026-10-29_

O **will-ia** é um assistente pessoal de IA de código aberto, que roda **na máquina do próprio usuário**
(servidor local + app desktop). Não existe um serviço central operado por nós: não há conta, não há
nuvem própria e não há banco de dados nosso com dados de usuários.

## Quais dados o app acessa

Quando o usuário conecta a própria conta Google (OAuth com um **cliente OAuth criado por ele mesmo**),
o app pode acessar, conforme os escopos autorizados:

| Escopo | Para que serve |
|---|---|
| `calendar.readonly`, `calendar.events` | ver agendas e eventos; criar/editar/excluir eventos (com aprovação do usuário) |
| `gmail.readonly` | ler e buscar e-mails |
| `gmail.modify` | arquivar, marcar como lido e aplicar labels (com aprovação do usuário) |
| `gmail.send` | enviar e-mails (somente com aprovação explícita do usuário) |

Também pode acessar WhatsApp (pareado por QR pelo próprio dono) e Telegram (bot do próprio usuário).

## Como os dados são usados

- Os dados são lidos **somente para executar o que o usuário pede** ao assistente (ex.: "o que tenho na
  agenda amanhã?", "resuma meus e-mails").
- Ações com efeito externo (enviar e-mail, criar/alterar evento, enviar mensagem) exigem **confirmação
  do usuário**.
- Os dados **não são vendidos, compartilhados nem usados para publicidade**, e **não são usados para
  treinar modelos de IA por nós**.

## Onde os dados ficam

- Tokens de acesso e configurações ficam **armazenados localmente**, na máquina do usuário.
- O conteúdo consultado (e-mails, eventos) é processado localmente e **enviado ao provedor de IA
  configurado pelo próprio usuário** (Claude Code / Anthropic, e OpenAI Realtime para voz) apenas na
  medida necessária para responder ao pedido. Esses provedores têm suas próprias políticas de privacidade.
- Nenhum dado é enviado a servidores dos autores do will-ia.

## Uso de dados de APIs do Google

O uso e a transferência, para qualquer outro app, de informações recebidas das APIs do Google seguem a
[Política de Dados de Usuário dos Serviços de API do Google](https://developers.google.com/terms/api-services-user-data-policy),
incluindo os requisitos de **Uso Limitado** (Limited Use).

## Retenção e exclusão

- O usuário pode **desconectar a conta Google** a qualquer momento no app, o que apaga os tokens locais.
- Também é possível revogar o acesso em <https://myaccount.google.com/permissions>.
- Como os dados ficam locais, apagar a pasta de dados do app remove tudo.

## Contato

Dúvidas sobre privacidade: **willguitaradmfar@gmail.com**

---

# Privacy Policy — will-ia (English summary)

_Last updated: 2026-10-29_

**will-ia** is an open-source personal AI assistant that runs **entirely on the user's own machine**. There is
no central service, account system or database operated by the authors.

- **Data accessed:** when the user connects their own Google account (using an OAuth client the user
  created), the app may access Google Calendar (`calendar.readonly`, `calendar.events`) and Gmail
  (`gmail.readonly`, `gmail.modify`, `gmail.send`), only to perform what the user asks.
- **Actions with external effects** (sending email, changing events, sending messages) require the
  user's explicit approval.
- **No sale, sharing or advertising use** of data. Data is not used by us to train AI models.
- **Storage:** tokens and settings are stored locally. Content needed to answer a request is processed
  locally and sent only to the AI provider the user configured (Anthropic / OpenAI), under their own
  policies. Nothing is sent to the authors' servers.
- **Google API data:** use of information received from Google APIs adheres to the
  [Google API Services User Data Policy](https://developers.google.com/terms/api-services-user-data-policy),
  including the Limited Use requirements.
- **Deletion:** disconnect the Google account in the app (deletes local tokens) and/or revoke access at
  <https://myaccount.google.com/permissions>.
- **Contact:** willguitaradmfar@gmail.com
