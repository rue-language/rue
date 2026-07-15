#!/usr/bin/env python3
"""
Generate SVG charts from benchmark history for the performance dashboard.

This script reads benchmark history from JSON and generates SVG charts:
1. timeline.svg - Time-series chart showing total compilation time over commits
2. breakdown.svg - Stacked bar chart showing time per compiler pass
3. memory.svg - Memory usage over time
4. binary_size.svg - Total binary size over time

Usage:
    # Generate charts for a single platform
    ./generate-charts.py <history-path> <output-dir> [--platform <name>]

    # Generate comparison charts from multiple platform histories
    ./generate-charts.py --comparison <output-dir> <history1> <history2> ...

Examples:
    # Single platform (legacy mode)
    ./generate-charts.py website/static/benchmarks/history.json website/static/benchmarks/

    # Per-platform generation
    ./generate-charts.py history-x86-64-linux.json platforms/x86-64-linux/ --platform x86-64-linux

    # Cross-platform comparison
    ./generate-charts.py --comparison comparison/ history-*.json
"""

import argparse
import json
import sys
from pathlib import Path
from typing import Optional

from benchmark_history import (
    comparison_break,
    latest_comparable_segment,
    load_history,
    run_regime,
)
from benchmark_metrics import derive_history_metrics

# Chart dimensions
TIMELINE_WIDTH = 800
TIMELINE_HEIGHT = 300
BREAKDOWN_WIDTH = 800
BREAKDOWN_HEIGHT = 350
MEMORY_WIDTH = 800
MEMORY_HEIGHT = 250
BINARY_WIDTH = 800
BINARY_HEIGHT = 250
COMPARISON_WIDTH = 900
COMPARISON_HEIGHT = 400

# Colors for passes (consistent with website theme)
HISTORICAL_SYMBOL_MERGE_PASS = "merge_" + "symbols"
PASS_COLORS = {
    "lexer": "#5c6b34",  # olive
    "parser": "#7d8f4a",  # moss
    "parse_file": "#5c6b34",  # historical combined lexer/parser
    "definition_snapshot": "#667a52",  # sage
    HISTORICAL_SYMBOL_MERGE_PASS: "#71813f",  # moss
    "parallel_astgen": "#5c6b34",  # historical olive
    "merge_rirs": "#71813f",  # historical moss
    "validate_and_generate_rir": "#8f984d",  # dry grass
    "astgen": "#a3a25b",  # dry grass
    "semantic_astgen": "#b7ad63",  # straw
    "rir_declaration_index": "#a98f55",  # muted gold
    "sema": "#c9970e",  # rue yellow
    "cfg": "#8a6d2f",  # ochre
    "cfg_construction": "#8a6d2f",  # ochre
    "codegen": "#a14e24",  # sienna
    "linker": "#6b4788",  # plum
}

# Nested aggregate spans would double-count their leaf timings in a breakdown.
AGGREGATE_PASSES = {"parse", "compile"}

# Current leaf order followed by historical names still present in old data.
# A chart only renders names present in the selected run, then appends unknown
# future leaves alphabetically so new instrumentation cannot disappear.
PASS_ORDER = [
    "lexer",
    "parser",
    "parse_file",
    "definition_snapshot",
    HISTORICAL_SYMBOL_MERGE_PASS,
    "parallel_astgen",
    "merge_rirs",
    "validate_and_generate_rir",
    "astgen",
    "semantic_astgen",
    "rir_declaration_index",
    "sema",
    "cfg_construction",
    "codegen",
    "linker",
    "cfg",
]

# Platform display names and colors
PLATFORM_INFO = {
    "x86-64-linux": {"name": "Linux x86-64", "color": "#5c6b34"},
    "aarch64-linux": {"name": "Linux ARM64", "color": "#c9970e"},
    "aarch64-macos": {"name": "macOS ARM64", "color": "#a14e24"},
}


def regime_summary(runs: list[dict]) -> list[dict]:
    """Describe contiguous comparable/non-comparable chart segments."""
    segments = []
    for index, run in enumerate(runs):
        regime = run_regime(run)
        if index == 0 or comparison_break(runs[index - 1], run):
            segments.append({
                "regime_id": regime,
                "comparable": regime != "unknown",
                "start_commit": short_commit(run.get("commit", "")),
                "end_commit": short_commit(run.get("commit", "")),
                "run_count": 1,
            })
        else:
            segments[-1]["end_commit"] = short_commit(run.get("commit", ""))
            segments[-1]["run_count"] += 1
    return segments


def point_segments(runs: list[dict], values: list[float]) -> list[list[tuple[int, float]]]:
    """Split nonzero chart points at missing data and comparison boundaries."""
    segments: list[list[tuple[int, float]]] = []
    current: list[tuple[int, float]] = []
    for index, (run, value) in enumerate(zip(runs, values)):
        previous = runs[index - 1] if index else None
        if comparison_break(previous, run) or value <= 0:
            if current:
                segments.append(current)
            current = []
        if value > 0:
            current.append((index, value))
    if current:
        segments.append(current)
    return segments


def get_pass_times(run: dict) -> dict[str, float]:
    """Sum each compiler pass across every benchmark in a run."""
    totals: dict[str, float] = {}
    for bench in run.get("benchmarks", []):
        for name, timing in bench.get("passes", {}).items():
            if name in AGGREGATE_PASSES:
                continue
            mean = timing.get("mean_ms", 0) if isinstance(timing, dict) else 0
            totals[name] = totals.get(name, 0) + mean
    return order_pass_times(totals)


def order_pass_times(passes: dict[str, float]) -> dict[str, float]:
    """Return pass timings in a stable known-first order."""
    names = [name for name in PASS_ORDER if name in passes]
    names.extend(sorted(set(passes) - set(PASS_ORDER)))
    return {name: passes[name] for name in names}


def benchmark_time(bench: dict) -> float:
    """Read one benchmark's mean time across current and legacy schemas."""
    # Before timing schema v2, mean_ms summed nested spans and was inflated.
    # Historical runs retained the honest root wall time as passes.compile.
    compile_timing = bench.get("passes", {}).get("compile")
    if isinstance(compile_timing, dict):
        compile_mean = compile_timing.get("mean_ms")
        if isinstance(compile_mean, (int, float)):
            return compile_mean
    if "mean_ms" in bench:
        return bench["mean_ms"]
    total = bench.get("total_ms", 0)
    return total.get("mean", 0) if isinstance(total, dict) else total


def get_total_time(run: dict) -> float:
    """Sum compilation time across every benchmark in a run."""
    return sum(benchmark_time(bench) for bench in run.get("benchmarks", []))


def get_peak_memory(run: dict) -> float:
    """Get the highest per-compile peak memory usage (in MB) in a run."""
    peak_bytes = max(
        (bench.get("peak_memory_bytes", 0) for bench in run.get("benchmarks", [])),
        default=0,
    )
    return peak_bytes / (1024 * 1024)


def get_binary_size(run: dict) -> float:
    """Get the combined size (in KB) of all benchmark output binaries."""
    total_bytes = sum(
        bench.get("binary_size_bytes", 0) for bench in run.get("benchmarks", [])
    )
    return total_bytes / 1024


def format_bytes(size_bytes: float) -> str:
    """Format bytes into human-readable form."""
    if size_bytes >= 1024 * 1024:
        return f"{size_bytes / (1024 * 1024):.1f}MB"
    elif size_bytes >= 1024:
        return f"{size_bytes / 1024:.1f}KB"
    else:
        return f"{size_bytes:.0f}B"


def calculate_delta(current: float, previous: float) -> tuple[float, str]:
    """Calculate delta and format as string with arrow indicator."""
    if previous == 0:
        return 0, ""
    delta = current - previous
    pct = (delta / previous) * 100
    if abs(pct) < 0.1:
        return pct, "→ 0%"
    arrow = "↑" if pct > 0 else "↓"
    return pct, f"{arrow} {abs(pct):.1f}%"


def format_delta_pct(pct: float) -> str:
    """Format an already-computed canonical percentage without reclassifying it."""
    if pct == 0:
        return "→ 0%"
    return f"{'↑' if pct > 0 else '↓'} {abs(pct):.1f}%"


def escape_xml(s: str) -> str:
    """Escape special XML characters."""
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def short_commit(commit: str) -> str:
    """Get short commit hash."""
    if commit and len(commit) >= 7:
        return commit[:7]
    return commit or "?"


def generate_empty_chart(width: int, height: int, message: str) -> str:
    """Generate an SVG chart showing a message when no data is available."""
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" class="benchmark-chart">
  <style>
    .chart-bg {{ fill: var(--chart-bg, #f9f6ee); }}
    .chart-text {{ fill: var(--chart-text, #57543f); font-family: system-ui, sans-serif; }}
    @media (prefers-color-scheme: dark) {{
      .chart-bg {{ fill: #1a1a1a; }}
      .chart-text {{ fill: #9ca3af; }}
    }}
  </style>
  <rect class="chart-bg" width="{width}" height="{height}" rx="8"/>
  <text class="chart-text" x="{width/2}" y="{height/2}" text-anchor="middle" font-size="14">{escape_xml(message)}</text>
</svg>'''


def generate_timeline_chart(runs: list[dict], platform: Optional[str] = None) -> str:
    """Generate time-series SVG chart of total compilation time."""
    if not runs:
        return generate_empty_chart(TIMELINE_WIDTH, TIMELINE_HEIGHT, "No benchmark data available yet")

    # Extract data points
    points = []
    recent_runs = runs[-20:]
    for index, run in enumerate(recent_runs):  # Show last 20 commits
        total = get_total_time(run)
        commit = short_commit(run.get("commit", ""))
        previous = recent_runs[index - 1] if index else None
        points.append({"commit": commit, "time": total, "break": comparison_break(previous, run)})

    if not points or all(p["time"] == 0 for p in points):
        return generate_empty_chart(TIMELINE_WIDTH, TIMELINE_HEIGHT, "No timing data in benchmarks")

    # Chart layout
    margin = {"top": 40, "right": 30, "bottom": 60, "left": 70}
    chart_width = TIMELINE_WIDTH - margin["left"] - margin["right"]
    chart_height = TIMELINE_HEIGHT - margin["top"] - margin["bottom"]

    # Scale calculations
    max_time = max(p["time"] for p in points) * 1.1  # 10% padding
    if max_time == 0:
        max_time = 1  # Avoid division by zero

    def scale_x(i: int) -> float:
        if len(points) == 1:
            return margin["left"] + chart_width / 2
        return margin["left"] + (i / (len(points) - 1)) * chart_width

    def scale_y(v: float) -> float:
        return margin["top"] + chart_height - (v / max_time) * chart_height

    # Title with optional platform
    title = "Compilation Time Over Recent Commits"
    if platform:
        platform_name = PLATFORM_INFO.get(platform, {}).get("name", platform)
        title = f"{title} ({platform_name})"

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {TIMELINE_WIDTH} {TIMELINE_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #f9f6ee); }
    .chart-text { fill: var(--chart-text, #57543f); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #23271c); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-line { stroke: var(--chart-accent, #5c6b34); fill: none; stroke-width: 2; }
    .chart-point { fill: var(--chart-accent, #5c6b34); }
    .chart-grid { stroke: var(--chart-grid, #d3cab0); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #8a8468); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{TIMELINE_WIDTH}" height="{TIMELINE_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{TIMELINE_WIDTH/2}" y="25" text-anchor="middle" font-size="16">{escape_xml(title)}</text>',
    ]

    # Y-axis grid lines and labels
    num_grid_lines = 5
    for i in range(num_grid_lines + 1):
        y = margin["top"] + (i / num_grid_lines) * chart_height
        value = max_time * (1 - i / num_grid_lines)
        svg_parts.append(
            f'  <line class="chart-grid" x1="{margin["left"]}" y1="{y}" x2="{TIMELINE_WIDTH - margin["right"]}" y2="{y}"/>'
        )
        svg_parts.append(
            f'  <text class="chart-text" x="{margin["left"] - 10}" y="{y + 4}" text-anchor="end" font-size="11">{value:.1f}ms</text>'
        )

    # Axes
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"]}" x2="{margin["left"]}" y2="{TIMELINE_HEIGHT - margin["bottom"]}"/>'
    )
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{TIMELINE_HEIGHT - margin["bottom"]}" x2="{TIMELINE_WIDTH - margin["right"]}" y2="{TIMELINE_HEIGHT - margin["bottom"]}"/>'
    )

    # Draw line connecting points
    segment = []
    for index, point in enumerate(points):
        if point["break"] and segment:
            if len(segment) > 1:
                svg_parts.append('  <path class="chart-line" d="M ' + " L ".join(segment) + '"/>')
            segment = []
        segment.append(f"{scale_x(index)},{scale_y(point['time'])}")
    if len(segment) > 1:
        svg_parts.append('  <path class="chart-line" d="M ' + " L ".join(segment) + '"/>')

    # Draw points and x-axis labels
    for i, p in enumerate(points):
        x = scale_x(i)
        y = scale_y(p["time"])
        svg_parts.append(f'  <circle class="chart-point" cx="{x}" cy="{y}" r="4"/>')

        # X-axis label (rotated for readability)
        label_y = TIMELINE_HEIGHT - margin["bottom"] + 15
        svg_parts.append(
            f'  <text class="chart-text" x="{x}" y="{label_y}" text-anchor="end" font-size="10" transform="rotate(-45 {x} {label_y})">{escape_xml(p["commit"])}</text>'
        )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)


def get_benchmark_names(runs: list[dict]) -> list[str]:
    """Get list of all benchmark names from runs."""
    names = set()
    for run in runs:
        for bench in run.get("benchmarks", []):
            if "name" in bench:
                names.add(bench["name"])
    return sorted(names)


# Colors for different benchmark programs
BENCHMARK_COLORS = [
    "#4f6ddb",  # blue
    "#10b981",  # emerald
    "#f59e0b",  # amber
    "#ef4444",  # red
    "#8b5cf6",  # violet
    "#06b6d4",  # cyan
    "#ec4899",  # pink
]


def get_benchmark_time(run: dict, benchmark_name: str) -> float:
    """Get timing for a specific benchmark from a run."""
    for bench in run.get("benchmarks", []):
        if bench.get("name") == benchmark_name:
            return benchmark_time(bench)
    return 0


def generate_multi_timeline_chart(runs: list[dict], benchmark_names: list[str]) -> str:
    """Generate time-series SVG chart showing each benchmark program as a separate line."""
    if not runs or not benchmark_names:
        return generate_empty_chart(TIMELINE_WIDTH, TIMELINE_HEIGHT + 50, "No benchmark data available yet")

    # Extract data points for each benchmark
    recent_runs = runs[-20:]
    commits = [short_commit(run.get("commit", "")) for run in recent_runs]
    benchmark_data = {}

    for name in benchmark_names:
        points = []
        for run in recent_runs:
            time = get_benchmark_time(run, name)
            points.append(time)
        benchmark_data[name] = points

    # Check if we have any data
    all_times = [t for pts in benchmark_data.values() for t in pts]
    if not all_times or all(t == 0 for t in all_times):
        return generate_empty_chart(TIMELINE_WIDTH, TIMELINE_HEIGHT + 50, "No timing data in benchmarks")

    # Chart layout (taller to accommodate legend)
    height = TIMELINE_HEIGHT + 80
    margin = {"top": 40, "right": 30, "bottom": 60, "left": 70}
    chart_width = TIMELINE_WIDTH - margin["left"] - margin["right"]
    chart_height = TIMELINE_HEIGHT - margin["top"] - margin["bottom"]

    # Scale calculations
    max_time = max(all_times) * 1.1  # 10% padding
    if max_time == 0:
        max_time = 1

    def scale_x(i: int) -> float:
        if len(commits) == 1:
            return margin["left"] + chart_width / 2
        return margin["left"] + (i / (len(commits) - 1)) * chart_width

    def scale_y(v: float) -> float:
        return margin["top"] + chart_height - (v / max_time) * chart_height

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {TIMELINE_WIDTH} {height}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #f9f6ee); }
    .chart-text { fill: var(--chart-text, #57543f); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #23271c); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-grid { stroke: var(--chart-grid, #d3cab0); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #8a8468); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{TIMELINE_WIDTH}" height="{height}" rx="8"/>',
        f'  <text class="chart-title" x="{TIMELINE_WIDTH/2}" y="25" text-anchor="middle" font-size="16">Compilation Time by Program</text>',
    ]

    # Y-axis grid lines and labels
    num_grid_lines = 5
    for i in range(num_grid_lines + 1):
        y = margin["top"] + (i / num_grid_lines) * chart_height
        value = max_time * (1 - i / num_grid_lines)
        svg_parts.append(
            f'  <line class="chart-grid" x1="{margin["left"]}" y1="{y}" x2="{TIMELINE_WIDTH - margin["right"]}" y2="{y}"/>'
        )
        svg_parts.append(
            f'  <text class="chart-text" x="{margin["left"] - 10}" y="{y + 4}" text-anchor="end" font-size="11">{value:.1f}ms</text>'
        )

    # Axes
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"]}" x2="{margin["left"]}" y2="{TIMELINE_HEIGHT - margin["bottom"]}"/>'
    )
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{TIMELINE_HEIGHT - margin["bottom"]}" x2="{TIMELINE_WIDTH - margin["right"]}" y2="{TIMELINE_HEIGHT - margin["bottom"]}"/>'
    )

    # Draw lines and points for each benchmark
    for idx, name in enumerate(benchmark_names):
        color = BENCHMARK_COLORS[idx % len(BENCHMARK_COLORS)]
        points = benchmark_data[name]

        # Draw connecting line
        if len(points) > 1:
            for segment in point_segments(recent_runs, points):
                if len(segment) < 2:
                    continue
                path_d = "M " + " L ".join(
                    f"{scale_x(i)},{scale_y(time)}" for i, time in segment
                )
                svg_parts.append(f'  <path d="{path_d}" fill="none" stroke="{color}" stroke-width="2"/>')

        # Draw points
        for i, time in enumerate(points):
            if time > 0:
                x = scale_x(i)
                y = scale_y(time)
                svg_parts.append(f'  <circle cx="{x}" cy="{y}" r="3" fill="{color}"/>')

    # X-axis labels (commits)
    for i, commit in enumerate(commits):
        x = scale_x(i)
        label_y = TIMELINE_HEIGHT - margin["bottom"] + 15
        svg_parts.append(
            f'  <text class="chart-text" x="{x}" y="{label_y}" text-anchor="end" font-size="10" transform="rotate(-45 {x} {label_y})">{escape_xml(commit)}</text>'
        )

    # Legend at bottom
    legend_y = TIMELINE_HEIGHT + 10
    legend_x_start = margin["left"]
    for idx, name in enumerate(benchmark_names):
        color = BENCHMARK_COLORS[idx % len(BENCHMARK_COLORS)]
        x = legend_x_start + (idx % 3) * 200
        y = legend_y + (idx // 3) * 20
        svg_parts.append(f'  <rect x="{x}" y="{y}" width="12" height="12" fill="{color}" rx="2"/>')
        svg_parts.append(
            f'  <text class="chart-text" x="{x + 18}" y="{y + 10}" font-size="11">{escape_xml(name)}</text>'
        )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)


def get_pass_times_for_benchmark(run: dict, benchmark_name: str) -> dict[str, float]:
    """Extract pass timing for a specific benchmark from a run."""
    for bench in run.get("benchmarks", []):
        if bench.get("name") == benchmark_name and "passes" in bench:
            passes = {
                name: timing.get("mean_ms", 0)
                for name, timing in bench["passes"].items()
                if isinstance(timing, dict) and name not in AGGREGATE_PASSES
            }
            return order_pass_times(passes)
    return {}


def generate_breakdown_chart(runs: list[dict], benchmark_name: Optional[str] = None, platform: Optional[str] = None) -> str:
    """Generate stacked bar chart showing time per compiler pass.

    If benchmark_name is provided, shows data for that specific benchmark.
    Otherwise, shows aggregate data across all benchmarks.
    """
    if not runs:
        return generate_empty_chart(BREAKDOWN_WIDTH, BREAKDOWN_HEIGHT, "No benchmark data available yet")

    # Get the most recent run with pass data
    pass_times: Optional[dict[str, float]] = None
    commit = ""
    for run in reversed(runs):
        if benchmark_name:
            pt = get_pass_times_for_benchmark(run, benchmark_name)
        else:
            pt = get_pass_times(run)
        if pt and any(v > 0 for v in pt.values()):
            pass_times = pt
            commit = short_commit(run.get("commit", ""))
            break

    if not pass_times or all(v == 0 for v in pass_times.values()):
        return generate_empty_chart(BREAKDOWN_WIDTH, BREAKDOWN_HEIGHT, "No pass timing data available")

    # Chart layout
    margin = {"top": 50, "right": 150, "bottom": 40, "left": 70}
    chart_width = BREAKDOWN_WIDTH - margin["left"] - margin["right"]
    chart_height = BREAKDOWN_HEIGHT - margin["top"] - margin["bottom"]

    total = sum(pass_times.values())
    if total == 0:
        total = 1

    # Build title
    title_parts = ["Compilation Time by Pass"]
    if benchmark_name:
        title_parts.append(f" - {benchmark_name}")
    if platform:
        platform_name = PLATFORM_INFO.get(platform, {}).get("name", platform)
        title_parts.append(f" ({platform_name})")
    title = "".join(title_parts)

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {BREAKDOWN_WIDTH} {BREAKDOWN_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #f9f6ee); }
    .chart-text { fill: var(--chart-text, #57543f); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #23271c); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-grid { stroke: var(--chart-grid, #d3cab0); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #8a8468); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{BREAKDOWN_WIDTH}" height="{BREAKDOWN_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{BREAKDOWN_WIDTH/2}" y="25" text-anchor="middle" font-size="16">{escape_xml(title)}</text>',
        f'  <text class="chart-text" x="{BREAKDOWN_WIDTH/2}" y="42" text-anchor="middle" font-size="11">(commit: {escape_xml(commit)})</text>',
    ]

    # Horizontal stacked bar
    bar_height = 40
    bar_y = margin["top"] + (chart_height - bar_height) / 2
    x_offset = margin["left"]

    for pass_name in pass_times:
        time = pass_times.get(pass_name, 0)
        width = (time / total) * chart_width if time > 0 else 0
        color = PASS_COLORS.get(pass_name, "#888888")

        if width > 0:
            svg_parts.append(
                f'  <rect x="{x_offset}" y="{bar_y}" width="{width}" height="{bar_height}" fill="{color}" rx="2"/>'
            )
            # Add time label if bar is wide enough
            if width > 30:
                svg_parts.append(
                    f'  <text x="{x_offset + width/2}" y="{bar_y + bar_height/2 + 4}" text-anchor="middle" font-size="10" fill="white">{time:.1f}ms</text>'
                )
            x_offset += width

    # Legend
    legend_x = BREAKDOWN_WIDTH - margin["right"] + 20
    legend_y = margin["top"] + 20

    for i, pass_name in enumerate(pass_times):
        y = legend_y + i * 22
        color = PASS_COLORS.get(pass_name, "#888888")
        time = pass_times.get(pass_name, 0)
        pct = (time / total) * 100

        svg_parts.append(f'  <rect x="{legend_x}" y="{y}" width="12" height="12" fill="{color}" rx="2"/>')
        svg_parts.append(
            f'  <text class="chart-text" x="{legend_x + 18}" y="{y + 10}" font-size="11">{pass_name} ({pct:.0f}%)</text>'
        )

    # Total time annotation
    svg_parts.append(
        f'  <text class="chart-text" x="{margin["left"]}" y="{bar_y + bar_height + 25}" font-size="12">Total: {total:.1f}ms</text>'
    )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)


def generate_memory_chart(runs: list[dict], platform: Optional[str] = None) -> str:
    """Generate time-series SVG chart of peak memory usage."""
    if not runs:
        return generate_empty_chart(MEMORY_WIDTH, MEMORY_HEIGHT, "No benchmark data available yet")

    # Extract data points
    points = []
    recent_runs = runs[-20:]
    for run in recent_runs:  # Show last 20 commits
        memory = get_peak_memory(run)
        commit = short_commit(run.get("commit", ""))
        points.append({"commit": commit, "memory": memory})

    if not points or all(p["memory"] == 0 for p in points):
        return generate_empty_chart(MEMORY_WIDTH, MEMORY_HEIGHT, "No memory data in benchmarks")

    # Chart layout
    margin = {"top": 40, "right": 30, "bottom": 60, "left": 70}
    chart_width = MEMORY_WIDTH - margin["left"] - margin["right"]
    chart_height = MEMORY_HEIGHT - margin["top"] - margin["bottom"]

    # Scale calculations
    max_memory = max(p["memory"] for p in points) * 1.1  # 10% padding
    if max_memory == 0:
        max_memory = 1  # Avoid division by zero

    def scale_x(i: int) -> float:
        if len(points) == 1:
            return margin["left"] + chart_width / 2
        return margin["left"] + (i / (len(points) - 1)) * chart_width

    def scale_y(v: float) -> float:
        return margin["top"] + chart_height - (v / max_memory) * chart_height

    # Title with optional platform
    title = "Peak Memory Usage Over Recent Commits"
    if platform:
        platform_name = PLATFORM_INFO.get(platform, {}).get("name", platform)
        title = f"{title} ({platform_name})"

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {MEMORY_WIDTH} {MEMORY_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #f9f6ee); }
    .chart-text { fill: var(--chart-text, #57543f); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #23271c); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-line { stroke: var(--chart-memory, #10b981); fill: none; stroke-width: 2; }
    .chart-point { fill: var(--chart-memory, #10b981); }
    .chart-grid { stroke: var(--chart-grid, #d3cab0); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #8a8468); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{MEMORY_WIDTH}" height="{MEMORY_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{MEMORY_WIDTH/2}" y="25" text-anchor="middle" font-size="16">{escape_xml(title)}</text>',
    ]

    # Y-axis grid lines and labels
    num_grid_lines = 4
    for i in range(num_grid_lines + 1):
        y = margin["top"] + (i / num_grid_lines) * chart_height
        value = max_memory * (1 - i / num_grid_lines)
        svg_parts.append(
            f'  <line class="chart-grid" x1="{margin["left"]}" y1="{y}" x2="{MEMORY_WIDTH - margin["right"]}" y2="{y}"/>'
        )
        svg_parts.append(
            f'  <text class="chart-text" x="{margin["left"] - 10}" y="{y + 4}" text-anchor="end" font-size="11">{value:.1f}MB</text>'
        )

    # Axes
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"]}" x2="{margin["left"]}" y2="{MEMORY_HEIGHT - margin["bottom"]}"/>'
    )
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{MEMORY_HEIGHT - margin["bottom"]}" x2="{MEMORY_WIDTH - margin["right"]}" y2="{MEMORY_HEIGHT - margin["bottom"]}"/>'
    )

    # Draw line connecting points
    for segment in point_segments(recent_runs, [point["memory"] for point in points]):
        if len(segment) > 1:
            path_d = "M " + " L ".join(f"{scale_x(i)},{scale_y(value)}" for i, value in segment)
            svg_parts.append(f'  <path class="chart-line" d="{path_d}"/>')

    # Draw points and x-axis labels
    for i, p in enumerate(points):
        x = scale_x(i)
        if p["memory"] > 0:
            y = scale_y(p["memory"])
            svg_parts.append(f'  <circle class="chart-point" cx="{x}" cy="{y}" r="4"/>')

        # X-axis label (rotated for readability)
        label_y = MEMORY_HEIGHT - margin["bottom"] + 15
        svg_parts.append(
            f'  <text class="chart-text" x="{x}" y="{label_y}" text-anchor="end" font-size="10" transform="rotate(-45 {x} {label_y})">{escape_xml(p["commit"])}</text>'
        )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)


def generate_binary_size_chart(runs: list[dict], platform: Optional[str] = None) -> str:
    """Generate time-series SVG chart of binary size."""
    if not runs:
        return generate_empty_chart(BINARY_WIDTH, BINARY_HEIGHT, "No benchmark data available yet")

    # Extract data points
    points = []
    recent_runs = runs[-20:]
    for run in recent_runs:  # Show last 20 commits
        size = get_binary_size(run)
        commit = short_commit(run.get("commit", ""))
        points.append({"commit": commit, "size": size})

    if not points or all(p["size"] == 0 for p in points):
        return generate_empty_chart(BINARY_WIDTH, BINARY_HEIGHT, "No binary size data in benchmarks")

    # Chart layout
    margin = {"top": 40, "right": 30, "bottom": 60, "left": 70}
    chart_width = BINARY_WIDTH - margin["left"] - margin["right"]
    chart_height = BINARY_HEIGHT - margin["top"] - margin["bottom"]

    # Scale calculations
    max_size = max(p["size"] for p in points) * 1.1  # 10% padding
    if max_size == 0:
        max_size = 1  # Avoid division by zero

    def scale_x(i: int) -> float:
        if len(points) == 1:
            return margin["left"] + chart_width / 2
        return margin["left"] + (i / (len(points) - 1)) * chart_width

    def scale_y(v: float) -> float:
        return margin["top"] + chart_height - (v / max_size) * chart_height

    # Title with optional platform
    title = "Total Binary Size Over Recent Commits"
    if platform:
        platform_name = PLATFORM_INFO.get(platform, {}).get("name", platform)
        title = f"{title} ({platform_name})"

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {BINARY_WIDTH} {BINARY_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #f9f6ee); }
    .chart-text { fill: var(--chart-text, #57543f); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #23271c); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-line { stroke: var(--chart-binary, #f59e0b); fill: none; stroke-width: 2; }
    .chart-point { fill: var(--chart-binary, #f59e0b); }
    .chart-grid { stroke: var(--chart-grid, #d3cab0); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #8a8468); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{BINARY_WIDTH}" height="{BINARY_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{BINARY_WIDTH/2}" y="25" text-anchor="middle" font-size="16">{escape_xml(title)}</text>',
    ]

    # Y-axis grid lines and labels
    num_grid_lines = 4
    for i in range(num_grid_lines + 1):
        y = margin["top"] + (i / num_grid_lines) * chart_height
        value = max_size * (1 - i / num_grid_lines)
        svg_parts.append(
            f'  <line class="chart-grid" x1="{margin["left"]}" y1="{y}" x2="{BINARY_WIDTH - margin["right"]}" y2="{y}"/>'
        )
        svg_parts.append(
            f'  <text class="chart-text" x="{margin["left"] - 10}" y="{y + 4}" text-anchor="end" font-size="11">{value:.1f}KB</text>'
        )

    # Axes
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"]}" x2="{margin["left"]}" y2="{BINARY_HEIGHT - margin["bottom"]}"/>'
    )
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{BINARY_HEIGHT - margin["bottom"]}" x2="{BINARY_WIDTH - margin["right"]}" y2="{BINARY_HEIGHT - margin["bottom"]}"/>'
    )

    # Draw line connecting points
    for segment in point_segments(recent_runs, [point["size"] for point in points]):
        if len(segment) > 1:
            path_d = "M " + " L ".join(f"{scale_x(i)},{scale_y(value)}" for i, value in segment)
            svg_parts.append(f'  <path class="chart-line" d="{path_d}"/>')

    # Draw points and x-axis labels
    for i, p in enumerate(points):
        x = scale_x(i)
        if p["size"] > 0:
            y = scale_y(p["size"])
            svg_parts.append(f'  <circle class="chart-point" cx="{x}" cy="{y}" r="4"/>')

        # X-axis label (rotated for readability)
        label_y = BINARY_HEIGHT - margin["bottom"] + 15
        svg_parts.append(
            f'  <text class="chart-text" x="{x}" y="{label_y}" text-anchor="end" font-size="10" transform="rotate(-45 {x} {label_y})">{escape_xml(p["commit"])}</text>'
        )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)


def generate_comparison_timeline_chart(platform_data: dict[str, list[dict]]) -> str:
    """Generate a comparison chart showing all platforms on the same timeline."""
    if not platform_data or all(not runs for runs in platform_data.values()):
        return generate_empty_chart(COMPARISON_WIDTH, COMPARISON_HEIGHT, "No benchmark data available")

    # Build a unified commit timeline from all platforms
    commit_to_times: dict[str, dict[str, float]] = {}

    for platform, runs in platform_data.items():
        for run in runs[-20:]:
            commit = short_commit(run.get("commit", ""))
            time = get_total_time(run)
            if commit not in commit_to_times:
                commit_to_times[commit] = {}
            commit_to_times[commit][platform] = time

    if not commit_to_times:
        return generate_empty_chart(COMPARISON_WIDTH, COMPARISON_HEIGHT, "No timing data available")

    # Sort commits (we'll use order of appearance in first platform that has data)
    commits = list(commit_to_times.keys())[-20:]  # Last 20 unique commits
    platforms = list(platform_data.keys())

    # Find max time across all platforms
    all_times = [t for times in commit_to_times.values() for t in times.values() if t > 0]
    if not all_times:
        return generate_empty_chart(COMPARISON_WIDTH, COMPARISON_HEIGHT, "No timing data available")
    max_time = max(all_times) * 1.1

    # Chart layout
    margin = {"top": 50, "right": 150, "bottom": 70, "left": 70}
    chart_width = COMPARISON_WIDTH - margin["left"] - margin["right"]
    chart_height = COMPARISON_HEIGHT - margin["top"] - margin["bottom"] - 50  # Room for legend

    def scale_x(i: int) -> float:
        if len(commits) == 1:
            return margin["left"] + chart_width / 2
        return margin["left"] + (i / (len(commits) - 1)) * chart_width

    def scale_y(v: float) -> float:
        return margin["top"] + chart_height - (v / max_time) * chart_height

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {COMPARISON_WIDTH} {COMPARISON_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #f9f6ee); }
    .chart-text { fill: var(--chart-text, #57543f); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #23271c); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-grid { stroke: var(--chart-grid, #d3cab0); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #8a8468); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{COMPARISON_WIDTH}" height="{COMPARISON_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{COMPARISON_WIDTH/2}" y="25" text-anchor="middle" font-size="16">Compilation Time - All Platforms</text>',
    ]

    # Y-axis grid lines and labels
    num_grid_lines = 5
    for i in range(num_grid_lines + 1):
        y = margin["top"] + (i / num_grid_lines) * chart_height
        value = max_time * (1 - i / num_grid_lines)
        svg_parts.append(
            f'  <line class="chart-grid" x1="{margin["left"]}" y1="{y}" x2="{COMPARISON_WIDTH - margin["right"]}" y2="{y}"/>'
        )
        svg_parts.append(
            f'  <text class="chart-text" x="{margin["left"] - 10}" y="{y + 4}" text-anchor="end" font-size="11">{value:.1f}ms</text>'
        )

    # Axes
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"]}" x2="{margin["left"]}" y2="{margin["top"] + chart_height}"/>'
    )
    svg_parts.append(
        f'  <line class="chart-axis" x1="{margin["left"]}" y1="{margin["top"] + chart_height}" x2="{COMPARISON_WIDTH - margin["right"]}" y2="{margin["top"] + chart_height}"/>'
    )

    # Draw lines for each platform
    for platform in platforms:
        info = PLATFORM_INFO.get(platform, {"name": platform, "color": "#888888"})
        color = info["color"]

        # Collect points for this platform
        line_segments = []
        line_points = []
        runs_by_commit = {
            short_commit(run.get("commit", "")): run
            for run in platform_data[platform][-20:]
        }
        previous_run = None
        for i, commit in enumerate(commits):
            time = commit_to_times.get(commit, {}).get(platform, 0)
            run = runs_by_commit.get(commit)
            if run is None or time <= 0 or comparison_break(previous_run, run):
                if line_points:
                    line_segments.append(line_points)
                line_points = []
            if run is not None and time > 0:
                line_points.append((i, time))
                previous_run = run
            else:
                previous_run = None
        if line_points:
            line_segments.append(line_points)

        # Draw connecting line
        for segment in line_segments:
            if len(segment) > 1:
                path_d = "M " + " L ".join(
                    f"{scale_x(i)},{scale_y(t)}" for i, t in segment
                )
                svg_parts.append(f'  <path d="{path_d}" fill="none" stroke="{color}" stroke-width="2"/>')

        # Draw points
        for segment in line_segments:
            for i, t in segment:
                svg_parts.append(f'  <circle cx="{scale_x(i)}" cy="{scale_y(t)}" r="4" fill="{color}"/>')

    # X-axis labels
    for i, commit in enumerate(commits):
        x = scale_x(i)
        label_y = margin["top"] + chart_height + 15
        svg_parts.append(
            f'  <text class="chart-text" x="{x}" y="{label_y}" text-anchor="end" font-size="9" transform="rotate(-45 {x} {label_y})">{escape_xml(commit)}</text>'
        )

    # Legend
    legend_y = COMPARISON_HEIGHT - 35
    legend_x_start = margin["left"]
    for idx, platform in enumerate(platforms):
        info = PLATFORM_INFO.get(platform, {"name": platform, "color": "#888888"})
        x = legend_x_start + idx * 200
        svg_parts.append(f'  <rect x="{x}" y="{legend_y}" width="14" height="14" fill="{info["color"]}" rx="2"/>')
        svg_parts.append(
            f'  <text class="chart-text" x="{x + 20}" y="{legend_y + 11}" font-size="12">{escape_xml(info["name"])}</text>'
        )

    svg_parts.append("</svg>")
    return "\n".join(svg_parts)


def calculate_coverage_metrics(runs: list[dict]) -> dict:
    """Calculate benchmark coverage metrics.

    Returns coverage information including:
    - How many distinct commits have been benchmarked
    - The commit ranges covered by each benchmark run
    - Data gaps (periods without benchmarks)
    """
    if not runs:
        return {
            "measured_commit_count": 0,
            "skipped_commit_count": 0,
            "unknown_gap_count": 0,
            "run_count": 0,
            "runs": [],
        }

    measured_commits = set()
    skipped_commits = set()
    unknown_gaps = 0
    run_info = []

    for run in runs:
        publication = run.get("publication", {})
        coverage = publication.get("coverage", {}) if isinstance(publication, dict) else {}
        measured = coverage.get("measured_commit")
        if measured:
            measured_commits.add(measured)
        skipped = coverage.get("skipped_commits", [])
        if isinstance(skipped, list):
            skipped_commits.update(c for c in skipped if isinstance(c, str) and c)
        if coverage.get("gap_unknown") is True:
            unknown_gaps += 1

        run_info.append({
            "commit": short_commit(run.get("commit", "")),
            "timestamp": run.get("timestamp", ""),
            "represented_commits": coverage.get("represented_commits", []),
            "skipped_commits": skipped,
            "gap_unknown": coverage.get("gap_unknown", True),
            "reason": publication.get("trigger_reason", "unknown"),
            "regime_id": run_regime(run),
        })

    return {
        "measured_commit_count": len(measured_commits),
        "skipped_commit_count": len(skipped_commits),
        "unknown_gap_count": unknown_gaps,
        "run_count": len(runs),
        "runs": run_info[-20:]  # Last 20 runs for display
    }


def generate_summary_data(runs: list[dict], platform: Optional[str] = None) -> dict:
    """Generate summary statistics for the performance dashboard."""
    if not runs:
        return {}

    comparable_runs = latest_comparable_segment(runs)
    latest = comparable_runs[-1]
    previous = comparable_runs[-2] if len(comparable_runs) >= 2 else None

    # Get latest values
    latest_time = get_total_time(latest) if latest else 0
    latest_memory = get_peak_memory(latest) if latest else 0
    latest_binary = get_binary_size(latest) if latest else 0
    latest_commit = short_commit(latest.get("commit", "")) if latest else ""

    # Calculate deltas
    prev_memory = get_peak_memory(previous) if previous else 0
    prev_binary = get_binary_size(previous) if previous else 0

    derived = derive_history_metrics(runs)
    time_comparison = derived["points"][-1]["previous"]
    canonical_headline = time_comparison.get("headline", {})
    time_delta_pct = canonical_headline.get("delta_pct", 0)
    time_delta_str = (
        format_delta_pct(time_delta_pct)
        if time_comparison.get("status") == "comparable"
        and canonical_headline.get("classification") != "insufficient_data"
        else ""
    )
    memory_delta_pct, memory_delta_str = calculate_delta(latest_memory, prev_memory)
    binary_delta_pct, binary_delta_str = calculate_delta(latest_binary, prev_binary)

    # Calculate 7-run average (or whatever we have)
    recent_runs = comparable_runs[-7:]
    avg_time = sum(get_total_time(r) for r in recent_runs) / len(recent_runs) if recent_runs else 0
    avg_memory = sum(get_peak_memory(r) for r in recent_runs) / len(recent_runs) if recent_runs else 0

    # Find best ever
    all_times = [get_total_time(r) for r in comparable_runs if get_total_time(r) > 0]
    best_time = min(all_times) if all_times else 0

    result = {
        "latest_commit": latest_commit,
        "latest_time_ms": round(latest_time, 2),
        "latest_memory_mb": round(latest_memory, 2),
        "latest_binary_kb": round(latest_binary, 2),
        "time_delta_pct": round(time_delta_pct, 2),
        "time_delta_str": time_delta_str,
        "time_comparison": time_comparison,
        "memory_delta_pct": round(memory_delta_pct, 2),
        "memory_delta_str": memory_delta_str,
        "binary_delta_pct": round(binary_delta_pct, 2),
        "binary_delta_str": binary_delta_str,
        "avg_time_ms": round(avg_time, 2),
        "avg_memory_mb": round(avg_memory, 2),
        "best_time_ms": round(best_time, 2),
        "run_count": len(runs),
        "comparable_run_count": len(comparable_runs),
    }

    if platform:
        result["platform"] = platform
        info = PLATFORM_INFO.get(platform, {})
        result["platform_name"] = info.get("name", platform)

    return result


def generate_platform_charts(history_path: Path, output_dir: Path, platform: Optional[str] = None):
    """Generate all charts for a single platform."""
    # Load history
    history = load_history(history_path)
    runs = history.get("runs", [])

    print(f"Loaded {len(runs)} benchmark runs from {history_path}")
    if platform:
        print(f"Generating charts for platform: {platform}")

    # Ensure output directory exists
    output_dir.mkdir(parents=True, exist_ok=True)

    # Get benchmark names first (needed for multi-timeline)
    benchmark_names = get_benchmark_names(runs)
    print(f"Found {len(benchmark_names)} benchmarks: {', '.join(benchmark_names)}")

    # Generate aggregate timeline chart
    timeline_svg = generate_timeline_chart(runs, platform)
    timeline_path = output_dir / "timeline.svg"
    with open(timeline_path, "w") as f:
        f.write(timeline_svg)
    print(f"Generated {timeline_path}")

    # Generate per-program timeline chart (multi-line)
    if benchmark_names:
        multi_timeline_svg = generate_multi_timeline_chart(runs, benchmark_names)
        multi_timeline_path = output_dir / "timeline_by_program.svg"
        with open(multi_timeline_path, "w") as f:
            f.write(multi_timeline_svg)
        print(f"Generated {multi_timeline_path}")

    # Generate aggregate breakdown chart (for backwards compatibility)
    breakdown_svg = generate_breakdown_chart(runs, platform=platform)
    breakdown_path = output_dir / "breakdown.svg"
    with open(breakdown_path, "w") as f:
        f.write(breakdown_svg)
    print(f"Generated {breakdown_path}")

    # Generate memory usage chart
    memory_svg = generate_memory_chart(runs, platform)
    memory_path = output_dir / "memory.svg"
    with open(memory_path, "w") as f:
        f.write(memory_svg)
    print(f"Generated {memory_path}")

    # Generate binary size chart
    binary_svg = generate_binary_size_chart(runs, platform)
    binary_path = output_dir / "binary_size.svg"
    with open(binary_path, "w") as f:
        f.write(binary_svg)
    print(f"Generated {binary_path}")

    # Generate per-benchmark breakdown charts
    for bench_name in benchmark_names:
        bench_svg = generate_breakdown_chart(runs, bench_name, platform)
        # Use sanitized filename
        safe_name = bench_name.replace(" ", "_").replace("/", "_")
        bench_path = output_dir / f"breakdown_{safe_name}.svg"
        with open(bench_path, "w") as f:
            f.write(bench_svg)
        print(f"Generated {bench_path}")

    # Generate summary statistics
    summary = generate_summary_data(runs, platform)

    # Include latest run's metrics for display
    latest_benchmarks = []
    if runs:
        latest_run = runs[-1]
        for bench in latest_run.get("benchmarks", []):
            bench_info = {
                "name": bench.get("name", ""),
                "mean_ms": benchmark_time(bench),
            }
            if "source_metrics" in bench:
                sm = bench["source_metrics"]
                bench_info["source_metrics"] = sm
                # Calculate throughput metrics
                if bench_info["mean_ms"] > 0:
                    seconds = bench_info["mean_ms"] / 1000
                    bench_info["lines_per_sec"] = int(sm.get("lines", 0) / seconds)
                    bench_info["tokens_per_sec"] = int(sm.get("tokens", 0) / seconds)
            if "peak_memory_bytes" in bench:
                bench_info["peak_memory_mb"] = round(bench["peak_memory_bytes"] / (1024 * 1024), 2)
            if "binary_size_bytes" in bench:
                bench_info["binary_size_kb"] = round(bench["binary_size_bytes"] / 1024, 2)
            latest_benchmarks.append(bench_info)

    # Calculate coverage metrics
    coverage = calculate_coverage_metrics(runs)

    # Write metadata JSON for the website to consume (includes summary and detailed metrics)
    metadata = {
        "benchmarks": benchmark_names,
        "run_count": len(runs),
        "latest_commit": short_commit(runs[-1].get("commit", "")) if runs else None,
        "summary": summary,
        "latest_benchmarks": latest_benchmarks,
        "coverage": coverage,
        "regimes": regime_summary(runs),
        "metric_semantics": derive_history_metrics(runs),
    }
    if platform:
        metadata["platform"] = platform
        info = PLATFORM_INFO.get(platform, {})
        metadata["platform_name"] = info.get("name", platform)

    metadata_path = output_dir / "metadata.json"
    with open(metadata_path, "w") as f:
        json.dump(metadata, f, indent=2)
    print(f"Generated {metadata_path}")


def generate_comparison_charts(history_files: list[Path], output_dir: Path):
    """Generate comparison charts from multiple platform history files."""
    print(f"Generating comparison charts from {len(history_files)} history files")

    # Load all histories
    platform_data: dict[str, list[dict]] = {}
    platform_info_list = []

    for path in history_files:
        # Extract platform from a legacy filename or v3 directory name.
        name = path.stem
        if name.startswith("history-"):
            platform = name[8:]  # Remove "history-" prefix
        elif name == "history":
            platform = "unknown"
        else:
            platform = name

        history = load_history(path)
        runs = history.get("runs", [])

        if runs:
            platform_data[platform] = runs
            info = PLATFORM_INFO.get(platform, {"name": platform, "color": "#888888"})
            platform_info_list.append({
                "id": platform,
                "name": info["name"],
                "color": info["color"],
                "run_count": len(runs),
                "latest_commit": short_commit(runs[-1].get("commit", "")) if runs else None,
                "has_data": True
            })
            print(f"  Loaded {len(runs)} runs for {platform}")
        else:
            print(f"  No data for {platform}")

    if not platform_data:
        print("No data available for comparison charts")
        return

    # Ensure output directory exists
    output_dir.mkdir(parents=True, exist_ok=True)

    # Generate comparison timeline
    comparison_svg = generate_comparison_timeline_chart(platform_data)
    comparison_path = output_dir / "timeline.svg"
    with open(comparison_path, "w") as f:
        f.write(comparison_svg)
    print(f"Generated {comparison_path}")

    # Generate comparison metadata
    metadata = {
        "platforms": platform_info_list,
        "default_platform": platform_info_list[0]["id"] if platform_info_list else None
    }
    metadata_path = output_dir / "metadata.json"
    with open(metadata_path, "w") as f:
        json.dump(metadata, f, indent=2)
    print(f"Generated {metadata_path}")


def main():
    parser = argparse.ArgumentParser(
        description="Generate SVG charts from benchmark history for the performance dashboard."
    )
    parser.add_argument(
        "--comparison",
        action="store_true",
        help="Generate comparison charts from multiple platform histories"
    )
    parser.add_argument(
        "--platform",
        type=str,
        help="Platform identifier for chart titles (e.g., x86-64-linux)"
    )
    parser.add_argument(
        "paths",
        nargs="+",
        help="History file(s) and output directory. For single platform: <history.json> <output-dir>. "
             "For comparison: <output-dir> <history1.json> <history2.json> ..."
    )

    args = parser.parse_args()

    if args.comparison:
        # Comparison mode: first arg is output dir, rest are history files
        if len(args.paths) < 2:
            print("Error: Comparison mode requires output directory and at least one history file", file=sys.stderr)
            sys.exit(1)
        output_dir = Path(args.paths[0])
        history_files = [Path(p) for p in args.paths[1:]]
        generate_comparison_charts(history_files, output_dir)
    else:
        # Single platform mode
        if len(args.paths) != 2:
            print("Error: Single platform mode requires exactly <history.json> <output-dir>", file=sys.stderr)
            sys.exit(1)
        history_path = Path(args.paths[0])
        output_dir = Path(args.paths[1])
        generate_platform_charts(history_path, output_dir, args.platform)


if __name__ == "__main__":
    main()
