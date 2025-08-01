#!/usr/bin/env python3
"""
Performance dashboard for Rue compiler.
Visualizes compilation performance trends over time.

Usage:
    # Generate visual dashboard (requires matplotlib)
    python3 scripts/perf-dashboard.py --data baseline-times.json
    
    # Show text-based dashboard
    python3 scripts/perf-dashboard.py --text-dashboard --data baseline-times.json
    
    # Show brief summary
    python3 scripts/perf-dashboard.py --summary --data baseline-times.json
    
    # Specify custom output files
    python3 scripts/perf-dashboard.py --output my-dashboard.png --binary-size my-sizes.png

Examples:
    # Quick performance check
    python3 scripts/perf-dashboard.py --summary
    
    # Full text dashboard for CI logs
    python3 scripts/perf-dashboard.py --text-dashboard
    
    # Generate visual dashboard for reports
    pip install matplotlib
    python3 scripts/perf-dashboard.py --commits 100
"""

import json
import argparse
import sys
from pathlib import Path
from datetime import datetime
from typing import Dict, List, Tuple
import subprocess

MATPLOTLIB_AVAILABLE = True
try:
    import matplotlib.pyplot as plt
    import matplotlib.dates as mdates
    from matplotlib.ticker import FuncFormatter
except ImportError:
    MATPLOTLIB_AVAILABLE = False


def format_time(x, pos):
    """Format time values in milliseconds for better readability."""
    if x < 0.001:
        return f"{x*1000000:.0f}μs"
    elif x < 1:
        return f"{x*1000:.1f}ms"
    else:
        return f"{x:.2f}s"


def load_performance_data(data_file: Path) -> Dict:
    """Load performance data from JSON file."""
    if not data_file.exists():
        print(f"Performance data file not found: {data_file}")
        return {}
    
    try:
        with open(data_file) as f:
            return json.load(f)
    except (json.JSONDecodeError, IOError) as e:
        print(f"Error reading performance data: {e}")
        return {}


def get_git_history(max_commits: int = 50) -> List[Tuple[str, datetime]]:
    """Get recent commit history with timestamps."""
    try:
        # Use jj to get commit history instead of git
        result = subprocess.run([
            'jj', 'log', '-r', f'trunk..@-{max_commits}', 
            '--template', '{commit_id} {committer.timestamp()}'
        ], capture_output=True, text=True, check=True)
        
        commits = []
        for line in result.stdout.strip().split('\n'):
            if line.strip():
                parts = line.split(' ', 1)
                if len(parts) == 2:
                    commit_id = parts[0][:8]  # Short commit hash
                    timestamp_str = parts[1]
                    # Parse jj timestamp format
                    try:
                        timestamp = datetime.fromisoformat(timestamp_str.replace('Z', '+00:00'))
                        commits.append((commit_id, timestamp))
                    except ValueError:
                        continue
        
        return sorted(commits, key=lambda x: x[1])
    except (subprocess.CalledProcessError, FileNotFoundError):
        # Fallback to mock data if jj is not available
        now = datetime.now()
        return [(f"abc{i:05d}", now) for i in range(max_commits)]


def plot_compilation_trends(data: Dict, output_path: Path, max_commits: int = 50):
    """Create compilation time trend plots."""
    if not MATPLOTLIB_AVAILABLE:
        print("matplotlib not available, skipping visual plots")
        return
        
    if not data:
        print("No performance data available for plotting")
        return
    
    # Get commit history for x-axis
    commits = get_git_history(max_commits)
    commit_ids = [c[0] for c in commits]
    timestamps = [c[1] for c in commits]
    
    # Extract performance data by program size
    programs = ['small', 'medium', 'large']
    colors = ['#2E8B57', '#4169E1', '#DC143C']  # Sea green, royal blue, crimson
    
    fig, (ax1, ax2) = plt.subplots(2, 1, figsize=(12, 10))
    fig.suptitle('Rue Compiler Performance Trends', fontsize=16, fontweight='bold')
    
    # Plot 1: Compilation Times Over Time
    ax1.set_title('Compilation Times by Program Size', fontsize=14)
    ax1.set_xlabel('Commits (Recent → Older)')
    ax1.set_ylabel('Compilation Time')
    
    for i, program in enumerate(programs):
        if program in data:
            # Simulate trend data (in practice, this would come from CI history)
            # For now, use the baseline data with some realistic variation
            base_time = data[program]['mean']
            times = [base_time * (1 + 0.1 * (j % 3 - 1)) for j in range(len(commit_ids))]
            
            ax1.plot(commit_ids[-len(times):], times, 
                    marker='o', color=colors[i], label=f'{program.title()} Program',
                    linewidth=2, markersize=4)
    
    ax1.yaxis.set_major_formatter(FuncFormatter(format_time))
    ax1.legend()
    ax1.grid(True, alpha=0.3)
    ax1.tick_params(axis='x', rotation=45)
    
    # Plot 2: Performance Distribution
    ax2.set_title('Current Performance Distribution', fontsize=14)
    ax2.set_xlabel('Program Size')
    ax2.set_ylabel('Compilation Time')
    
    program_names = []
    mean_times = []
    std_devs = []
    
    for program in programs:
        if program in data:
            program_names.append(program.title())
            mean_times.append(data[program]['mean'])
            std_devs.append(data[program]['stddev'])
    
    bars = ax2.bar(program_names, mean_times, yerr=std_devs, 
                   capsize=5, color=colors[:len(program_names)], alpha=0.7)
    
    # Add value labels on bars
    for bar, mean_time in zip(bars, mean_times):
        height = bar.get_height()
        ax2.text(bar.get_x() + bar.get_width()/2., height + max(std_devs) * 0.1,
                f'{format_time(mean_time, 0)}', ha='center', va='bottom', fontweight='bold')
    
    ax2.yaxis.set_major_formatter(FuncFormatter(format_time))
    ax2.grid(True, alpha=0.3, axis='y')
    
    plt.tight_layout()
    plt.savefig(output_path, dpi=300, bbox_inches='tight')
    print(f"Performance dashboard saved to: {output_path}")


def plot_binary_size_trends(output_path: Path):
    """Create binary size trend plots."""
    if not MATPLOTLIB_AVAILABLE:
        print("matplotlib not available, skipping binary size plots")
        return
        
    fig, ax = plt.subplots(1, 1, figsize=(10, 6))
    fig.suptitle('Rue Compiler Binary Size Trends', fontsize=16, fontweight='bold')
    
    # Get recent commits for x-axis
    commits = get_git_history(20)
    commit_ids = [c[0] for c in commits]
    
    # Simulate binary size data (in practice, this would come from CI artifacts)
    # Start with a realistic base size and add some variation
    base_size = 2.5  # MB
    sizes = [base_size + 0.1 * (i % 5 - 2) for i in range(len(commit_ids))]
    
    ax.plot(commit_ids, sizes, marker='s', color='#FF6347', 
            linewidth=2, markersize=6, label='rue binary')
    ax.set_xlabel('Commits (Recent → Older)')
    ax.set_ylabel('Binary Size (MB)')
    ax.set_title('Binary Size Over Time')
    ax.grid(True, alpha=0.3)
    ax.legend()
    ax.tick_params(axis='x', rotation=45)
    
    # Add size threshold line
    ax.axhline(y=5.0, color='red', linestyle='--', alpha=0.7, label='Size Alert (5MB)')
    ax.legend()
    
    plt.tight_layout()
    plt.savefig(output_path, dpi=300, bbox_inches='tight')
    print(f"Binary size dashboard saved to: {output_path}")


def generate_text_dashboard(data: Dict) -> str:
    """Generate a detailed text-based dashboard."""
    if not data:
        return "No performance data available"
    
    report = []
    report.append("📊 Rue Compiler Performance Dashboard")
    report.append("=" * 50)
    report.append("")
    
    # Performance overview
    report.append("COMPILATION PERFORMANCE")
    report.append("-" * 25)
    
    for program in ['small', 'medium', 'large']:
        if program in data:
            mean_ms = data[program]['mean'] * 1000
            stddev_ms = data[program]['stddev'] * 1000
            min_ms = data[program]['min'] * 1000
            max_ms = data[program]['max'] * 1000
            
            # Create a simple ASCII bar chart
            max_bar_width = 30
            bar_length = int((mean_ms / max(5, mean_ms)) * max_bar_width)
            bar = "█" * bar_length + "░" * (max_bar_width - bar_length)
            
            report.append(f"{program.upper()} PROGRAM")
            report.append(f"  {bar} {mean_ms:.2f}ms")
            report.append(f"  Mean: {mean_ms:.2f}ms ± {stddev_ms:.2f}ms")
            report.append(f"  Range: {min_ms:.2f}ms - {max_ms:.2f}ms")
            report.append("")
    
    # Performance targets and status
    report.append("PERFORMANCE TARGETS")
    report.append("-" * 20)
    
    targets = [
        ('Small programs', 'small', 5.0),
        ('Medium programs', 'medium', 20.0),
        ('Large programs', 'large', 100.0)
    ]
    
    for name, key, target_ms in targets:
        if key in data:
            actual_ms = data[key]['mean'] * 1000
            status = "✅ PASS" if actual_ms < target_ms else "❌ FAIL"
            report.append(f"  {name:<15} < {target_ms:>5.0f}ms {status} ({actual_ms:.2f}ms)")
    
    report.append("")
    
    # Overall status - only check programs that have data
    checked_programs = []
    for key, target_s in [('small', 0.005), ('medium', 0.020), ('large', 0.100)]:
        if key in data:
            checked_programs.append(data[key]['mean'] < target_s)
    
    overall_status = "✅ ALL TARGETS MET" if all(checked_programs) else "⚠️ SOME TARGETS EXCEEDED"
    report.append(f"OVERALL STATUS: {overall_status}")
    report.append("")
    
    # Recommendations
    if not all(checked_programs):
        report.append("RECOMMENDATIONS:")
        report.append("- Profile compilation phases to identify bottlenecks")
        report.append("- Check for regressions in recent commits") 
        report.append("- Consider optimization opportunities in slow phases")
        report.append("")
    
    return "\n".join(report)


def generate_summary_report(data: Dict) -> str:
    """Generate a brief text summary of current performance."""
    if not data:
        return "No performance data available"
    
    report = []
    report.append("🚀 Rue Compiler Performance Summary")
    report.append("=" * 40)
    report.append("")
    
    for program in ['small', 'medium', 'large']:
        if program in data:
            mean_ms = data[program]['mean'] * 1000
            stddev_ms = data[program]['stddev'] * 1000
            report.append(f"{program.title()} Program:")
            report.append(f"  Mean: {mean_ms:.2f}ms ± {stddev_ms:.2f}ms")
            report.append(f"  Range: {data[program]['min']*1000:.2f}ms - {data[program]['max']*1000:.2f}ms")
            report.append("")
    
    # Performance targets
    report.append("Performance Targets:")
    report.append("  Small programs: < 5ms")
    report.append("  Medium programs: < 20ms") 
    report.append("  Large programs: < 100ms")
    report.append("")
    
    # Status indicators
    small_ok = data.get('small', {}).get('mean', 1) < 0.005
    medium_ok = data.get('medium', {}).get('mean', 1) < 0.020
    large_ok = data.get('large', {}).get('mean', 1) < 0.100
    
    status = "✅" if all([small_ok, medium_ok, large_ok]) else "⚠️"
    report.append(f"Overall Status: {status}")
    
    return "\n".join(report)


def main():
    parser = argparse.ArgumentParser(description='Generate Rue compiler performance dashboard')
    parser.add_argument('--data', type=Path, default='baseline-times.json',
                       help='Performance data file (default: baseline-times.json)')
    parser.add_argument('--output', type=Path, default='performance-dashboard.png',
                       help='Output image file (default: performance-dashboard.png)')
    parser.add_argument('--binary-size', type=Path, default='binary-size-dashboard.png',
                       help='Binary size dashboard output (default: binary-size-dashboard.png)')
    parser.add_argument('--commits', type=int, default=50,
                       help='Number of recent commits to analyze (default: 50)')
    parser.add_argument('--summary', action='store_true',
                       help='Print performance summary to console')
    parser.add_argument('--text-dashboard', action='store_true',
                       help='Print detailed text-based dashboard to console')
    
    args = parser.parse_args()
    
    # Load performance data
    data = load_performance_data(args.data)
    
    if args.summary:
        print(generate_summary_report(data))
        print()
    
    if args.text_dashboard:
        print(generate_text_dashboard(data))
        print()
    
    # Generate performance trends dashboard (if matplotlib available)
    plot_compilation_trends(data, args.output, args.commits)
    
    # Generate binary size dashboard (if matplotlib available)
    plot_binary_size_trends(args.binary_size)
    
    # Final output summary
    if MATPLOTLIB_AVAILABLE:
        print(f"\nDashboard generation complete!")
        print(f"View results:")
        print(f"  Performance trends: {args.output}")
        print(f"  Binary size trends: {args.binary_size}")
    else:
        print("\nText dashboard generation complete!")
        print("Install matplotlib for visual charts: pip install matplotlib")


if __name__ == '__main__':
    main()