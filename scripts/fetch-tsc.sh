#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 ]]; then
    echo "Usage: bash scripts/fetch-tsc.sh NEW_DIRECTORY" >&2
    exit 2
fi
if [[ "$(uname -s)/$(uname -m)" != Linux/x86_64 ]]; then
    echo "This compiler download is for Linux x86-64." >&2
    exit 2
fi

root=$(cd -- "$(dirname -- "$0")/.." && pwd)
destination=$(realpath -m -- "$1")
if [[ -e "$destination" || -L "$destination" ]]; then
    echo "Compiler destination already exists at $destination" >&2
    exit 2
fi

package=$(jq -e '.packages["node_modules/@typescript/typescript-linux-x64"]' "$root/package-lock.json")
url=$(jq -er '.resolved' <<< "$package")
integrity=$(jq -er '.integrity' <<< "$package")
version=$(jq -er '.version' <<< "$package")

parent=$(dirname -- "$destination")
mkdir -p -- "$parent"
work=$(mktemp -d -- "$parent/.jsnir-tsc.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT

curl --fail --silent --show-error --location --retry 3 --connect-timeout 15 \
    --proto '=https' --proto-redir '=https' --output "$work/tsc.tgz" "$url"
actual=$(openssl dgst -sha512 -binary "$work/tsc.tgz" | openssl base64 -A)
if [[ "sha512-$actual" != "$integrity" ]]; then
    echo "The TypeScript archive does not match package-lock.json." >&2
    exit 1
fi

mkdir -- "$work/compiler"
tar -xzf "$work/tsc.tgz" --strip-components=1 --no-same-owner --directory "$work/compiler" package/lib
if [[ $("$work/compiler/lib/tsc" --version) != "Version $version" ]]; then
    echo "The compiler does not report TypeScript $version." >&2
    exit 1
fi
mv -T -- "$work/compiler" "$destination"
printf '%s\n' "$destination/lib/tsc"
