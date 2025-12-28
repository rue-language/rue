#!/usr/bin/env python3
"""
Generate SVG charts from benchmark history for the performance dashboard.

This script reads benchmark history from JSON and generates two SVG charts:
1. timeline.svg - Time-series chart showing total compilation time over commits
2. breakdown.svg - Stacked bar chart showing time per compiler pass

Usage:
    ./generate-charts.py <history.json> <output-dir>

Example:
    ./generate-charts.py website/static/benchmarks/history.json website/static/benchmarks/
"""

import json
import sys
from pathlib import Path
from typing import Optional

# Chart dimensions
TIMELINE_WIDTH = 800
TIMELINE_HEIGHT = 300
BREAKDOWN_WIDTH = 800
BREAKDOWN_HEIGHT = 350
MEMORY_WIDTH = 800
MEMORY_HEIGHT = 250
BINARY_WIDTH = 800
BINARY_HEIGHT = 250

# Colors for passes (consistent with website theme)
PASS_COLORS = {
    "lexer": "#4f6ddb",     # accent blue
    "parser": "#7c9dff",    # lighter blue
    "astgen": "#3b82f6",    # sky blue
    "sema": "#06b6d4",      # cyan
    "cfg": "#10b981",       # emerald
    "codegen": "#f59e0b",   # amber
    "linker": "#ef4444",    # red
}

# Order of passes in the stack
PASS_ORDER = ["lexer", "parser", "astgen", "sema", "cfg", "codegen", "linker"]


def load_history(path: Path) -> dict:
    """Load benchmark history from JSON file."""
    if not path.exists():
        return {"version": 1, "runs": []}

    with open(path, "r") as f:
        data = json.load(f)

    # Handle legacy format
    if isinstance(data, list):
        return {"version": 1, "runs": data}

    return data


def get_pass_times(run: dict) -> dict[str, float]:
    """Extract pass timing from a benchmark run."""
    # Look for pass timing data in the benchmarks
    for bench in run.get("benchmarks", []):
        if "passes" in bench:
            # New format with per-pass timing
            passes = bench["passes"]
            return {
                name: passes.get(name, {}).get("mean_ms", 0)
                for name in PASS_ORDER
            }
    return {}


def get_total_time(run: dict) -> float:
    """Get total compilation time from a run."""
    for bench in run.get("benchmarks", []):
        if "mean_ms" in bench:
            return bench["mean_ms"]
        if "total_ms" in bench:
            total = bench["total_ms"]
            if isinstance(total, dict):
                return total.get("mean", 0)
            return total
    return 0


def get_peak_memory(run: dict) -> float:
    """Get peak memory usage (in MB) from a run."""
    for bench in run.get("benchmarks", []):
        if "peak_memory_bytes" in bench:
            return bench["peak_memory_bytes"] / (1024 * 1024)  # Convert to MB
    return 0


def get_binary_size(run: dict) -> float:
    """Get binary size (in KB) from a run."""
    for bench in run.get("benchmarks", []):
        if "binary_size_bytes" in bench:
            return bench["binary_size_bytes"] / 1024  # Convert to KB
    return 0


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
    .chart-bg {{ fill: var(--chart-bg, #ffffff); }}
    .chart-text {{ fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }}
    @media (prefers-color-scheme: dark) {{
      .chart-bg {{ fill: #1a1a1a; }}
      .chart-text {{ fill: #9ca3af; }}
    }}
  </style>
  <rect class="chart-bg" width="{width}" height="{height}" rx="8"/>
  <text class="chart-text" x="{width/2}" y="{height/2}" text-anchor="middle" font-size="14">{escape_xml(message)}</text>
</svg>'''


def generate_timeline_chart(runs: list[dict]) -> str:
    """Generate time-series SVG chart of total compilation time."""
    if not runs:
        return generate_empty_chart(TIMELINE_WIDTH, TIMELINE_HEIGHT, "No benchmark data available yet")

    # Extract data points
    points = []
    for run in runs[-20:]:  # Show last 20 commits
        total = get_total_time(run)
        commit = short_commit(run.get("commit", ""))
        points.append({"commit": commit, "time": total})

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

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {TIMELINE_WIDTH} {TIMELINE_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #ffffff); }
    .chart-text { fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #1a1a1a); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-line { stroke: var(--chart-accent, #4f6ddb); fill: none; stroke-width: 2; }
    .chart-point { fill: var(--chart-accent, #4f6ddb); }
    .chart-grid { stroke: var(--chart-grid, #e5e7eb); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #9ca3af); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{TIMELINE_WIDTH}" height="{TIMELINE_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{TIMELINE_WIDTH/2}" y="25" text-anchor="middle" font-size="16">Compilation Time Over Recent Commits</text>',
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
    if len(points) > 1:
        path_d = "M " + " L ".join(
            f"{scale_x(i)},{scale_y(p['time'])}"
            for i, p in enumerate(points)
        )
        svg_parts.append(f'  <path class="chart-line" d="{path_d}"/>')

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
            if "mean_ms" in bench:
                return bench["mean_ms"]
            if "total_ms" in bench:
                total = bench["total_ms"]
                if isinstance(total, dict):
                    return total.get("mean", 0)
                return total
    return 0


def generate_multi_timeline_chart(runs: list[dict], benchmark_names: list[str]) -> str:
    """Generate time-series SVG chart showing each benchmark program as a separate line."""
    if not runs or not benchmark_names:
        return generate_empty_chart(TIMELINE_WIDTH, TIMELINE_HEIGHT + 50, "No benchmark data available yet")

    # Extract data points for each benchmark
    commits = [short_commit(run.get("commit", "")) for run in runs[-20:]]
    benchmark_data = {}

    for name in benchmark_names:
        points = []
        for run in runs[-20:]:
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
    .chart-bg { fill: var(--chart-bg, #ffffff); }
    .chart-text { fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #1a1a1a); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-grid { stroke: var(--chart-grid, #e5e7eb); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #9ca3af); stroke-width: 1; }
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
            line_points = []
            for i, time in enumerate(points):
                if time > 0:
                    line_points.append(f"{scale_x(i)},{scale_y(time)}")
            if line_points:
                path_d = "M " + " L ".join(line_points)
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
            passes = bench["passes"]
            return {
                name: passes.get(name, {}).get("mean_ms", 0)
                for name in PASS_ORDER
            }
    return {}


def generate_breakdown_chart(runs: list[dict], benchmark_name: Optional[str] = None) -> str:
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

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {BREAKDOWN_WIDTH} {BREAKDOWN_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #ffffff); }
    .chart-text { fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #1a1a1a); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-grid { stroke: var(--chart-grid, #e5e7eb); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #9ca3af); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{BREAKDOWN_WIDTH}" height="{BREAKDOWN_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{BREAKDOWN_WIDTH/2}" y="25" text-anchor="middle" font-size="16">Compilation Time by Pass{" - " + escape_xml(benchmark_name) if benchmark_name else ""}</text>',
        f'  <text class="chart-text" x="{BREAKDOWN_WIDTH/2}" y="42" text-anchor="middle" font-size="11">(commit: {escape_xml(commit)})</text>',
    ]

    # Horizontal stacked bar
    bar_height = 40
    bar_y = margin["top"] + (chart_height - bar_height) / 2
    x_offset = margin["left"]

    for pass_name in PASS_ORDER:
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

    for i, pass_name in enumerate(PASS_ORDER):
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


def generate_memory_chart(runs: list[dict]) -> str:
    """Generate time-series SVG chart of peak memory usage."""
    if not runs:
        return generate_empty_chart(MEMORY_WIDTH, MEMORY_HEIGHT, "No benchmark data available yet")

    # Extract data points
    points = []
    for run in runs[-20:]:  # Show last 20 commits
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

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {MEMORY_WIDTH} {MEMORY_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #ffffff); }
    .chart-text { fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #1a1a1a); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-line { stroke: var(--chart-memory, #10b981); fill: none; stroke-width: 2; }
    .chart-point { fill: var(--chart-memory, #10b981); }
    .chart-grid { stroke: var(--chart-grid, #e5e7eb); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #9ca3af); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{MEMORY_WIDTH}" height="{MEMORY_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{MEMORY_WIDTH/2}" y="25" text-anchor="middle" font-size="16">Peak Memory Usage Over Recent Commits</text>',
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
    valid_points = [(i, p) for i, p in enumerate(points) if p["memory"] > 0]
    if len(valid_points) > 1:
        path_d = "M " + " L ".join(
            f"{scale_x(i)},{scale_y(p['memory'])}"
            for i, p in valid_points
        )
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


def generate_binary_size_chart(runs: list[dict]) -> str:
    """Generate time-series SVG chart of binary size."""
    if not runs:
        return generate_empty_chart(BINARY_WIDTH, BINARY_HEIGHT, "No benchmark data available yet")

    # Extract data points
    points = []
    for run in runs[-20:]:  # Show last 20 commits
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

    # Build SVG
    svg_parts = [
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {BINARY_WIDTH} {BINARY_HEIGHT}" class="benchmark-chart">',
        '''  <style>
    .chart-bg { fill: var(--chart-bg, #ffffff); }
    .chart-text { fill: var(--chart-text, #6b7280); font-family: system-ui, sans-serif; }
    .chart-title { fill: var(--chart-title, #1a1a1a); font-family: system-ui, sans-serif; font-weight: 600; }
    .chart-line { stroke: var(--chart-binary, #f59e0b); fill: none; stroke-width: 2; }
    .chart-point { fill: var(--chart-binary, #f59e0b); }
    .chart-grid { stroke: var(--chart-grid, #e5e7eb); stroke-width: 1; }
    .chart-axis { stroke: var(--chart-axis, #9ca3af); stroke-width: 1; }
    @media (prefers-color-scheme: dark) {
      .chart-bg { fill: #1a1a1a; }
      .chart-text { fill: #9ca3af; }
      .chart-title { fill: #f0f0f0; }
      .chart-grid { stroke: #2e2e2e; }
      .chart-axis { stroke: #4b5563; }
    }
  </style>''',
        f'  <rect class="chart-bg" width="{BINARY_WIDTH}" height="{BINARY_HEIGHT}" rx="8"/>',
        f'  <text class="chart-title" x="{BINARY_WIDTH/2}" y="25" text-anchor="middle" font-size="16">Binary Size Over Recent Commits</text>',
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
    valid_points = [(i, p) for i, p in enumerate(points) if p["size"] > 0]
    if len(valid_points) > 1:
        path_d = "M " + " L ".join(
            f"{scale_x(i)},{scale_y(p['size'])}"
            for i, p in valid_points
        )
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


def generate_summary_data(runs: list[dict]) -> dict:
    """Generate summary statistics for the performance dashboard."""
    if not runs:
        return {}

    latest = runs[-1] if runs else None
    previous = runs[-2] if len(runs) >= 2 else None

    # Get latest values
    latest_time = get_total_time(latest) if latest else 0
    latest_memory = get_peak_memory(latest) if latest else 0
    latest_binary = get_binary_size(latest) if latest else 0
    latest_commit = short_commit(latest.get("commit", "")) if latest else ""

    # Calculate deltas
    prev_time = get_total_time(previous) if previous else 0
    prev_memory = get_peak_memory(previous) if previous else 0
    prev_binary = get_binary_size(previous) if previous else 0

    time_delta_pct, time_delta_str = calculate_delta(latest_time, prev_time)
    memory_delta_pct, memory_delta_str = calculate_delta(latest_memory, prev_memory)
    binary_delta_pct, binary_delta_str = calculate_delta(latest_binary, prev_binary)

    # Calculate 7-run average (or whatever we have)
    recent_runs = runs[-7:] if len(runs) >= 7 else runs
    avg_time = sum(get_total_time(r) for r in recent_runs) / len(recent_runs) if recent_runs else 0
    avg_memory = sum(get_peak_memory(r) for r in recent_runs) / len(recent_runs) if recent_runs else 0

    # Find best ever
    all_times = [get_total_time(r) for r in runs if get_total_time(r) > 0]
    best_time = min(all_times) if all_times else 0

    return {
        "latest_commit": latest_commit,
        "latest_time_ms": round(latest_time, 2),
        "latest_memory_mb": round(latest_memory, 2),
        "latest_binary_kb": round(latest_binary, 2),
        "time_delta_pct": round(time_delta_pct, 2),
        "time_delta_str": time_delta_str,
        "memory_delta_pct": round(memory_delta_pct, 2),
        "memory_delta_str": memory_delta_str,
        "binary_delta_pct": round(binary_delta_pct, 2),
        "binary_delta_str": binary_delta_str,
        "avg_time_ms": round(avg_time, 2),
        "avg_memory_mb": round(avg_memory, 2),
        "best_time_ms": round(best_time, 2),
        "run_count": len(runs),
    }


def main():
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <history.json> <output-dir>", file=sys.stderr)
        sys.exit(1)

    history_path = Path(sys.argv[1])
    output_dir = Path(sys.argv[2])

    # Load history
    history = load_history(history_path)
    runs = history.get("runs", [])

    print(f"Loaded {len(runs)} benchmark runs from {history_path}")

    # Ensure output directory exists
    output_dir.mkdir(parents=True, exist_ok=True)

    # Get benchmark names first (needed for multi-timeline)
    benchmark_names = get_benchmark_names(runs)
    print(f"Found {len(benchmark_names)} benchmarks: {', '.join(benchmark_names)}")

    # Generate aggregate timeline chart
    timeline_svg = generate_timeline_chart(runs)
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
    breakdown_svg = generate_breakdown_chart(runs)
    breakdown_path = output_dir / "breakdown.svg"
    with open(breakdown_path, "w") as f:
        f.write(breakdown_svg)
    print(f"Generated {breakdown_path}")

    # Generate memory usage chart
    memory_svg = generate_memory_chart(runs)
    memory_path = output_dir / "memory.svg"
    with open(memory_path, "w") as f:
        f.write(memory_svg)
    print(f"Generated {memory_path}")

    # Generate binary size chart
    binary_svg = generate_binary_size_chart(runs)
    binary_path = output_dir / "binary_size.svg"
    with open(binary_path, "w") as f:
        f.write(binary_svg)
    print(f"Generated {binary_path}")

    # Generate per-benchmark breakdown charts
    for bench_name in benchmark_names:
        bench_svg = generate_breakdown_chart(runs, bench_name)
        # Use sanitized filename
        safe_name = bench_name.replace(" ", "_").replace("/", "_")
        bench_path = output_dir / f"breakdown_{safe_name}.svg"
        with open(bench_path, "w") as f:
            f.write(bench_svg)
        print(f"Generated {bench_path}")

    # Generate summary statistics
    summary = generate_summary_data(runs)

    # Include latest run's metrics for display
    latest_benchmarks = []
    if runs:
        latest_run = runs[-1]
        for bench in latest_run.get("benchmarks", []):
            bench_info = {
                "name": bench.get("name", ""),
                "mean_ms": bench.get("mean_ms", 0),
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

    # Write metadata JSON for the website to consume (includes summary and detailed metrics)
    metadata = {
        "benchmarks": benchmark_names,
        "run_count": len(runs),
        "latest_commit": short_commit(runs[-1].get("commit", "")) if runs else None,
        "summary": summary,
        "latest_benchmarks": latest_benchmarks,
    }
    metadata_path = output_dir / "metadata.json"
    with open(metadata_path, "w") as f:
        json.dump(metadata, f, indent=2)
    print(f"Generated {metadata_path}")


if __name__ == "__main__":
    main()
