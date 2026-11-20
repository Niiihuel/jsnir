#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 2 ]]; then
    echo "Usage: bash build-app.sh APP.tsx NEW_BUNDLE.js" >&2
    exit 2
fi

sdk=$(cd -- "$(dirname -- "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT

cp -- "$sdk/jsnir.d.ts" "$sdk/sdk.ts" "$sdk/tsconfig.base.json" "$work/"
cp -- "$1" "$work/app.tsx"
cat > "$work/tsconfig.json" <<'EOF'
{
  "extends": "./tsconfig.base.json",
  "files": ["jsnir.d.ts", "sdk.ts", "app.tsx"]
}
EOF

"${JSNIR_TSC:-tsc}" -p "$work/tsconfig.json" --noEmit false --outDir "$work/out"
set -o noclobber
{
    cat -- "$work/out/sdk.js"
    printf '\n'
    cat -- "$work/out/app.js"
} > "$2"
