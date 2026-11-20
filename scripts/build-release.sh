#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
    echo "Usage: bash scripts/build-release.sh NEW_DIRECTORY [TAG]" >&2
    exit 2
fi
if [[ "$(uname -s)/$(uname -m)" != Linux/x86_64 ]]; then
    echo "This release build targets Linux x86-64." >&2
    exit 2
fi

output=$(realpath -m -- "$1")
if [[ -e "$output" || -L "$output" ]]; then
    echo "Release output already exists at $output" >&2
    exit 2
fi
root=$(cd -- "$(dirname -- "$0")/.." && pwd)
cd -- "$root"

manifest=backends/gpu/Cargo.toml
target=x86_64-unknown-linux-gnu
version=$(cargo metadata --locked --format-version 1 --no-deps --manifest-path "$manifest" \
    | jq -er '.packages[] | select(.name == "jsnir-gpu") | .version')
if [[ -n ${2:-} && "$2" != "v$version" ]]; then
    echo "Release tag $2 must match jsnir-gpu v$version." >&2
    exit 2
fi

parent=$(dirname -- "$output")
mkdir -p -- "$parent"
work=$(mktemp -d -- "$parent/.jsnir-release.XXXXXXXX")
trap 'rm -rf -- "$work"' EXIT

target_dir=$(realpath -m -- "${CARGO_TARGET_DIR:-target}")
flags=(--release --locked --target "$target" --target-dir "$target_dir" --no-default-features)
cargo test "${flags[@]}" -- --test-threads=1
cargo test "${flags[@]}" --manifest-path "$manifest" --all-targets
cargo build "${flags[@]}" --manifest-path "$manifest" --bin jsnir-gpu

name="jsnir-v$version-linux-x86_64"
package="$work/$name"
mkdir -- "$package" "$work/assets" "$work/unpacked" "$work/home"
mkdir -m 700 -- "$work/runtime"
mkdir -- "$package/sdk"
install -m 644 -- app/build-app.sh app/jsnir.d.ts app/sdk.ts app/tsconfig.base.json "$package/sdk/"
install -m 755 -- "$target_dir/$target/release/jsnir-gpu" "$package/jsnir-gpu"
strip --strip-unneeded "$package/jsnir-gpu"
install -m 644 -- LICENSE "$package/LICENSE"
install -m 644 -- assets/icons/Hugeicons-LICENSE.txt "$package/Hugeicons-LICENSE.txt"
cargo about generate --locked --fail --no-default-features --target "$target" \
    --manifest-path "$manifest" --config scripts/release-licenses.toml \
    --format json --output-file "$work/licenses.json"
jq -er '.licenses[] | "\(.id)\nUsed by \([.used_by[].crate | "\(.name) \(.version)"] | unique | join(", "))\n\n\(.text)\n"' \
    "$work/licenses.json" > "$package/THIRD-PARTY-LICENSES.txt"

archive="$name.tar.gz"
tar --sort=name --owner=0 --group=0 --numeric-owner -czf "$work/assets/$archive" -C "$work" "$name"
(
    cd -- "$work/assets"
    sha256sum "$archive" > "$archive.sha256"
    sha256sum --check "$archive.sha256"
)
tar -xzf "$work/assets/$archive" -C "$work/unpacked"

if [[ -z ${JSNIR_TSC:-} ]]; then
    JSNIR_TSC=$(bash scripts/fetch-tsc.sh "$work/typescript")
fi
export JSNIR_TSC
ts_version=$(jq -er '.packages["node_modules/@typescript/typescript-linux-x64"].version' package-lock.json)
if [[ $("$JSNIR_TSC" --version) != "Version $ts_version" ]]; then
    echo "Set JSNIR_TSC to the native TypeScript $ts_version executable." >&2
    exit 2
fi

# Run the extracted binary from outside the checkout with no tools on PATH.
(
    cd -- "$work/unpacked"
    run() {
        timeout 40s env -i PATH=/nonexistent HOME="$work/home" XDG_RUNTIME_DIR="$work/runtime" \
            WGPU_BACKEND=gl EGL_PLATFORM=surfaceless LIBGL_ALWAYS_SOFTWARE=1 "./$name/jsnir-gpu" "$@"
    }
    [[ $(run --version) == "jsnir-gpu $version" ]]
    for demo in rust js stepper; do
        if run "$demo" > /dev/null 2>&1; then
            echo "The engine archive contains the $demo example." >&2
            exit 1
        fi
    done

    cat > smoke.tsx <<'EOF'
mount(() => <Box paint={{ background: 0x224466ff }} />);
EOF
    bash "./$name/sdk/build-app.sh" smoke.tsx smoke.js
    run run smoke.js --size 64x48 --offscreen smoke.ppm
    [[ $(head -n 3 smoke.ppm) == $'P6\n64 48\n255' ]]
    read -r red green blue < <(od -An -t u1 -j 13 -N 3 smoke.ppm)
    [[ "$red $green $blue" == "34 68 102" ]]
    printf 'throw new Error("startup smoke test");\n' > broken.js
    if run run broken.js > startup.log 2>&1; then
        echo "The engine accepted a broken app." >&2
        exit 1
    fi
    grep -F 'startup smoke test' startup.log
)

mv -T -- "$work/assets" "$output"
printf 'Release assets written to %s\n' "$output"
