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

## Adendo (2026-10-29, noite): chave-mestra sai do Keychain

**O que aconteceu:** mesmo com assinatura estável, o Will continuou vendo ~20 diálogos
*"will-ia Server wants to access key 'will-ia'"* pedindo a senha do keychain `login`. Logs:

- a migração preguiçosa funcionou (os valores foram copiados para `secrets.enc`), mas **cada
  item antigo pede a senha ao ser lido** — e a leitura é preguiçosa, então os diálogos vêm em
  levas (tokens Claude ao listar o Motor, refresh tokens do Google ao usar a agenda…);
- **apagar o item antigo falha** com `errSecInvalidOwnerEdit` ("Invalid attempt to change the
  owner of this item"): o macOS só deixa o *dono* (o binário ad-hoc antigo, que já não existe)
  remover o item. Resultado: os 19 itens continuam no Acesso às Chaves, e qualquer segredo ainda
  não migrado volta a pedir senha a cada start.

**Decisão:** a chave-mestra passa a viver em **arquivo** — `<data_dir>/secrets.key` (base64,
0600, gravação atômica), `FileMasterKey`. O Keychain **sai do caminho normal**:

- na primeira execução, se existir `will-ia`/`master-key` no Keychain, a chave é **copiada** para
  o arquivo (o cofre já cifrado continua legível; esse item tem o servidor na ACL, então não pede
  senha). Depois disso o Keychain só é consultado para migrar itens antigos ainda não lidos;
- segredos novos nunca tocam o Keychain → **zero diálogos**, em qualquer versão do binário;
- a falha ao apagar o item antigo vira `info` (inofensiva: o cofre é lido primeiro); a limpeza
  dos itens `will-ia` no Acesso às Chaves fica a cargo do usuário.

**Trade-off assumido:** com a chave ao lado do cofre, `secrets.enc` + `secrets.key` equivalem a
um arquivo em texto claro com 0600 — a proteção é a do usuário do SO (permissões + FileVault),
o mesmo modelo de `gh`, `rclone`, `aws` e `gcloud`. Quem tem a sessão do usuário lê os segredos;
quem só tem o disco (sem FileVault) também. Em troca, nenhum diálogo e nenhum acoplamento com a
assinatura do binário. A chave num item do Keychain (modelo anterior) continua implementada
(`KeyringMasterKey`), caso se queira voltar.

**Backup:** `secrets.enc` só é útil junto com `secrets.key` (ambos em `<data_dir>`).
