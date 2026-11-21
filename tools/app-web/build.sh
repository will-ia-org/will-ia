#!/bin/sh
# Monta o app web do will-ia em dist/app-web (docs/33): compila o will-ia-app
# para WASM, gera a cola JS com o wasm-bindgen e copia a página.
# Requisitos: `rustup target add wasm32-unknown-unknown` e
# `cargo install wasm-bindgen-cli --version <a mesma do Cargo.lock>`.
set -eu
cd "$(dirname "$0")/../.."
OUT=dist/app-web
cargo build --release --target wasm32-unknown-unknown -p will-app
rm -rf "$OUT"
mkdir -p "$OUT"
wasm-bindgen --target web --no-typescript --out-dir "$OUT" --out-name will-ia-app \
  target/wasm32-unknown-unknown/release/will-ia-app.wasm
cp crates/will-app/web/index.html crates/will-app/web/manifest.webmanifest "$OUT/"
sips -z 180 180 assets/icon/will-ia-512.png --out "$OUT/icon-180.png" >/dev/null 2>&1 \
  || cp assets/icon/will-ia-256.png "$OUT/icon-180.png"
cp assets/icon/will-ia-512.png "$OUT/icon-512.png"
# Pré-comprime o que é grande: em rede móvel o WASM cai para ~1/3.
gzip -9 -k -f "$OUT/will-ia-app_bg.wasm" "$OUT/will-ia-app.js"
ls -la "$OUT"
