#!/bin/bash

# Rue Test Runner Execution Script
#
# This script provides various ways to run the Rue test suite
# using the comprehensive test runner framework.

set -euo pipefail

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RUNNER_BINARY="${PROJECT_ROOT}/target/debug/rue-runner"
RUE_BINARY="${PROJECT_ROOT}/target/debug/rue"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Logging functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Build the test runner and rue compiler
build_tools() {
    log_info "Building Rue compiler and test runner..."
    
    cd "$PROJECT_ROOT"
    
    # Build the main rue compiler
    if ! cargo build --bin rue; then
        log_error "Failed to build rue compiler"
        exit 1
    fi
    
    # Build the test runner
    if ! cargo build --bin rue-runner; then
        log_error "Failed to build rue-runner"
        exit 1
    fi
    
    log_success "Build completed successfully"
}

# Run basic test suite
run_basic_tests() {
    log_info "Running basic test suite..."
    
    "$RUNNER_BINARY" \
        --rue-binary "$RUE_BINARY" \
        --test-paths tests/runner \
        --spec-file spec/norms.toml \
        --snapshot-dir tests/runner/snapshots \
        --verbose
    
    local exit_code=$?
    if [[ $exit_code -ne 0 ]]; then
        log_error "Test suite failed with exit code $exit_code"
        exit $exit_code
    fi
    
    log_success "All tests passed!"
}

# Run tests with JSON report
run_tests_with_report() {
    local report_file="${1:-test-report.json}"
    
    log_info "Running tests with JSON report output..."
    
    "$RUNNER_BINARY" \
        --rue-binary "$RUE_BINARY" \
        --test-paths tests/runner \
        --spec-file spec/norms.toml \
        --snapshot-dir tests/runner/snapshots \
        --report-file "$report_file" \
        --verbose
        
    if [[ -f "$report_file" ]]; then
        log_success "Test report generated: $report_file"
        
        # Show summary from JSON report
        if command -v jq &> /dev/null; then
            echo
            log_info "Test Summary:"
            jq -r '.summary | "Total: \(.total), Passed: \(.passed), Failed: \(.failed), Skipped: \(.skipped)"' "$report_file"
        fi
    fi
}

# Update golden snapshots
update_snapshots() {
    log_info "Updating golden snapshots..."
    
    "$RUNNER_BINARY" \
        --rue-binary "$RUE_BINARY" \
        --test-paths tests/runner \
        --spec-file spec/norms.toml \
        --snapshot-dir tests/runner/snapshots \
        --update-snapshots \
        --verbose
        
    log_success "Snapshots updated"
}

# Run tests with filter
run_filtered_tests() {
    local filter="${1:-.*}"
    
    log_info "Running filtered tests (pattern: $filter)..."
    
    "$RUNNER_BINARY" \
        --rue-binary "$RUE_BINARY" \
        --test-paths tests/runner \
        --spec-file spec/norms.toml \
        --snapshot-dir tests/runner/snapshots \
        --filter "$filter" \
        --verbose
}

# Run normative specification compliance tests
run_spec_compliance() {
    log_info "Running normative specification compliance tests..."
    
    # Run all tests and generate detailed report
    local report_file="spec-compliance-report.json"
    
    "$RUNNER_BINARY" \
        --rue-binary "$RUE_BINARY" \
        --test-paths tests/runner examples \
        --spec-file spec/norms.toml \
        --snapshot-dir tests/runner/snapshots \
        --report-file "$report_file" \
        --verbose
        
    if [[ -f "$report_file" ]] && command -v jq &> /dev/null; then
        echo
        log_info "Specification Compliance Summary:"
        
        # Extract specification coverage
        jq -r '.results[] | select(.spec_references | length > 0) | .spec_references[]' "$report_file" | \
        sort | uniq -c | sort -nr | head -10 | \
        while read count spec; do
            echo "  $spec: $count tests"
        done
    fi
}

# Benchmark test execution
benchmark_tests() {
    log_info "Benchmarking test execution performance..."
    
    local iterations=3
    local total_time=0
    
    for i in $(seq 1 $iterations); do
        log_info "Iteration $i/$iterations..."
        
        local start_time=$(date +%s%N)
        
        "$RUNNER_BINARY" \
            --rue-binary "$RUE_BINARY" \
            --test-paths tests/runner \
            --spec-file spec/norms.toml \
            --snapshot-dir tests/runner/snapshots \
            --ignore-failures > /dev/null 2>&1
            
        local end_time=$(date +%s%N)
        local iteration_time=$(( (end_time - start_time) / 1000000 )) # Convert to milliseconds
        
        echo "  Time: ${iteration_time}ms"
        total_time=$((total_time + iteration_time))
    done
    
    local avg_time=$((total_time / iterations))
    log_success "Average execution time: ${avg_time}ms"
}

# Show usage information
show_usage() {
    echo "Rue Test Runner Execution Script"
    echo
    echo "Usage: $0 [command] [options]"
    echo
    echo "Commands:"
    echo "  build                 Build rue compiler and test runner"
    echo "  test                  Run basic test suite"
    echo "  report [file]         Run tests with JSON report (default: test-report.json)"
    echo "  update-snapshots      Update golden snapshots"
    echo "  filter <pattern>      Run tests matching regex pattern"
    echo "  spec-compliance       Run normative specification compliance tests"
    echo "  benchmark             Benchmark test execution performance"
    echo "  help                  Show this help message"
    echo
    echo "Examples:"
    echo "  $0 build              # Build tools"
    echo "  $0 test               # Run all tests"
    echo "  $0 report results.json # Generate JSON report"
    echo "  $0 filter 'compile.*' # Run only compile tests"
    echo "  $0 update-snapshots   # Update golden snapshots"
}

# Main script logic
main() {
    cd "$PROJECT_ROOT"
    
    local command="${1:-test}"
    
    case "$command" in
        "build")
            build_tools
            ;;
        "test")
            build_tools
            run_basic_tests
            ;;
        "report")
            build_tools
            run_tests_with_report "${2:-test-report.json}"
            ;;
        "update-snapshots")
            build_tools
            update_snapshots
            ;;
        "filter")
            if [[ $# -lt 2 ]]; then
                log_error "Filter pattern required"
                show_usage
                exit 1
            fi
            build_tools
            run_filtered_tests "$2"
            ;;
        "spec-compliance")
            build_tools
            run_spec_compliance
            ;;
        "benchmark")
            build_tools
            benchmark_tests
            ;;
        "help"|"-h"|"--help")
            show_usage
            ;;
        *)
            log_error "Unknown command: $command"
            show_usage
            exit 1
            ;;
    esac
}

# Run main function with all arguments
main "$@"