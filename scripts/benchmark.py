"""Three repetitions in fresh processes. Publishes data only after the images pass validation."""
import csv
import json
import math
import os
import platform
import subprocess
import sys
from pathlib import Path
from visual import ROOT, check

out = Path(sys.argv[1] if len(sys.argv) > 1 else "metrics/local").resolve()
out.mkdir(parents=True, exist_ok=False)
bins = {}
for backend in ("egui",):
    target = ROOT / "target"
    subprocess.run(["cargo","build","--release","--locked","--target-dir",target,
                    "--manifest-path",ROOT / f"backends/{backend}/Cargo.toml"], check=True)
    bins[backend] = target / f"release/jsnir-{backend}"

report = {"os":platform.platform(), "rustc":subprocess.check_output(["rustc","-V"],text=True).strip(),
          "cpu":next(row.split(":")[1].strip() for row in Path("/proc/cpuinfo").read_text().splitlines() if row.startswith("model name")),
          "samples_per_run":1000, "warmup":100, "runs":[]}
cases = [(b,m) for b in bins for m in ("rust","js")]
for repeat in range(1,4):
    for backend,mode in (cases if repeat % 2 else reversed(cases)):
        directory = out / f"{repeat}-{backend}-{mode}"
        env = dict(os.environ,SDL_VIDEODRIVER="offscreen")
        process = subprocess.run([bins[backend],mode,directory],env=env,check=True,capture_output=True,text=True)
        visual = check(directory)
        with (directory / "samples.csv").open() as f:
            rows = list(csv.DictReader(f))
        assert len(rows) == 1000
        row = {"backend":backend,"mode":mode,"repeat":repeat,"driver":env["SDL_VIDEODRIVER"],
               "renderer":process.stderr.strip(), "visual":visual}
        for field in ("commit_ns","render_ns"):
            values=sorted(int(s[field]) for s in rows)
            row[field]={f"p{p}":values[math.ceil(len(values)*p/100)-1] for p in (50,95,99)}
        report["runs"].append(row)
(out/"summary.json").write_text(json.dumps(report,indent=2)+"\n")
print(out / "summary.json")
