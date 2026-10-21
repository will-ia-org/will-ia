# Pacote portátil (pendrive)

```
will-ia/
├── will-ia.sh            # lançador Linux/macOS (detecta SO e arquitetura)
├── will-ia.cmd           # lançador Windows
├── portable              # marcador: "tudo fica ao lado deste arquivo"
├── will-ia.db            # banco SQLite (criado no primeiro uso)
├── data/                 # segredos (arquivo), sessões do WhatsApp, logs, cache
├── workspace/            # (opcional) base de conhecimento levada no pendrive
└── bin/
    ├── will-ia-linux-x86_64
    ├── will-ia-linux-aarch64
    ├── will-ia-macos-x86_64
    ├── will-ia-macos-aarch64
    └── will-ia-windows-x86_64.exe
```

Como funciona: o binário detecta o modo portátil quando encontra o arquivo `portable`
(ou `will-ia.db`, ou `data/` + `bin/`) na própria pasta ou na pasta acima (`bin/`).
Nesse modo:

- o banco fica em `will-ia.db` na raiz e tudo o mais em `data/`;
- os segredos vão para `data/secrets.json` (o keyring da máquina não serve, porque o
  pendrive muda de máquina). Proteja o pendrive: quem tiver o arquivo tem os tokens;
- a base de conhecimento pode ser um caminho **relativo** à raiz (ex.: `workspace`),
  assim ela viaja junto; caminhos absolutos continuam valendo;
- "Iniciar com o sistema" fica desabilitado.

O que precisa existir na máquina hospedeira: o Claude Code CLI (`claude`) no PATH
(ou o caminho informado em Motor Claude) e, para scripts da KB, Node/Python.

Montar o pacote: copie os binários de cada plataforma para `bin/` com os nomes acima
(`cargo build --release -p will-desktop` gera `target/release/will-ia`), os dois
lançadores e crie o arquivo `portable` vazio. Nenhum outro arquivo é necessário.

## macOS: "Apple could not verify … is free of malware"

Os binários ainda não são notarizados. Na primeira execução, libere o arquivo da quarentena:

```bash
xattr -d com.apple.quarantine bin/will-ia-macos-aarch64   # ou bin/will-ia-macos-x86_64
chmod +x bin/will-ia-macos-aarch64
```

Ou, depois da primeira tentativa, vá em Ajustes do Sistema › Privacidade e Segurança e clique em "Abrir Mesmo Assim".
