#!/bin/sh
# Assemble the static interface demo (published to GitHub Pages) into the
# directory given as $1 (default: _site). The demo runs entirely on the
# in-browser simulation in web/demo.js.
set -eu
out="${1:-_site}"
cd "$(dirname "$0")/.."
rm -rf "$out"
mkdir -p "$out"
cp web/style.css web/hud.js web/demo.js web/app.js web/host.html web/host.js web/shell.js "$out/"
# The client needs a daemon unless it runs in demo mode, and Pages has none.
# Send any direct visit to the demo-mode client; the demo's iframe already
# asks for ?demo, so it does not loop.
sed 's|<head>|<head>\
  <script>if (!/[?\&]demo/.test(location.search)) location.replace("client.html?demo");</script>|' \
  web/index.html > "$out/client.html"
cp web/demo.html "$out/index.html"
touch "$out/.nojekyll"
echo "demo written to $out"
