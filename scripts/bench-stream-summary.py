"""Prints one line per history size from the reports bench-stream.sh left in target/bench."""
import json
import sys

SIZE = {"B": 1, "KB": 1024, "MB": 1024**2, "GB": 1024**3}
TIME = {"ns": 1e-6, "µs": 1e-3, "ms": 1.0, "s": 1e3}


def size(text):
    value, unit = text.split()
    return float(value) * SIZE[unit]


def millis(text):
    value, unit = text.split()
    return float(value) * TIME[unit]


def mem(n):
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024 or unit == "GB":
            return f"{n:.1f} {unit}"
        n /= 1024


def main(out, histories):
    print(f"{'history':>7} {'frames':>7} {'transcripts':>11} {'agent_rows':>10} {'engine alloc':>13} {'per frame':>10} {'engine cpu':>10} {'render p99':>10}")
    for history in histories:
        try:
            with open(f"{out}/stream-{history}.json") as file:
                report = json.load(file)
        except (OSError, ValueError):
            print(f"{history:>7}  (no report)")
            continue
        fns = report["functions_timing"]["data"]

        def find(name):
            return next((f for f in fns if f["name"] == name or f["name"].endswith("::" + name)), None)

        render, transcript, row = find("Reader::render"), find("chat::transcript"), find("chat::agent_row")
        frames = render["calls"] if render else 0
        engine = [t for t in report["threads"]["data"] if t["name"].startswith("nsh-engine")]
        alloc = sum(size(t["alloc_bytes"]) for t in engine)
        cpu = max((float(t["cpu_percent_avg"].rstrip("%")) for t in engine), default=0.0)
        p99 = millis(render["p99"]) if render else 0.0
        print(
            f"{history:>7} {frames:>7} {transcript['calls'] if transcript else 0:>11} {row['calls'] if row else 0:>10} "
            f"{mem(alloc):>13} {mem(alloc / max(frames, 1)):>10} {cpu:>9.1f}% {p99:>8.2f}ms"
        )


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2:])
