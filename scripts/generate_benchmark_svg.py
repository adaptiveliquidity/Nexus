#!/usr/bin/env python3
"""Generate docs/benchmark-chart.svg from Criterion benchmark output.

Called by CI after benchmarks run on main. Parses the Criterion raw log
to extract median timings and renders a log-scale horizontal bar chart
SVG that GitHub renders in both light and dark mode.

Usage:
    python scripts/generate_benchmark_svg.py benchmark_evidence/criterion_raw.log
"""

import math
import re
import sys
from datetime import datetime, timezone


BENCHMARKS = [
    ("cold_start/sandbox_new", "Cold start", "bar-cold"),
    ("snapshot_rollback/size/1MiB", "Rollback 1 MiB", "bar-rollback"),
    ("snapshot_rollback/size/10MiB", "Rollback 10 MiB", "bar-rollback"),
    ("execute_tool/trivial_wasm_start", "Execute tool", "bar-execute"),
    ("snapshot_create/size/1MiB", "Snapshot 1 MiB", "bar-snapshot"),
    ("snapshot_rollback/size/100MiB", "Rollback 100 MiB", "bar-rollback"),
    ("snapshot_create/size/100MiB", "Snapshot 100 MiB", "bar-snapshot"),
]

METRIC_CARDS = [
    ("cold_start/sandbox_new", "Cold start"),
    ("snapshot_rollback/size/1MiB", "Rollback (1 MiB)"),
    ("execute_tool/trivial_wasm_start", "Execute tool"),
    ("snapshot_create/size/1MiB", "Snapshot (1 MiB)"),
]


def parse_criterion_log(path):
    """Parse Criterion output, return {benchmark_name: median_us}."""
    results = {}
    with open(path) as f:
        text = f.read()

    pattern = re.compile(
        r"^([\w/]+)\s+"
        r"time:\s*\[\s*([\d.]+)\s+([\w\xb5]+)\s+"
        r"([\d.]+)\s+([\w\xb5]+)\s+([\d.]+)\s+([\w\xb5]+)\s*\]",
        re.MULTILINE,
    )
    two_line = re.compile(
        r"^([\w/]+)\s*\n\s+"
        r"time:\s*\[\s*([\d.]+)\s+([\w\xb5]+)\s+"
        r"([\d.]+)\s+([\w\xb5]+)\s+([\d.]+)\s+([\w\xb5]+)\s*\]",
        re.MULTILINE,
    )
    for pat in [pattern, two_line]:
        for m in pat.finditer(text):
            name = m.group(1)
            if name in results:
                continue
            median_val = float(m.group(4))
            median_unit = m.group(5)
            results[name] = to_microseconds(median_val, median_unit)

    return results


def to_microseconds(value, unit):
    unit = unit.lower().strip()
    if unit in ("ns", "nanoseconds"):
        return value / 1000.0
    if unit in ("µs", "us", "microseconds"):
        return value
    if unit in ("ms", "milliseconds"):
        return value * 1000.0
    if unit in ("s", "seconds"):
        return value * 1_000_000.0
    raise ValueError(f"Unknown time unit: {unit}")


def format_time(us):
    if us < 1:
        return f"{us * 1000:.0f} ns"
    if us < 1000:
        if us < 10:
            return f"{us:.1f} µs"
        return f"{us:.0f} µs"
    ms = us / 1000
    if ms < 10:
        return f"{ms:.2f} ms"
    if ms < 100:
        return f"{ms:.1f} ms"
    return f"{ms:.0f} ms"


def log_width(us, scale=120):
    """Convert microseconds to pixel width on a log10 scale."""
    if us <= 0:
        return 1
    return max(1, int(math.log10(us) * scale))


def render_svg(results, date_str):
    bars = []
    for bench_key, label, css_class in BENCHMARKS:
        if bench_key in results:
            bars.append((label, results[bench_key], css_class))

    if not bars:
        print("WARNING: no benchmark results matched", file=sys.stderr)
        return None

    bar_count = len(bars)
    bar_h = 24
    bar_gap = 16
    chart_top = 150
    chart_h = bar_count * (bar_h + bar_gap) - bar_gap
    legend_y = chart_top + chart_h + 30
    svg_h = legend_y + 20
    chart_bottom = chart_top + chart_h

    cards_xml = []
    card_positions = [(20, 185), (215, 185), (410, 185), (605, 195)]
    for i, (bench_key, card_label) in enumerate(METRIC_CARDS):
        if bench_key not in results:
            continue
        us = results[bench_key]
        fmt = format_time(us)
        parts = fmt.split(" ", 1)
        val_str, unit_str = parts[0], parts[1] if len(parts) > 1 else ""
        cx, cw = card_positions[i]
        mid = cx + cw // 2
        cards_xml.append(
            f'  <rect class="metric-card" x="{cx}" y="64" width="{cw}" '
            f'height="62" rx="8" stroke-width="1"/>\n'
            f'  <text x="{mid}" y="86" text-anchor="middle" font-size="10" '
            f'class="metric-label">{card_label}</text>\n'
            f'  <text x="{mid - 10}" y="112" text-anchor="middle" '
            f'font-size="24" font-weight="600" class="metric-value">'
            f'{val_str}</text>\n'
            f'  <text x="{mid + 14}" y="112" text-anchor="start" '
            f'font-size="12" class="metric-unit">{unit_str}</text>'
        )

    grid_xml = []
    axis_labels_xml = []
    axis_labels = [
        (180, "1 µs"),
        (300, "10 µs"),
        (420, "100 µs"),
        (540, "1 ms"),
        (660, "10 ms"),
        (780, "100 ms"),
    ]
    for x, _ in axis_labels:
        grid_xml.append(
            f'  <line class="grid-line" x1="{x}" y1="{chart_top}" '
            f'x2="{x}" y2="{chart_bottom}" stroke-width="0.5"/>'
        )
    for x, lbl in axis_labels:
        axis_labels_xml.append(
            f'  <text x="{x}" y="{chart_bottom + 15}" text-anchor="middle" '
            f'font-size="10" class="axis-label">{lbl}</text>'
        )

    bars_xml = []
    for i, (label, us, css_class) in enumerate(bars):
        y = chart_top + i * (bar_h + bar_gap)
        w = log_width(us)
        fmt = format_time(us)
        text_x = 180 + w + 6
        font_sz = 10 if text_x + 40 < 790 else 9
        bars_xml.append(
            f'  <text x="172" y="{y + 18}" text-anchor="end" '
            f'font-size="11" class="label">{label}</text>\n'
            f'  <rect class="{css_class}" x="180" y="{y + 4}" '
            f'width="{w}" height="{bar_h}" rx="3"/>\n'
            f'  <text x="{text_x}" y="{y + 21}" text-anchor="start" '
            f'font-size="{font_sz}" font-weight="500" class="value">'
            f'{fmt}</text>'
        )

    legend_items = [
        (180, "bar-cold", "Cold start"),
        (280, "bar-rollback", "Rollback"),
        (370, "bar-execute", "Execute"),
        (460, "bar-snapshot", "Snapshot"),
    ]
    legend_xml = []
    for lx, lcls, ltxt in legend_items:
        legend_xml.append(
            f'  <rect class="{lcls}" x="{lx}" y="{legend_y}" '
            f'width="10" height="10" rx="2"/>\n'
            f'  <text x="{lx + 14}" y="{legend_y + 9}" font-size="10" '
            f'class="legend-text">{ltxt}</text>'
        )
    legend_xml.append(
        f'  <text x="780" y="{legend_y + 9}" text-anchor="end" '
        f'font-size="9" class="note">Log scale · {date_str}</text>'
    )

    style = """  <style>
    @media (prefers-color-scheme: dark) {
      .bg { fill: #161b22; }
      .title { fill: #e6edf3; }
      .subtitle { fill: #8b949e; }
      .label { fill: #c9d1d9; }
      .value { fill: #e6edf3; }
      .axis-line { stroke: #30363d; }
      .axis-label { fill: #8b949e; }
      .grid-line { stroke: #21262d; }
      .bar-cold { fill: #3fb950; }
      .bar-rollback { fill: #58a6ff; }
      .bar-execute { fill: #d2a8ff; }
      .bar-snapshot { fill: #f0883e; }
      .legend-text { fill: #c9d1d9; }
      .note { fill: #8b949e; }
      .metric-card { fill: #161b22; stroke: #30363d; }
      .metric-value { fill: #58a6ff; }
      .metric-label { fill: #8b949e; }
      .metric-unit { fill: #8b949e; }
    }
    @media (prefers-color-scheme: light) {
      .bg { fill: #ffffff; }
      .title { fill: #1f2328; }
      .subtitle { fill: #656d76; }
      .label { fill: #1f2328; }
      .value { fill: #1f2328; }
      .axis-line { stroke: #d0d7de; }
      .axis-label { fill: #656d76; }
      .grid-line { stroke: #eaeef2; }
      .bar-cold { fill: #1a7f37; }
      .bar-rollback { fill: #0969da; }
      .bar-execute { fill: #8250df; }
      .bar-snapshot { fill: #bf5700; }
      .legend-text { fill: #1f2328; }
      .note { fill: #656d76; }
      .metric-card { fill: #f6f8fa; stroke: #d0d7de; }
      .metric-value { fill: #0969da; }
      .metric-label { fill: #656d76; }
      .metric-unit { fill: #656d76; }
    }
  </style>"""

    parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 820 {svg_h}" '
        f'font-family="system-ui, -apple-system, \'Segoe UI\', sans-serif">',
        style,
        "",
        f'  <rect class="bg" width="820" height="{svg_h}" rx="12"/>',
        "",
        "  <!-- Title -->",
        '  <text x="410" y="32" text-anchor="middle" font-size="16" '
        'font-weight="600" class="title">Nexus benchmark results</text>',
        '  <text x="410" y="50" text-anchor="middle" font-size="11" '
        'class="subtitle">Criterion.rs on ubuntu-24.04 CI runners '
        '· lower is better</text>',
        "",
        "  <!-- Metric cards -->",
        "\n".join(cards_xml),
        "",
        "  <!-- Grid lines -->",
        "\n".join(grid_xml),
        "",
        "  <!-- Axis labels -->",
        "\n".join(axis_labels_xml),
        "",
        "  <!-- Axis line -->",
        f'  <line class="axis-line" x1="180" y1="{chart_bottom}" '
        f'x2="780" y2="{chart_bottom}" stroke-width="1"/>',
        "",
        "  <!-- Bars -->",
        "\n".join(bars_xml),
        "",
        "  <!-- Legend -->",
        "\n".join(legend_xml),
        "</svg>",
    ]
    return "\n".join(parts) + "\n"


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <criterion_raw.log>", file=sys.stderr)
        sys.exit(1)

    log_path = sys.argv[1]
    results = parse_criterion_log(log_path)

    if not results:
        print(f"ERROR: no benchmarks parsed from {log_path}", file=sys.stderr)
        sys.exit(1)

    print(f"Parsed {len(results)} benchmarks:", file=sys.stderr)
    for name, us in sorted(results.items()):
        print(f"  {name}: {format_time(us)}", file=sys.stderr)

    date_str = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    svg = render_svg(results, date_str)
    if svg is None:
        sys.exit(1)

    out_path = "docs/benchmark-chart.svg"
    if len(sys.argv) >= 3:
        out_path = sys.argv[2]

    with open(out_path, "w") as f:
        f.write(svg)
    print(f"Wrote {out_path}", file=sys.stderr)


if __name__ == "__main__":
    main()
