# Pacote portátil (pendrive)

```
will-ia/
├── will-ia.sh            # lançador Linux/macOS (detecta SO e arquitetura; sobe o servidor e abre o app)
├── will-ia.cmd           # lançador Windows (idem)
├── portable              # marcador: "tudo fica ao lado deste arquivo"
├── will-ia.db            # banco SQLite (criado no primeiro uso)
├── data/                 # segredos (arquivo), sessões do WhatsApp, logs, cache, api-port, ui-session.token
├── workspace/            # (opcional) base de conhecimento levada no pendrive
└── bin/
    ├── will-ia-linux-x86_64          # servidor
    ├── will-ia-app-linux-x86_64      # app desktop
    ├── will-ia-macos-x86_64          will-ia-app-macos-x86_64
    ├── will-ia-macos-aarch64         will-ia-app-macos-aarch64
    └── will-ia-windows-x86_64.exe    will-ia-app-windows-x86_64.exe
```

São **dois binários** por plataforma (`docs/26`, ADR-0019):

- **`will-ia`** (servidor): motor Claude, módulos, API local. Residente; com desktop fica na
  bandeja (item "Abrir will-ia"); sem desktop roda sem bandeja.
- **`will-ia-app`** (app): a janela. É um cliente do servidor; no pendrive conecta em
  "Esta máquina" lendo `data/api-port` e `data/ui-session.token`.

Os lançadores fazem as duas coisas: sobem o servidor (se já estiver no ar, a segunda
execução só abre o app e sai) e, assim que `data/api-port` aparece, abrem o app. Fechar a
janela do app só a oculta; rodar o lançador de novo traz a janela de volta.

Como funciona o modo portátil: os binários detectam quando encontram o arquivo `portable`
(ou `will-ia.db`, ou `data/` + `bin/`) na própria pasta ou na pasta acima (`bin/`).
Nesse modo:

- o banco fica em `will-ia.db` na raiz e tudo o mais em `data/`;
- os segredos vão para `data/secrets.json` (o keyring da máquina não serve, porque o
  pendrive muda de máquina). Proteja o pendrive: quem tiver o arquivo tem os tokens;
- a base de conhecimento pode ser um caminho **relativo** à raiz (ex.: `workspace`),
  assim ela viaja junto; caminhos absolutos continuam valendo;
- "Iniciar com o sistema" fica desabilitado;
- os perfis do app (`profiles.json`) ficam na pasta de configuração do app na máquina
  hospedeira, não no pendrive; o perfil "Esta máquina" é descoberto a cada execução.

O que precisa existir na máquina hospedeira: o Claude Code CLI (`claude`) no PATH
(ou o caminho informado em Motor Claude) e, para scripts da KB, Node/Python.

Montar o pacote: copie os binários de cada plataforma para `bin/` com os nomes acima
(`cargo build --release -p will-ia -p will-app` gera `target/release/will-ia` e
`target/release/will-ia-app`), os dois lançadores e crie o arquivo `portable` vazio.
Nenhum outro arquivo é necessário. O workflow `release.yml` monta esse zip a cada tag.

## macOS: "Apple could not verify … is free of malware"

Os binários ainda não são notarizados. Na primeira execução, libere os dois arquivos da quarentena:

```bash
xattr -d com.apple.quarantine bin/will-ia-macos-aarch64 bin/will-ia-app-macos-aarch64   # ou …-macos-x86_64
chmod +x bin/will-ia-macos-aarch64 bin/will-ia-app-macos-aarch64
```

Ou, depois da primeira tentativa, vá em Ajustes do Sistema › Privacidade e Segurança e clique em "Abrir Mesmo Assim".
