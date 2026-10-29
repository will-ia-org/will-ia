#!/bin/zsh
# Gera os ícones a partir dos SVGs (macOS: Chrome headless + sips + iconutil + python3).
#   assets/icon/will-ia.svg        → will-ia-{16..1024}.png, will-ia.icns, will-ia.ico
#   assets/icon/tray-template.svg  → tray-template-{22,32,44,64}.png (monocromático, só alfa)
# Rode após mudar o desenho: ./assets/icon/gerar.sh
set -euo pipefail
cd "${0:A:h}"
CHROME="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
render() { # svg png
  "$CHROME" --headless=new --disable-gpu --hide-scrollbars --default-background-color=00000000 \
    --window-size=1024,1024 --screenshot="$2" "file://$PWD/$1" >/dev/null 2>&1
}
render will-ia.svg will-ia-1024.png
render tray-template.svg tray-template-1024.png
for s in 512 256 128 64 48 44 32 22 16; do sips -z $s $s will-ia-1024.png --out will-ia-$s.png >/dev/null; done
for s in 64 44 32 22; do sips -z $s $s tray-template-1024.png --out tray-template-$s.png >/dev/null; done
rm -f tray-template-1024.png
# .icns (macOS)
rm -rf will-ia.iconset && mkdir will-ia.iconset
for s in 16 32 128 256 512; do
  cp will-ia-$s.png will-ia.iconset/icon_${s}x${s}.png
  d=$((s*2)); cp will-ia-$d.png will-ia.iconset/icon_${s}x${s}@2x.png
done
iconutil -c icns will-ia.iconset -o will-ia.icns && rm -rf will-ia.iconset
# .ico (Windows): contêiner ICO com entradas PNG
python3 - <<'PY'
import struct
sizes=[16,32,48,64,128,256]
blobs=[(s,open(f"will-ia-{s}.png","rb").read()) for s in sizes]
out=bytearray(struct.pack("<HHH",0,1,len(blobs)))
off=6+16*len(blobs)
for s,b in blobs:
    out+=struct.pack("<BBBBHHII", 0 if s==256 else s, 0 if s==256 else s, 0,0,1,32,len(b),off); off+=len(b)
for _,b in blobs: out+=b
open("will-ia.ico","wb").write(out)
PY
ls -la *.png *.icns *.ico | awk '{print $5, $9}'
