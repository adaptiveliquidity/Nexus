#!/usr/bin/env python3
"""Parse Criterion benchmark output into JSON for the dashboard build."""

import json
import re
import sys

BENCH_RE = re.compile(
    r"^(\S+)(?:[ \t]+|\s*\n\s+)time:\s+\[([^\]]+)\]",
    re.MULTILINE,
)
TIME_RE = re.compile(r"([\d.]+)\s+(ns|µs|µs|us|ms|s)")

UNIT_TO_NS = {"ns": 1, "µs": 1_000, "us": 1_000, "µs": 1_000, "ms": 1_000_000, "s": 1_000_000_000}


def parse_time(s: str) -> float | None:
    m = TIME_RE.search(s)
    if not m:
        return None
    return float(m.group(1)) * UNIT_TO_NS[m.group(2)]


def parse_criterion_log(text: str) -> list[dict]:
    results = []
    for m in BENCH_RE.finditer(text):
        name = m.group(1)
        times_str = m.group(2)
        matches = TIME_RE.findall(times_str)
        if len(matches) >= 3:
            low = float(matches[0][0]) * UNIT_TO_NS[matches[0][1]]
            mid = float(matches[1][0]) * UNIT_TO_NS[matches[1][1]]
            high = float(matches[2][0]) * UNIT_TO_NS[matches[2][1]]
        elif len(matches) >= 1:
            mid = float(matches[0][0]) * UNIT_TO_NS[matches[0][1]]
            low = high = mid
        else:
            continue

        group, _, bench = name.partition("/")
        results.append({
            "name": name,
            "group": group,
            "benchmark": bench or name,
            "ns": {"low": low, "median": mid, "high": high},
            "ms": {"low": low / 1e6, "median": mid / 1e6, "high": high / 1e6},
        })
    return results


def main():
    if len(sys.argv) < 2:
        print("Usage: criterion_to_json.py <criterion_raw.log> [output.json]", file=sys.stderr)
        sys.exit(1)

    with open(sys.argv[1], encoding="utf-8") as f:
        text = f.read()

    results = parse_criterion_log(text)
    if not results:
        print("WARNING: no benchmarks parsed", file=sys.stderr)

    output = sys.argv[2] if len(sys.argv) > 2 else "-"
    data = {"benchmarks": results, "count": len(results)}

    if output == "-":
        json.dump(data, sys.stdout, indent=2)
    else:
        with open(output, "w", encoding="utf-8") as f:
            json.dump(data, f, indent=2)
        print(f"Wrote {len(results)} benchmarks to {output}", file=sys.stderr)


if __name__ == "__main__":
    main()
