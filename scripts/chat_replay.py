"""Runs script A–E in both modes and compares every framebuffer."""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("out", type=Path)
parser.add_argument("--repeats", type=int, default=5)
args = parser.parse_args()
if args.repeats < 1:
    parser.error("At least one repeat is required")
subprocess.run(["cargo", "build", "--release", "--locked", "--manifest-path",
                ROOT / "backends/egui/Cargo.toml", "--target-dir", ROOT / "target"], check=True)
args.out.mkdir(parents=True, exist_ok=False)
baseline = {}
with (args.out / "times.csv").open("w", newline="") as handle:
    writer = csv.writer(handle)
    writer.writerow(["repeat", "mode", "send_to_D_ms", "send_to_E_ms"])
    for i in range(args.repeats):
        for mode in (("rust","js") if i % 2 == 0 else ("js","rust")):
            with tempfile.TemporaryDirectory() as temp:
                folder = (args.out if i == 0 else Path(temp)) / mode
                run = subprocess.run([ROOT / "target/release/jsnir-egui", mode, "chat", folder],
                                     env=dict(os.environ, SDL_VIDEODRIVER="offscreen"),
                                     check=True, stdout=subprocess.PIPE, text=True)
                times = re.fullmatch(r"D=(\d+) ms E=(\d+) ms\s*", run.stdout)
                if not times:
                    raise ValueError(run.stdout)
                writer.writerow([i+1, mode, *times.groups()])
                for state in "ABCDE":
                    pixels = (folder / f"{state}.ppm").read_bytes()
                    digest = hashlib.sha256(pixels).hexdigest()
                    if baseline.setdefault(state, digest) != digest:
                        (args.out / f"difference-{i+1}-{mode}-{state}.ppm").write_bytes(pixels)
                        raise RuntimeError(f"Different image in {mode}, state {state}")
(args.out / "parity.json").write_text(json.dumps({
    "verified_captures": args.repeats * 10,
    "reference": "egui Rust, first run",
    "sha256_ppm": baseline
}, indent=2) + "\n")
