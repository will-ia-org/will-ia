# ADR-0024 — macOS: segredos num cofre cifrado, chave-mestra como único item do Keychain

- **Data:** 2026-10-29
- **Status:** Aceita

## Contexto

O `KeyringStore` guardava **um item do Keychain por segredo** (tokens do Claude, `client_secret`
do Google, refresh token de cada conta Google, Telegram, OpenAI, MCPs externos…). No macOS a ACL
de cada item fica amarrada ao *requisito designado* do binário que o criou; um binário novo
(até a ADR-0023, ad-hoc, com cdhash diferente a cada build) é "outro app", e o servidor dispara um
diálogo "will-ia Server quer acessar a chave…" **por item**, no start — 20+ diálogos seguidos a cada
instalação. Mesmo com a assinatura estável, "Permitir sempre" teria de ser clicado item a item, e
"Permitir" (sem *sempre*) volta a pedir em todo start.

## Decisão

`EncryptedFileStore` (`will-storage/src/secrets.rs`), padrão no macOS:

- os segredos ficam em **um arquivo** `<data_dir>/secrets.enc` (0600, gravação atômica
  tmp + rename), cifrado com **XChaCha20-Poly1305** (nonce aleatório de 24 bytes por gravação),
  envelope JSON `{ "v": 1, "alg": "xchacha20poly1305", "nonce": b64, "ct": b64 }`; o conteúdo
  cifrado é o JSON `{ chave: valor }`;
- a **chave-mestra** (32 bytes aleatórios, base64) é **um único item** do Keychain
  (`will-ia` / `master-key`), criado na primeira execução e lido uma vez por processo;
- **migração preguiçosa**: ao ler uma chave ausente no arquivo, o cofre consulta o item antigo
  (`will-ia` / `<secret_ref>`), copia o valor para o arquivo e **apaga** o item do Keychain. Cada
  segredo antigo pode pedir **uma última vez**; depois só a chave-mestra pede (e nada, com
  "Permitir sempre" + assinatura estável);
- `AutoSecretStore::new` escolhe o backend uma vez: macOS → cofre cifrado (se o Keychain falhar,
  arquivo JSON 0600 com aviso, como antes); Linux/Windows → `KeyringStore` por item (Secret
  Service e Credential Manager não amarram ACL à assinatura e não exibem um diálogo por item).

## Consequências

- Diálogos do Keychain no macOS: no máximo **1 por versão** (zero com "Permitir sempre").
- O arquivo em disco não revela nada sem a chave-mestra; a chave só existe no Keychain do usuário.
- Backup/restauração: `secrets.enc` só é útil com o item `master-key` do mesmo Keychain.
- Nova dependência: `chacha20poly1305` (RustCrypto). `AppPaths::encrypted_secrets_file()`.
