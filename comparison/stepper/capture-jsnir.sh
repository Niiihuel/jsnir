#!/bin/sh
# Captures the JSNIR port in the six spec states (offscreen wgpu, scale 1) as captures/jsnir-s1..s6.png.
set -eu
root=$(cd "$(dirname "$0")/../.." && pwd)
cargo build --release --locked --features stepper --manifest-path "$root/backends/gpu/Cargo.toml" --target-dir "$root/target"
bin="$root/target/release/jsnir-gpu"
out="$root/comparison/stepper/captures"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
shot() {
    id=$1; shift
    "$bin" stepper "$@" --offscreen "$tmp/$id.ppm"
    magick "$tmp/$id.ppm" "$out/jsnir-$id.png"
}
shot s1 --step 1
shot s2 --step 2
shot s3 --step 3
shot s4 --step 3 --name Ada --focus
shot s5 --step 4 --name Ada
shot s6 --step 5 --name Ada
