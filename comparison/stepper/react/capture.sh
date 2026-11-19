#!/bin/sh
# Renders the six spec states with headless Chromium into ../captures/react-s1.png … react-s6.png.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out="$here/../captures"
mkdir -p "$out"
capture() {
  chromium --headless --disable-gpu --hide-scrollbars --force-device-scale-factor=1 --window-size=640,560 \
    --virtual-time-budget=3000 --allow-file-access-from-files --screenshot="$out/react-$1.png" \
    "file://$here/index.html?capture=1&$2" 2>/dev/null
}
capture s1 "step=1"
capture s2 "step=2&plan=pro"
capture s3 "step=3"
capture s4 "step=3&name=Ada&focus=1"
capture s5 "step=4&name=Ada&plan=pro"
capture s6 "step=5&name=Ada"
