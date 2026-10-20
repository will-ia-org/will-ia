#!/bin/sh
# Lançador portátil do will-ia (Linux e macOS).
# Layout esperado ao lado deste script:
#   bin/will-ia-linux-x86_64, bin/will-ia-linux-aarch64,
#   bin/will-ia-macos-x86_64, bin/will-ia-macos-aarch64 (ou bin/will-ia-windows-x86_64.exe)
#   will-ia.db (criado no primeiro uso), data/ (criada no primeiro uso), portable (marcador)
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
BIN="$ROOT/bin/will-ia-$os-$arch"
if [ ! -x "$BIN" ]; then
  echo "Binário não encontrado: $BIN" >&2
  exit 1
fi
touch "$ROOT/portable" 2>/dev/null || true
export WILL_IA_PORTABLE=1
exec "$BIN" "$@"
