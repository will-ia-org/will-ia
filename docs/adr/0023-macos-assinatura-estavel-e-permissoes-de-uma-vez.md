# ADR-0023 — macOS: assinatura com identidade estável e permissões pedidas de uma vez

- **Data:** 2026-10-29
- **Status:** aceito
- **Relacionados:** ADR-0021 (instalador), ADR-0022 (servidor acessório), `docs/27` "Permissões no macOS"

## Contexto

1. **"Toda vez que instalo tenho que dar permissão de novo"** (microfone, Keychain…). Os binários
   saem do `cargo build` com assinatura *ad-hoc* do linker (`flags=adhoc,linker-signed`, identificador
   `will_ia_app-<hash do crate>`), cujo **requisito designado** é `cdhash H"…"`. O TCC (microfone,
   notificações, Arquivos e Pastas) e o Keychain amarram a concessão a esse requisito; cada build tem
   um cdhash novo, logo é "outro app" e tudo é pedido outra vez. Confirmado nesta máquina com
   `codesign -dr -` nos binários instalados.
2. **Os pedidos saíam aos poucos**, no primeiro uso de cada recurso — ruim quando o usuário está
   remoto (Telegram) e não há ninguém para clicar em "Permitir".

## Decisão

- **Identidade estável no instalador** (`crates/will-setup/src/sign.rs`): na primeira instalação o
  `will-ia-setup` cria um certificado autoassinado de *code signing* ("will-ia Local Signing", RSA
  2048, 20 anos) com o `openssl` do sistema e o importa num **keychain próprio**
  (`<data_dir>/signing/will-ia-signing.keychain-db`, senha aleatória em `keychain.pass` 0600,
  `set-key-partition-list` liberando o `codesign`) — sem pedir a senha do usuário e sem diálogo.
  Depois assina `will-ia` (`-i dev.will.will-ia.server`), `will-ia-app` e o bundle
  (`-i dev.will.will-ia`). O requisito designado passa a ser
  `identifier "dev.will.will-ia" and certificate leaf = H"<sha1 do certificado>"`, idêntico em toda
  versão. Verificado num keychain temporário: o `codesign` assina com a identidade **mesmo não
  confiável** (`CSSMERR_TP_NOT_TRUSTED` na listagem) e o binário "satisfies its Designated
  Requirement"; TCC e Keychain só comparam o requisito, não avaliam a cadeia. Se `openssl`/`security`
  falharem, cai em ad-hoc com identificador fixo (`-s - -i …`) e avisa no log.
- **Passagem de permissões na primeira abertura** (`crates/will-app/src/permissions.rs`): o app, ao
  abrir pela primeira vez após a instalação (marcador `<config do app>/permissions-pass.json` com a
  versão da passagem), pede o **microfone** pelo `AVCaptureDevice requestAccessForMediaType:` e dispara
  uma notificação "will-ia instalado" para gerar o pedido de **notificações**. O **servidor** lista a
  base de conhecimento no start (`will-ia` `main.rs`): isso faz o pedido de **Arquivos e Pastas** sair
  de uma vez e, se já estiver negado (EPERM), registra aviso e notifica com o caminho de Ajustes.
- **Cartão "Permissões do macOS"** na página Início do app: estado do microfone (AVFoundation),
  botões "Pedir agora"/"Testar notificação"/"Abrir Ajustes" (URLs `x-apple.systempreferences:`).
- **Notificação única por evento** na máquina do servidor: o app só emite notificação nativa quando o
  servidor é **remoto**; em loopback o servidor já notifica. A `will_platform::notify` ganhou um teto
  de 6 notificações em voo (o backend do macOS segura a thread até a notificação ser dispensada).

## Alternativas

- *Developer ID + notarização*: elimina também o aviso de Gatekeeper, mas exige conta paga da Apple e
  não cabe num instalador que cada usuário roda na própria máquina. Fica como caminho para uma
  distribuição pública.
- *Certificado confiável pelo sistema* (`security add-trusted-cert`): pede a senha do usuário num
  diálogo; desnecessário para TCC/Keychain.

## Consequências

- Depois da **primeira** instalação com esta versão, o sistema pede as permissões uma última vez
  (o requisito mudou de cdhash para certificado); nas seguintes a concessão persiste.
- Itens do Keychain gravados pelos binários ad-hoc antigos pedem "Permitir sempre" uma vez mais.
- O keychain de assinatura fica em `<data_dir>/signing/`; apagá-lo gera uma identidade nova (e um
  novo ciclo de pedidos). `will-ia-setup --uninstall --purge` o remove junto com os dados.
