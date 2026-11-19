"""Image contract v1. Compares RGB client-area captures, without rescaling."""
import argparse
import hashlib
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
STATES = (0, 1, 42, 999, 1000)
SIZE = (320, 240)


def rgb(path):
    # ImageMagick keeps the size and can check PNG or PPM captures.
    dims = subprocess.check_output(["magick", "identify", "-format", "%w %h", path])
    if dims != b"320 240":
        raise ValueError(f"{path}: {dims.decode()} px, 320 240 required")
    return subprocess.check_output(["magick", path, "-alpha", "off", "-depth", "8", "rgb:-"])


def check(directory):
    results = []
    for state in STATES:
        name = f"{state:04}"
        matches = [p for ext in ("ppm", "png") if (p := directory / f"{name}.{ext}").is_file()]
        if len(matches) != 1:
            raise ValueError(f"Expected one capture of {name} in {directory}")
        actual = rgb(matches[0])
        expected = rgb(ROOT / "design" / f"{name}.png")
        if len(actual) != 320 * 240 * 3 or len(expected) != len(actual):
            raise ValueError("Incomplete RGB capture")
        diff = sum(actual[i:i+3] != expected[i:i+3] for i in range(0, len(actual), 3))
        if diff:
            raise ValueError(f"{matches[0]}: {diff} different pixels")
        results.append({"event": state, "different_pixels": diff, "rgb_sha256": hashlib.sha256(actual).hexdigest()})
    return results


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    for result in check(args.directory):
        print(result)
