# ADR-0021 — Instalador próprio por usuário (`will-ia-setup`) com os binários embutidos

**Status:** Aceita · **Data:** 2026-10-29

## Contexto

Até a v0.1.4 instalar/atualizar o will-ia era manual: baixar o tar.gz/zip/.app, copiar os dois
binários para a pasta certa, escrever o LaunchAgent/unit/chave Run à mão, matar o servidor
residente (que ignora SIGTERM) e religar. Na máquina de origem isso virou um LaunchAgent manual
(`com.will.will-ia`) e substituições de binário feitas por um agente — frágil e sem padrão entre SOs.

O usuário quer um binário de setup "next, next, next" por SO que: detecte o que já existe, pare o
que está rodando, troque os binários **sem perder dados**, registre o serviço no início do
sistema, crie atalhos e sirva para atualizar clicando nele.

## Decisão

1. **Terceiro binário** `will-ia-setup` (`crates/will-setup`), um por plataforma, com `will-ia` e
   `will-ia-app` **embutidos** (`build.rs` + `WILL_SETUP_PAYLOAD_DIR`): um arquivo só instala tudo,
   offline, e a versão do instalador é sempre a dos binários dentro dele.
2. **Instalação por usuário, sem privilégios**: `~/Applications/will-ia.app` · `~/.local/bin` ·
   `%LOCALAPPDATA%\Programs\will-ia`; serviço de sessão (LaunchAgent / autostart ou user unit /
   chave Run), coerente com `docs/13` ("serviço = aplicação de sessão com autostart").
3. **Assistente egui** (mesma stack do app) + **modo texto** equivalente (`--cli`, `--yes`), porque
   servidor Linux não tem tela. Toda a lógica fica em `plan.rs`/`os.rs`/`install.rs`; a GUI e a CLI
   só apresentam.
4. **Só instalação**: nada de configurar Telegram/Google/WhatsApp; isso é do app. A pasta de dados
   nunca é tocada (exceto `install-manifest.json`; `--purge` é explícito).
5. **Absorve instalações manuais**: LaunchAgent `com.will.will-ia` vira backup e dá lugar a
   `dev.will.will-ia`; Login Item do `auto-launch` é removido no macOS e o app para de aplicá-lo
   quando o manifesto existe.

## Consequências

- O instalador fica grande (≈ 65 MB) e precisa ser compilado **depois** dos dois binários; o
  `release.yml` faz isso no mesmo job e publica `will-ia-setup-<versão>-<plataforma>`.
- A partir da v0.1.5 a atualização na máquina do usuário é **pelo instalador** (clique duplo),
  não por cópia de binário; agentes/automação só compilam e apontam o caminho do setup.
- Sem assinatura/notarização, o SO pede confirmação na primeira abertura (igual ao `.app`).
- Rollback, ícone e instalação de sistema ficam para depois (ver "Limites" em `docs/27`).
