#!/bin/sh
# Lançador portátil do will-ia (Linux e macOS): sobe o SERVIDOR e depois abre o APP.
# Layout esperado ao lado deste script:
#   bin/will-ia-<os>-<arch>       servidor residente (bandeja quando há desktop)
#   bin/will-ia-app-<os>-<arch>   app desktop (cliente do servidor)
#   os = linux | macos; arch = x86_64 | aarch64
#   will-ia.db (criado no primeiro uso), data/ (criada no primeiro uso), portable (marcador)
# Argumentos passados a este script vão para o app (ex.: --hidden).
set -e
ROOT=$(cd "$(dirname "$0")" && pwd)
OS=$(uname -s)
ARCH=$(uname -m)
case "$OS" in
  Linux)  os=linux ;;
  Darwin) os=macos ;;
  *) echo "SO não suportado: $OS" >&2; exit 1 ;;
esac
case "$ARCH" in
  x86_64|amd64) arch=x86_64 ;;
  arm64|aarch64) arch=aarch64 ;;
  *) echo "Arquitetura não suportada: $ARCH" >&2; exit 1 ;;
esac
SERVER="$ROOT/bin/will-ia-$os-$arch"
APP="$ROOT/bin/will-ia-app-$os-$arch"
if [ ! -x "$SERVER" ]; then
  echo "Binário do servidor não encontrado: $SERVER" >&2
  exit 1
fi
touch "$ROOT/portable" 2>/dev/null || true
export WILL_IA_PORTABLE=1

# 1) servidor: se já há um rodando sobre este pendrive, a segunda execução só
#    tenta abrir o app e sai (trinco instance.lock), então é seguro chamar sempre.
#    Com desktop ele fica na bandeja; sem desktop roda sem bandeja.
"$SERVER" >/dev/null 2>&1 &

# 2) app: espera a API local aparecer (data/api-port) e conecta em "Esta máquina".
if [ -x "$APP" ]; then
  i=0
  while [ ! -s "$ROOT/data/api-port" ] && [ $i -lt 40 ]; do
    sleep 0.25; i=$((i+1))
  done
  exec "$APP" "$@"
fi
echo "App não encontrado ($APP); o servidor ficou no ar. Rode \`$SERVER info\` para ver a URL e o token." >&2
wait
