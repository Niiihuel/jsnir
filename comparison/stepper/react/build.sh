#!/bin/sh
# Downloads the original React Bits Stepper into original/ and bundles app.jsx with it into dist/
# (one JS and one CSS file, no runtime network). The component is not in this repository: its license
# (MIT + Commons Clause) does not allow redistributing the component itself.
# Source: https://github.com/DavidHDev/react-bits/tree/fefc5b05ca34a7475b38a977127441fbfad2f177/src/content/Components/Stepper
# Pinned: react 19.3.1, react-dom 19.3.1, @hugeicons/react 1.1.10, @hugeicons/core-free-icons 4.4.0, esbuild 0.28.2.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
upstream=https://raw.githubusercontent.com/DavidHDev/react-bits/fefc5b05ca34a7475b38a977127441fbfad2f177
mkdir -p "$here/original"
curl -fsSL -o "$here/original/Stepper.jsx" "$upstream/src/content/Components/Stepper/Stepper.jsx"
curl -fsSL -o "$here/original/Stepper.css" "$upstream/src/content/Components/Stepper/Stepper.css"
curl -fsSL -o "$here/original/LICENSE-react-bits.md" "$upstream/LICENSE.md"
deps=$(mktemp -d)
trap 'rm -rf "$deps"' EXIT
cd "$deps"
npm init -y >/dev/null
npm install --silent --no-audit --no-fund esbuild@0.28.2 react@19.3.1 react-dom@19.3.1 \
  @hugeicons/react@1.1.10 @hugeicons/core-free-icons@4.4.0
NODE_PATH="$deps/node_modules" "$deps/node_modules/.bin/esbuild" "$here/app.jsx" --bundle --minify \
  --format=iife --jsx=automatic --loader:.jsx=jsx --define:process.env.NODE_ENV='"production"' \
  --outfile="$here/dist/app.js"
