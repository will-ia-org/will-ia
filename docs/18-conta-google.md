# 18 — Conta Google (OAuth) — base de Agenda e Gmail

Agenda ([19](19-google-agenda.md)) e Gmail ([20](20-gmail.md)) usam a mesma autorização. Este doc cobre o OAuth e a seção **Conta Google** da UI.

## OAuth Client próprio do usuário

O will-ia **não embute** um client do Google. O usuário cria o seu (gratuito) e cola `client_id` e `client_secret` na UI. Motivos: sem verificação da Google para escopos sensíveis de Gmail, sem limite compartilhado de usuários de teste, sem segredo no binário. Ver [ADR-0012](adr/0012-google-oauth-rest.md).

### Passo a passo exibido na UI

1. Acesse `console.cloud.google.com` → crie um projeto (ex.: `will-ia`).
2. **APIs e serviços → Biblioteca**: ative **Google Calendar API** e **Gmail API**.
3. **Tela de consentimento OAuth**: tipo **Externo**; preencha nome e e-mail; adicione os escopos (abaixo); adicione seu e-mail como **usuário de teste**.
4. **Credenciais → Criar credenciais → ID do cliente OAuth → tipo "App para computador"**. Copie `client_id` e `client_secret` para o will-ia.
5. **Importante:** em **Tela de consentimento → Status de publicação**, clique em **Publicar app**. Apps em modo *Teste* têm refresh tokens que **expiram em 7 dias**, o que obrigaria a reautorizar toda semana. Em *Produção* sem verificação, o Google mostra uma tela de "app não verificado" (clique em *Avançado → Acessar*), e o token não expira.

A UI valida o formato do `client_id` (`*.apps.googleusercontent.com`) e testa a autorização.

## Fluxo de autorização

- **Authorization Code + PKCE** (crate `oauth2`), redirect em **loopback**: o serviço sobe um listener temporário em `http://127.0.0.1:<porta aleatória>/oauth/google/callback`, abre o navegador padrão (`will-platform::open_url`), recebe o `code`, troca por tokens, e responde ao navegador com uma página "Pode fechar esta aba".
- `access_type=offline` e `prompt=consent` para garantir refresh token.
- Tokens no keyring: `google:<account_id>:refresh_token`. Access token só em memória, renovado sob demanda (`expires_in` − 60 s).
- Várias contas Google podem ser anexadas (lista). Cada uma guarda `email`, escopos concedidos e data.
- Revogação: botão **Desconectar** chama `https://oauth2.googleapis.com/revoke` e apaga do keyring.
- Erros `invalid_grant` (token revogado/expirado) → conta em `Degraded(Reauthorize)` com botão "Autorizar de novo"; ferramentas dessa conta retornam erro acionável.

## Escopos

Solicitados de forma **incremental**, conforme o que o usuário habilita:

| Recurso | Escopo | Quando |
|---|---|---|
| Identidade | `openid email` | sempre |
| Agenda ler | `https://www.googleapis.com/auth/calendar.readonly` | módulo Agenda ligado |
| Agenda escrever | `https://www.googleapis.com/auth/calendar.events` | ferramentas de criação/edição ligadas |
| Gmail ler | `https://www.googleapis.com/auth/gmail.readonly` | módulo Gmail ligado |
| Gmail enviar | `https://www.googleapis.com/auth/gmail.send` | `gmail_send` ligado |
| Gmail modificar | `https://www.googleapis.com/auth/gmail.modify` | ferramentas de arquivar/marcar/rotular ligadas (opcional) |

Ligar um recurso que exige escopo novo dispara nova autorização (consentimento incremental, `include_granted_scopes=true`).

## Cliente HTTP

REST direto com `reqwest` + `serde` (Calendar v3 e Gmail v1 são APIs JSON simples), em vez dos crates gerados `google-*` (pesados e com tipos difíceis). Um `GoogleClient` com: injeção de bearer, renovação automática, retry com backoff em 429/5xx respeitando `Retry-After`, e paginação por `pageToken`.

## Seção "Conta Google" na UI

| Campo | Tipo |
|---|---|
| `client_id` / `client_secret` | texto / secret |
| Contas conectadas | lista: e-mail, escopos, status, [Reautorizar] [Desconectar] |
| Ação | **Conectar conta Google** (abre navegador) |

## Testes

- Fluxo OAuth contra servidor mock (`wiremock`): troca de código, refresh, `invalid_grant`, revogação.
- Listener de loopback: porta aleatória, timeout de 5 min, estado anti-CSRF (`state`).


## Passo a passo no Google Cloud (como aparece na UI)

1. **Projeto**: selecione ou crie um projeto.
2. **APIs**: APIs e serviços › Biblioteca › ativar *Google Calendar API* e *Gmail API*.
3. **Tela de consentimento**: tipo Externo; adicione o próprio e-mail em usuários de teste.
4. **Publicar o app**: evita a expiração do refresh token em 7 dias do modo Teste. Não exige verificação para uso pessoal (aviso "app não verificado" → Avançado › Acessar).
5. **Credenciais › Criar credenciais › ID do cliente OAuth**, tipo **App para computador**.
6. Copiar `client_id` e `client_secret` para Configurações › Google e salvar.
7. **Conectar conta**, escolher escopos, autorizar no navegador.

### `redirect_uri_mismatch`

O will-ia usa loopback `http://127.0.0.1:<porta aleatória>/oauth/google/callback`. O Google só aceita porta aleatória sem cadastro para clientes **App para computador**. Com um cliente **Aplicativo da Web** o erro é garantido. Saídas: criar um cliente do tipo computador (recomendado) ou fixar a porta em Configurações › Google › "Porta do callback" (`google.redirect_port`) e cadastrar exatamente `http://127.0.0.1:<porta>/oauth/google/callback` em "URIs de redirecionamento autorizados".
