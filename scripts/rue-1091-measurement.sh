#!/usr/bin/env bash
#
# Run the RUE-1091 future post-flip structural/activation gauntlet.
#
# This is not the current-production value audit. The reproducible value audit
# has its own black-box runner in scripts/rue-value-audit.py so a historical
# pre-query revision can remain contextual when it cannot consume this
# post-flip harness.
#
# This script is intentionally orchestration only. The structural assertions
# remain in the RUE-1090/RUE-1121 harnesses, and timing is never collected from
# the counting-allocator binary.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

start_stage=1
pre_flip=0
results_dir=""
caldera_budget_seconds="${RUE_1091_CALDERA_BUDGET_SECONDS:-300}"

usage() {
    cat <<'EOF'
usage: scripts/rue-1091-measurement.sh [--pre-flip] [--stage N] [--results-dir DIR]

Run stages N through 9 (default: all stages):

  1  RUE-1090 normal matrices (both axes plus identity)
  2  RUE-1090 large declaration matrix (adds the 10,000 row)
  3  RUE-1121 exact-flat and edit-invalidation rows
  4  warm/fresh differential oracles
  5  forced-eviction and cancellation suites
  6  whole-body schedule permutations
  7  allocation accounting (counting allocator; no timing)
  8  cold/warm timing and peak memory (ordinary allocator)
  9  Caldera cold compile with the 300-second budget

--stage N resumes at stage N; it does not rerun earlier stages.
--pre-flip marks only stage 9 --allow-fail. This is the plumbing-validation
mode for current trunk; every other stage remains required to pass.

Logs and summary.tsv are written to a fresh temporary directory unless
--results-dir is supplied. The directory is retained after the run.

RUE_1091_CALDERA_BUDGET_SECONDS overrides the Caldera timeout for operator
testing. Leave it unset for the authoritative 300-second post-flip run.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --pre-flip)
            pre_flip=1
            shift
            ;;
        --stage)
            if [[ $# -lt 2 || ! "$2" =~ ^[1-9]$ ]]; then
                echo "error: --stage requires an integer from 1 through 9" >&2
                exit 2
            fi
            start_stage="$2"
            shift 2
            ;;
        --results-dir)
            if [[ $# -lt 2 || -z "$2" ]]; then
                echo "error: --results-dir requires a path" >&2
                exit 2
            fi
            results_dir="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown argument '$1'" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ ! "$caldera_budget_seconds" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: RUE_1091_CALDERA_BUDGET_SECONDS must be a positive integer" >&2
    exit 2
fi

if [[ -z "$results_dir" ]]; then
    results_dir="$(mktemp -d "${TMPDIR:-/tmp}/rue-1091-measurement.XXXXXX")"
else
    mkdir -p "$results_dir"
    results_dir="$(cd "$results_dir" && pwd)"
fi

summary="$results_dir/summary.tsv"
if [[ "$start_stage" -gt 1 && ! -f "$summary" ]]; then
    echo "error: --stage $start_stage requires an existing $summary" >&2
    exit 2
fi
if [[ "$start_stage" -eq 1 ]]; then
    printf 'stage\tname\tresult\texit_code\torchestration_seconds\tlog\n' >"$summary"
else
    summary_prefix="$results_dir/summary-prefix.tsv"
    awk -F '\t' -v resume="$start_stage" \
        'NR == 1 || ($1 ~ /^[0-9]+$/ && $1 < resume)' \
        "$summary" >"$summary_prefix"
    mv "$summary_prefix" "$summary"
fi

run_with_timeout() {
    local seconds="$1"
    shift
    python3 - "$seconds" "$@" <<'PY'
import os
import signal
import subprocess
import sys

budget = float(sys.argv[1])
command = sys.argv[2:]
process = subprocess.Popen(command, start_new_session=True)
try:
    raise SystemExit(process.wait(timeout=budget))
except subprocess.TimeoutExpired:
    os.killpg(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()
    print(f"TIMEOUT: command exceeded {budget:g} seconds", file=sys.stderr)
    raise SystemExit(124)
PY
}

stage_1() {
    scripts/rue unit compiler:scaling-matrix-test \
        scaling_matrix_ --nocapture
}

stage_2() {
    RUE_SCALING_LARGE=1 \
        scripts/rue unit compiler:scaling-matrix-test \
        scaling_matrix_fixed_bodies_growing_declarations --nocapture
}

stage_3() {
    set -e
    # The exact-flat cold rows are emitted by stages 1 and 2 from the shared
    # RUE-1090/RUE-1121 matrix. Exercise the small exact-flat row again so this
    # stage remains independently auditable, then run every edit row.
    scripts/rue unit compiler \
        scaling_smoke_fixed_bodies_growing_declarations --nocapture
    scripts/rue unit compiler invalidation_ --nocapture
}

stage_4() {
    scripts/rue unit \
        compiler:rue-compiler-differential-oracle-test --nocapture
}

stage_5() {
    set -e
    scripts/rue unit compiler cancellation --nocapture
    scripts/rue unit compiler eviction --nocapture
}

stage_6() {
    scripts/rue unit compiler \
        side_a_is_self_equal_across_full_shape_corpus_and_schedule_permutations \
        --nocapture
}

stage_7() {
    set -e
    # Do not add timing around the measured compiler intervals in this stage:
    # this binary installs the counting allocator.
    ./buck2 run --target-platforms //platforms:release \
        //crates/rue-scaling-bench:rue-scaling-bench-allocations -- \
        --mode alloc --bodies 1000 --decls 100
    ./buck2 run --target-platforms //platforms:release \
        //crates/rue-scaling-bench:rue-scaling-bench-allocations -- \
        --mode alloc --warm --bodies 1000 --decls 100
}

stage_8() {
    set -e
    # R5: these are separate processes built without the counting allocator.
    ./buck2 run --target-platforms //platforms:release \
        //crates/rue-scaling-bench:rue-scaling-bench -- \
        --mode timing --bodies 1000 --decls 100 --iterations 5 --json
    ./buck2 run --target-platforms //platforms:release \
        //crates/rue-scaling-bench:rue-scaling-bench -- \
        --mode timing --warm --bodies 1000 --decls 100 --iterations 20 --json
    ./buck2 run --target-platforms //platforms:release \
        //crates/rue-scaling-bench:rue-scaling-bench -- \
        --mode memory --bodies 1000 --decls 100
}

stage_9() {
    set -e
    local rue_binary
    rue_binary="$(scripts/rue-bin --target-platforms //platforms:release)"
    run_with_timeout "$caldera_budget_seconds" env \
        RUE_STD_PATH="$repo_root/std" \
        "$rue_binary" --time-passes examples/caldera/main.rue \
        -o "$results_dir/caldera"
}

stage_names=(
    ""
    "rue-1090-normal-matrices"
    "rue-1090-large-10k-declaration-matrix"
    "rue-1121-rows"
    "differential-oracles"
    "forced-eviction-and-cancellation"
    "schedule-permutations"
    "counting-allocator"
    "timing-and-memory"
    "caldera-300-second-budget"
)

required_failure=0
for stage in $(seq "$start_stage" 9); do
    name="${stage_names[$stage]}"
    log="$results_dir/stage-${stage}-${name}.log"
    allow_fail=0
    if [[ "$stage" -eq 9 && "$pre_flip" -eq 1 ]]; then
        allow_fail=1
    fi

    echo
    echo "== stage $stage/9: $name =="
    if [[ "$allow_fail" -eq 1 ]]; then
        echo "policy: --allow-fail (pre-flip Caldera only)"
    else
        echo "policy: required"
    fi
    echo "log: $log"

    started="$SECONDS"
    set +e
    "stage_$stage" 2>&1 | tee "$log"
    status=${PIPESTATUS[0]}
    set -e
    elapsed=$((SECONDS - started))

    if [[ "$status" -eq 0 ]]; then
        result="PASS"
    elif [[ "$allow_fail" -eq 1 ]]; then
        result="ALLOW_FAIL"
    else
        result="FAIL"
        required_failure=1
    fi
    printf '%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$stage" "$name" "$result" "$status" "$elapsed" "$log" >>"$summary"
    echo "stage $stage: $result (exit=$status, orchestration=${elapsed}s)"

    if [[ "$required_failure" -ne 0 ]]; then
        echo "stopping after required stage failure; resume with --stage $stage" >&2
        break
    fi
done

echo
echo "summary: $summary"
if [[ "$required_failure" -ne 0 ]]; then
    exit 1
fi

ledger_error="$(
    awk -F '\t' -v pre_flip="$pre_flip" '
        NR == 1 {
            if ($1 != "stage" || $3 != "result") {
                print "invalid summary header"
            }
            next
        }
        {
            rows++
            if ($1 != rows) {
                print "missing or out-of-order stage: expected " rows ", found " $1
            }
            allowed_pre_flip = pre_flip && $1 == 9 && $3 == "ALLOW_FAIL"
            if ($3 != "PASS" && !allowed_pre_flip) {
                print "non-passing stage " $1 ": " $3
            }
        }
        END {
            if (rows != 9) {
                print "incomplete ledger: expected 9 stages, found " rows
            }
        }
    ' "$summary"
)"
if [[ -n "$ledger_error" ]]; then
    echo "gauntlet failed ledger validation:" >&2
    echo "$ledger_error" >&2
    exit 1
fi
echo "gauntlet complete"
