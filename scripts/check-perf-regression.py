#!/usr/bin/env python3
"""
Performance regression detection for Rue compiler.
Compares current benchmark results against baseline and fails if regression detected.
"""

import json
import sys
import os
from pathlib import Path

def load_hyperfine_results(results_file):
    """Load hyperfine benchmark results from JSON file."""
    with open(results_file) as f:
        data = json.load(f)
    
    times = {}
    for result in data['results']:
        # Extract program name from command (last part of path)
        command_parts = result['command'].split()
        program_path = command_parts[-1]  # e.g., "bench-programs/small.rue"
        program_name = Path(program_path).stem  # e.g., "small"
        
        times[program_name] = {
            'mean': result['mean'],
            'stddev': result['stddev'],
            'median': result['median'],
            'min': result['min'],
            'max': result['max']
        }
    
    return times

def load_baseline(baseline_file):
    """Load baseline performance data."""
    if not os.path.exists(baseline_file):
        return None
    
    try:
        with open(baseline_file) as f:
            return json.load(f)
    except (json.JSONDecodeError, FileNotFoundError):
        return None

def save_baseline(times, baseline_file):
    """Save current times as new baseline."""
    with open(baseline_file, 'w') as f:
        json.dump(times, f, indent=2)

def check_regression(current_times, baseline_times, threshold=1.1):
    """
    Check for performance regressions.
    
    Args:
        current_times: Dict of current benchmark results
        baseline_times: Dict of baseline benchmark results  
        threshold: Regression threshold (1.1 = 10% slower)
    
    Returns:
        List of regressions found
    """
    regressions = []
    
    for program, current in current_times.items():
        if program not in baseline_times:
            print(f"INFO: No baseline for {program}, skipping regression check")
            continue
        
        baseline = baseline_times[program]
        current_mean = current['mean']
        baseline_mean = baseline['mean']
        
        if current_mean <= 0 or baseline_mean <= 0:
            print(f"WARNING: Invalid timing data for {program}")
            continue
        
        ratio = current_mean / baseline_mean
        percent_change = (ratio - 1) * 100
        
        print(f"{program}: {current_mean:.4f}s vs {baseline_mean:.4f}s ({percent_change:+.1f}%)")
        
        if ratio > threshold:
            regressions.append({
                'program': program,
                'current_time': current_mean,
                'baseline_time': baseline_mean,  
                'ratio': ratio,
                'percent_slower': percent_change
            })
    
    return regressions

def print_summary(current_times, baseline_times, regressions):
    """Print performance summary."""
    print("\n" + "="*60)
    print("PERFORMANCE SUMMARY")
    print("="*60)
    
    if baseline_times:
        print(f"Compared against baseline with {len(baseline_times)} programs")
        
        if regressions:
            print(f"\n❌ REGRESSIONS DETECTED ({len(regressions)}):")
            for reg in regressions:
                print(f"  • {reg['program']}: {reg['percent_slower']:+.1f}% slower")
        else:
            print(f"\n✅ NO REGRESSIONS DETECTED")
            
        # Show improvements
        improvements = []
        for program, current in current_times.items():
            if program in baseline_times:
                baseline = baseline_times[program]
                ratio = current['mean'] / baseline['mean']
                if ratio < 0.95:  # 5% faster
                    improvements.append({
                        'program': program,
                        'percent_faster': (1 - ratio) * 100
                    })
        
        if improvements:
            print(f"\n🚀 IMPROVEMENTS DETECTED ({len(improvements)}):")
            for imp in improvements:
                print(f"  • {imp['program']}: {imp['percent_faster']:.1f}% faster")
    else:
        print("No baseline found - establishing new baseline")
    
    print(f"\nCurrent performance:")
    for program, times in current_times.items():
        print(f"  • {program}: {times['mean']:.4f}s ± {times['stddev']:.4f}s")

def main():
    if len(sys.argv) < 2:
        print("Usage: check-perf-regression.py <hyperfine-results.json> [baseline.json]")
        sys.exit(1)
    
    results_file = sys.argv[1]
    baseline_file = sys.argv[2] if len(sys.argv) > 2 else 'baseline-times.json'
    
    # Load current results
    try:
        current_times = load_hyperfine_results(results_file)
    except Exception as e:
        print(f"ERROR: Failed to load results from {results_file}: {e}")
        sys.exit(1)
    
    if not current_times:
        print("ERROR: No benchmark results found")
        sys.exit(1)
    
    # Load baseline
    baseline_times = load_baseline(baseline_file)
    
    # Check for regressions
    regressions = []
    if baseline_times:
        regressions = check_regression(current_times, baseline_times, threshold=1.1)
    
    # Print summary
    print_summary(current_times, baseline_times, regressions)
    
    # Save new baseline
    save_baseline(current_times, baseline_file)
    print(f"\nUpdated baseline saved to {baseline_file}")
    
    # Exit with error if regressions found
    if regressions:
        print(f"\n❌ PERFORMANCE REGRESSION DETECTED - CI should fail")
        sys.exit(1)
    else:
        print(f"\n✅ Performance check passed")
        sys.exit(0)

if __name__ == '__main__':
    main()