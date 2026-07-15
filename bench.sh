#!/bin/bash
# Rue Compiler Benchmark Runner
#
# This script builds the compiler in release mode and runs benchmarks on all
# programs defined in benchmarks/manifest.toml. Results are saved as JSON
# for historical tracking.
#
# Usage:
#   ./bench.sh                    # Run benchmarks, append to history
#   ./bench.sh --output file.json # Save results to specific file
#   ./bench.sh --iterations 10    # Run 10 iterations (default: 5)
#   ./bench.sh --no-history       # Don't append to history file
#   ./bench.sh --help             # Show usage

# Strict mode: -e exit on error, -u error on unset vars, pipefail so a failing
# stage of a pipeline fails the whole pipeline. Pipelines that are *expected*
# to fail (a grep with no match, jj/git in an environment without them) are
# guarded explicitly with `|| true` below.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCHMARKS_DIR="$SCRIPT_DIR/benchmarks"
MANIFEST="$BENCHMARKS_DIR/manifest.toml"
HISTORY_FILE="$SCRIPT_DIR/website/static/benchmarks/history.json"
ITERATIONS=5
OUTPUT_FILE=""
APPEND_HISTORY=true
BUILD_MODE="release"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

usage() {
    echo "Usage: $0 [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  --output FILE      Save results to FILE instead of default"
    echo "  --iterations N     Run N iterations per benchmark (default: 5)"
    echo "  --no-history       Don't append results to history file"
    echo "  --debug            Build compiler in debug mode (default: release)"
    echo "  --help             Show this help message"
    echo ""
    echo "Examples:"
    echo "  $0                         # Run benchmarks with defaults (release)"
    echo "  $0 --debug                 # Run with debug build"
    echo "  $0 --iterations 10         # More iterations for accuracy"
    echo "  $0 --output results.json   # Save to custom file"
}

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --output)
            OUTPUT_FILE="$2"
            shift 2
            ;;
        --iterations)
            ITERATIONS="$2"
            shift 2
            ;;
        --no-history)
            APPEND_HISTORY=false
            shift
            ;;
        --debug)
            BUILD_MODE="debug"
            shift
            ;;
        --help)
            usage
            exit 0
            ;;
        *)
            log_error "Unknown option: $1"
            usage
            exit 1
            ;;
    esac
done

if [[ ! "$ITERATIONS" =~ ^[1-9][0-9]*$ ]]; then
    log_error "--iterations must be a positive integer"
    exit 1
fi

# Verify we're in the right directory
if [[ ! -f "$MANIFEST" ]]; then
    log_error "Cannot find benchmarks/manifest.toml"
    log_error "Run this script from the rue repository root"
    exit 1
fi

# Detect OS early (needed for platform-specific commands in the loop)
os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)

# Build the compiler
log_info "Building rue compiler ($BUILD_MODE mode)..."

# Get the path to the built compiler (scripts/rue-bin builds it and resolves a
# stable absolute path; extra args pass through to buck2 build).
RUE_BIN="$(./scripts/rue-bin --target-platforms //platforms:$BUILD_MODE)"
if [[ ! -x "$RUE_BIN" ]]; then
    log_error "Failed to find rue binary at: $RUE_BIN"
    exit 1
fi

log_info "Using compiler: $RUE_BIN"

# Create temp directory for outputs
TEMP_DIR=$(mktemp -d)
# Single-quote the trap so $TEMP_DIR is expanded (and quoted) at trap time,
# not now — avoids word-splitting on the path and keeps shellcheck (SC2064) happy.
trap 'rm -rf "$TEMP_DIR"' EXIT

# Parse manifest and run benchmarks
log_info "Running benchmarks ($ITERATIONS iterations each)..."

RESULTS_FILE="$TEMP_DIR/results.json"

# Parse benchmarks from manifest
# Format: [[benchmark]] followed by name = "...", path = "..."
benchmark_names=()
benchmark_paths=()

current_name=""
current_path=""

while IFS= read -r line; do
    # Trim whitespace
    line=$(echo "$line" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')

    if [[ "$line" == "[[benchmark]]" ]]; then
        # Save previous benchmark if we have one
        if [[ -n "$current_name" && -n "$current_path" ]]; then
            benchmark_names+=("$current_name")
            benchmark_paths+=("$current_path")
        fi
        current_name=""
        current_path=""
    elif [[ "$line" =~ ^name[[:space:]]*=[[:space:]]*\"(.*)\" ]]; then
        current_name="${BASH_REMATCH[1]}"
    elif [[ "$line" =~ ^path[[:space:]]*=[[:space:]]*\"(.*)\" ]]; then
        current_path="${BASH_REMATCH[1]}"
    fi
done < "$MANIFEST"

# Don't forget the last one
if [[ -n "$current_name" && -n "$current_path" ]]; then
    benchmark_names+=("$current_name")
    benchmark_paths+=("$current_path")
fi

# Run each benchmark
all_results=()
for i in "${!benchmark_names[@]}"; do
    name="${benchmark_names[$i]}"
    path="${benchmark_paths[$i]}"
    full_path="$BENCHMARKS_DIR/$path"

    if [[ ! -f "$full_path" ]]; then
        log_error "Benchmark file not found: $path"
        exit 1
    fi

    log_info "Running: $name"

    # Run multiple iterations and collect timing data
    iteration_results=()
    iteration_pass_data=()
    iteration_memory=()
    binary_size=0
    for ((iter=1; iter<=ITERATIONS; iter++)); do
        output_binary="$TEMP_DIR/bench_output_$$"

        # Run compilation with benchmark JSON output and memory tracking
        # Use /usr/bin/time to capture peak memory usage
        time_output="$TEMP_DIR/time_output_$$"
        if [[ "$os" == "darwin" ]]; then
            # macOS: -l gives max resident set size in bytes
            if ! timing_json=$(/usr/bin/time -l "$RUE_BIN" --benchmark-json "$full_path" -o "$output_binary" 2>"$time_output"); then
                log_error "  Iteration $iter failed"
                cat "$time_output" >&2
                rm -f "$time_output"
                exit 1
            fi
            # Extract max RSS from time output (in bytes on macOS).
            # `|| true`: a missing line is not fatal (pipefail would abort otherwise).
            peak_mem_bytes=$(grep "maximum resident set size" "$time_output" 2>/dev/null | awk '{print $1}' || true)
        else
            # Linux: -v gives max resident set size in KB
            if ! timing_json=$(/usr/bin/time -v "$RUE_BIN" --benchmark-json "$full_path" -o "$output_binary" 2>"$time_output"); then
                log_error "  Iteration $iter failed"
                cat "$time_output" >&2
                rm -f "$time_output"
                exit 1
            fi
            # Extract max RSS from time output (in KB on Linux, convert to bytes).
            # `|| true`: a missing line is not fatal (pipefail would abort otherwise);
            # an empty peak_mem_kb is treated as 0 in the arithmetic below.
            peak_mem_kb=$(grep "Maximum resident set size" "$time_output" 2>/dev/null | awk '{print $NF}' || true)
            peak_mem_bytes=$(( ${peak_mem_kb:-0} * 1024 ))
        fi
        rm -f "$time_output"

        # Parse every timing payload strictly. A malformed payload invalidates
        # the run rather than being silently omitted from the sample set.
        total_ms=$(PYTHONPATH="$SCRIPT_DIR/scripts" python3 -c '
import sys
from benchmark_collection import parse_iteration_json
print(parse_iteration_json(sys.argv[1])["total_ms"])
' "$timing_json")
        iteration_results+=("$total_ms")
        iteration_pass_data+=("$timing_json")
        if [[ -n "$peak_mem_bytes" && "$peak_mem_bytes" -gt 0 ]]; then
            iteration_memory+=("$peak_mem_bytes")
        fi

        # Capture binary size from the last successful iteration
        if [[ -f "$output_binary" ]]; then
            if [[ "$os" == "darwin" ]]; then
                binary_size=$(stat -f%z "$output_binary" 2>/dev/null || echo 0)
            else
                binary_size=$(stat -c%s "$output_binary" 2>/dev/null || echo 0)
            fi
        fi

        # Capture binary size from first successful iteration
        if [[ $binary_size -eq 0 && -f "$output_binary" ]]; then
            binary_size=$(stat -f%z "$output_binary" 2>/dev/null || stat -c%s "$output_binary" 2>/dev/null || echo 0)
        fi

        rm -f "$output_binary"
    done

    if [[ ${#iteration_results[@]} -ne $ITERATIONS ]]; then
        log_error "  Collected ${#iteration_results[@]} of $ITERATIONS iterations for $name"
        exit 1
    fi

    # Calculate mean and stddev for total time
    sum=0
    for val in "${iteration_results[@]}"; do
        sum=$(echo "$sum + $val" | bc -l)
    done
    count=${#iteration_results[@]}
    # Note: bc may output ".123" instead of "0.123" on some platforms.
    # We use printf to ensure proper JSON number formatting.
    mean_raw=$(echo "scale=3; $sum / $count" | bc -l)
    mean=$(printf "%.3f" "$mean_raw")

    # Calculate stddev
    sum_sq=0
    for val in "${iteration_results[@]}"; do
        diff=$(echo "$val - $mean_raw" | bc -l)
        sq=$(echo "$diff * $diff" | bc -l)
        sum_sq=$(echo "$sum_sq + $sq" | bc -l)
    done
    variance=$(echo "scale=6; $sum_sq / $count" | bc -l)
    stddev_raw=$(echo "scale=6; sqrt($variance)" | bc -l)
    stddev=$(printf "%.6f" "$stddev_raw")

    # Calculate mean and stddev for memory usage
    mem_mean=0
    mem_stddev=0
    if [[ ${#iteration_memory[@]} -gt 0 ]]; then
        mem_sum=0
        for val in "${iteration_memory[@]}"; do
            mem_sum=$(echo "$mem_sum + $val" | bc -l)
        done
        mem_count=${#iteration_memory[@]}
        mem_mean=$(echo "scale=0; $mem_sum / $mem_count" | bc -l)

        # Calculate memory stddev
        mem_sum_sq=0
        for val in "${iteration_memory[@]}"; do
            diff=$(echo "$val - $mem_mean" | bc -l)
            sq=$(echo "$diff * $diff" | bc -l)
            mem_sum_sq=$(echo "$mem_sum_sq + $sq" | bc -l)
        done
        mem_variance=$(echo "scale=0; $mem_sum_sq / $mem_count" | bc -l)
        mem_stddev=$(echo "scale=0; sqrt($mem_variance)" | bc -l)
    fi

    # Convert memory to MB for display
    mem_mean_mb=$(echo "scale=2; $mem_mean / 1048576" | bc -l)
    binary_size_kb=$(echo "scale=2; $binary_size / 1024" | bc -l)

    log_info "  $name: time=${mean}ms (±${stddev}), mem=${mem_mean_mb}MB, binary=${binary_size_kb}KB (n=$count)"

    # Every payload is parsed again by the shared strict aggregator. Any
    # malformed pass row or inconsistent timing schema aborts publication.
    extra_json=$(PYTHONPATH="$SCRIPT_DIR/scripts" python3 -c '
import json
import sys
from benchmark_collection import aggregate_iterations
print(json.dumps(aggregate_iterations(sys.argv[1:])))
' "${iteration_pass_data[@]}")

    # Extract components from the JSON
    passes_json=$(echo "$extra_json" | python3 -c "import sys, json; d=json.load(sys.stdin); print(json.dumps(d.get('passes', {})))")
    source_metrics_json=$(echo "$extra_json" | python3 -c "import sys, json; d=json.load(sys.stdin); sm=d.get('source_metrics'); print(json.dumps(sm) if sm else 'null')")
    timing_schema_version=$(echo "$extra_json" | python3 -c "import sys, json; d=json.load(sys.stdin); print(d.get('timing_schema_version', 1))")

    # Store result with all data (including memory and binary size from iteration tracking)
    samples_json="[$(IFS=,; echo "${iteration_results[*]}")]"
    result_parts=("\"name\":\"$name\"" "\"iterations\":$count" "\"mean_ms\":$mean" "\"std_ms\":$stddev" "\"samples_ms\":$samples_json" "\"timing_schema_version\":$timing_schema_version" "\"passes\":$passes_json")
    [[ "$source_metrics_json" != "null" ]] && result_parts+=("\"source_metrics\":$source_metrics_json")
    [[ "$mem_mean" -gt 0 ]] && result_parts+=("\"peak_memory_bytes\":$mem_mean")
    [[ "$binary_size" -gt 0 ]] && result_parts+=("\"binary_size_bytes\":$binary_size")

    all_results+=("{$(IFS=,; echo "${result_parts[*]}")}")
done

# Fail early if no benchmarks were collected
if [[ ${#all_results[@]} -eq 0 ]]; then
    log_error "No benchmark results collected! All benchmarks failed."
    log_error "Check the benchmark programs and compiler output above for errors."
    exit 1
fi

log_info "Successfully collected ${#all_results[@]} benchmark(s)"

# Get metadata
timestamp=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
# Get commit hash - try jj first (for local dev), fall back to git (for CI)
# Note: We check if the result is non-empty because piped commands may succeed but return empty
commit=""
if command -v jj &>/dev/null; then
    # `|| true`: jj may fail (or `head` may SIGPIPE it) outside a jj repo; we
    # fall back to git, then to "unknown", so this must not abort under errexit.
    commit=$(jj log -r @ --no-graph -T 'commit_id' 2>/dev/null || true)
fi
if [[ -z "$commit" ]] && command -v git &>/dev/null; then
    commit=$(git rev-parse HEAD 2>/dev/null || true)
fi
if [[ -z "$commit" ]]; then
    commit="unknown"
fi
host="${arch}-${os}"
runner_image="${RUE_BENCH_RUNNER_IMAGE:-local:${host}}"

# Build final JSON
cat > "$RESULTS_FILE" << EOF
{
  "measurement_family": "compiler",
  "scenario_family": "cold_compilation",
  "version": 1,
  "timestamp": "$timestamp",
  "commit": "$commit",
  "build_mode": "$BUILD_MODE",
  "runner_image": "$runner_image",
  "host": {
    "os": "$os",
    "arch": "$arch"
  },
  "iterations": $ITERATIONS,
  "benchmarks": [
EOF

# Add benchmark results
first=true
for result in "${all_results[@]}"; do
    if [[ "$first" == "true" ]]; then
        first=false
    else
        echo "," >> "$RESULTS_FILE"
    fi
    echo -n "    $result" >> "$RESULTS_FILE"
done

cat >> "$RESULTS_FILE" << EOF

  ]
}
EOF

# Output results
log_info "Benchmark run complete!"
echo ""
cat "$RESULTS_FILE"
echo ""

# Save to specified output file
if [[ -n "$OUTPUT_FILE" ]]; then
    cp "$RESULTS_FILE" "$OUTPUT_FILE"
    log_info "Results saved to: $OUTPUT_FILE"
fi

# Append to history if requested
if [[ "$APPEND_HISTORY" == "true" ]]; then
    # Create history directory if needed
    mkdir -p "$(dirname "$HISTORY_FILE")"

    # Append to history using the Python script
    if [[ -f "$SCRIPT_DIR/scripts/append-benchmark.py" ]]; then
        python3 "$SCRIPT_DIR/scripts/append-benchmark.py" "$RESULTS_FILE" "${HISTORY_FILE%.json}" \
            --legacy-history "$HISTORY_FILE" --platform "$host" \
            --runner-image "local:$host" --reason manual
        log_info "Results appended to history: $HISTORY_FILE"
    else
        log_warn "scripts/append-benchmark.py not found, skipping history append"
        log_warn "Run manually: scripts/append-benchmark.py $RESULTS_FILE $HISTORY_FILE"
    fi
fi

log_info "Done!"
