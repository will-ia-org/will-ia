# ADR-0012 — Google (Agenda e Gmail) com OAuth Client do próprio usuário e REST direto

**Status:** Aceita · **Data:** 2026-10-19

## Contexto
Agenda e Gmail exigem OAuth 2.0. Alternativas para o client: embutir um projeto nosso (exige verificação da Google para escopos sensíveis do Gmail, limita usuários de teste, expõe `client_secret`) ou o usuário criar o seu. Para a API: crates gerados `google-calendar3`/`google-gmail1` (pesados, tipos verbosos, dependem de `hyper` antigo) ou REST direto.

## Decisão
O usuário cria um OAuth Client "App para computador" no seu projeto Google Cloud e cola `client_id`/`client_secret`. Fluxo Authorization Code + PKCE com redirect em loopback (crate `oauth2`). Escopos incrementais. Refresh tokens no keyring. Chamadas REST diretas com `reqwest` + `serde` em um `GoogleClient` compartilhado. A UI orienta a **publicar o app** para evitar a expiração de 7 dias dos refresh tokens em modo teste.

## Consequências
- (+) Sem dependência de infraestrutura nossa, sem auditoria da Google, sem segredo no binário; múltiplas contas suportadas.
- (+) Código pequeno e controlado; fácil adicionar endpoints.
- (−) Onboarding do Google tem ~5 passos manuais; mitigado por passo a passo e validações na UI.
- (−) Tela "app não verificado" no consentimento (aceitável para uso pessoal).
