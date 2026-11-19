"""Event → real presentation latency. Usage: python scripts/presentation.py FOLDER [FOLDER...]

For each event, finds the first presented frame (wp_presentation) from the injection frame on
whose content shows the expected value. Reports p50/p95/p99, frames of delay and the refresh period.
"""
import csv, json, statistics, sys
from collections import Counter
from pathlib import Path


def analyze(folder: Path):
    frames = list(csv.DictReader(open(folder / "frames.csv")))
    events = list(csv.DictReader(open(folder / "events.csv")))
    meta = json.loads((folder / "meta.json").read_text())
    by_frame = {int(f["frame"]): f for f in frames}
    refresh = statistics.median(int(f["refresh_ns"]) for f in frames if f["refresh_ns"] and int(f["refresh_ns"]) > 0)
    latencies, late, missing = [], Counter(), 0
    for e in events:
        start, expected, inject = int(e["frame"]), int(e["expected"]), int(e["inject_ns"])
        shown = next((by_frame[f] for f in range(start, start + 60) if f in by_frame
                      and int(by_frame[f]["observed"]) == expected and by_frame[f]["presented_ns"]), None)
        if shown is None:
            missing += 1
            continue
        latencies.append((int(shown["presented_ns"]) - inject) / 1e6)
        late[int(shown["frame"]) - start] += 1
    latencies.sort()
    q = lambda p: latencies[int(p * (len(latencies) - 1))]
    return {
        "folder": folder.name, "events": len(events), "measured": len(latencies), "not_presented": missing,
        "refresh_ms": round(refresh / 1e6, 3), "p50_ms": round(q(.5), 3), "p95_ms": round(q(.95), 3),
        "p99_ms": round(q(.99), 3), "frames_late": dict(sorted(late.items())),
        "discarded": meta["discarded"], "clock": meta["clock"],
    }


if __name__ == "__main__":
    for folder in sys.argv[1:]:
        print(json.dumps(analyze(Path(folder)), ensure_ascii=False))
