"""Captures the client area of both modes on a test X11 server."""
import argparse
import json
import os
import shutil
import subprocess
import time
from pathlib import Path
from visual import ROOT, STATES, check, rgb

BACKENDS = ("egui",)


def session(backend, out):
    # DISPLAY comes from xvfb-run or a dedicated lab X11 server.
    env = dict(os.environ, SDL_VIDEODRIVER="x11", WINIT_X11_SCALE_FACTOR="1")
    env.pop("WAYLAND_SOCKET", None)
    env.pop("WAYLAND_DISPLAY", None)
    binary = ROOT / "target/release" / f"jsnir-{backend}"
    cargo = ["cargo","build","--release","--target-dir",ROOT / "target",
             "--manifest-path",ROOT / f"backends/{backend}/Cargo.toml"]
    if (ROOT / f"backends/{backend}/Cargo.lock").exists():
        cargo.append("--locked")
    subprocess.run(cargo, check=True)
    for mode in ("rust", "js"):
        folder = out / mode
        folder.mkdir(parents=True, exist_ok=True)
        app = subprocess.Popen([binary,mode], env=env)
        try:
            wid = subprocess.check_output(["xdotool","search","--sync","--onlyvisible",
                                           "--name","^JSNIR$"], env=env, timeout=30).splitlines()[0].decode()
            subprocess.run(["xdotool","mousemove","--window",wid,"160","173"],env=env,check=True)
            previous = 0
            for state in STATES:
                print(f"{backend} · {mode} · capture after {state} clicks", flush=True)
                count = state - previous
                if count:
                    subprocess.run(["xdotool","click","--repeat",str(count),"--delay","50","1"],
                                   env=env,check=True,timeout=10+count/10)
                previous = state
                path = folder / f"{state:04}.png"
                expected = rgb(ROOT / "design" / path.name)
                deadline = time.monotonic()+4
                while True:
                    subprocess.run(["magick","import","-window",wid,path],check=True,env=env,timeout=5)
                    if rgb(path)==expected:
                        break
                    if app.poll() is not None or time.monotonic()>deadline:
                        raise RuntimeError(f"Visual difference, see {path}")
                    time.sleep(0.04)
            (folder / "parity.json").write_text(json.dumps(check(folder),indent=2)+"\n")
        finally:
            app.terminate()
            try:
                app.wait(timeout=5)
            except subprocess.TimeoutExpired:
                app.kill()
                app.wait()


if __name__ == "__main__":
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("backend", choices=BACKENDS)
    parser.add_argument("out",type=Path)
    args=parser.parse_args()
    for tool in ("xdotool","magick"):
        if not shutil.which(tool):
            parser.error(f"Missing {tool}")
    if args.out.exists() and any(path.is_file() for path in args.out.rglob("*")):
        parser.error("The directory already has files. Pick another one to keep those captures.")
    session(args.backend,args.out.resolve())
