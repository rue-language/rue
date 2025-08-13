#!/bin/bash

# Continuous Integration Test Script for Rue Test Runner
#
# This script is designed for CI/CD environments and provides
# comprehensive testing with proper exit codes and structured output.

set -euo pipefail

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RUNNER_BINARY="${PROJECT_ROOT}/target/release/rue-runner"
RUE_BINARY="${PROJECT_ROOT}/target/release/rue"

# CI-specific settings
export RUST_LOG="${RUST_LOG:-rue=info}"
export RUST_BACKTRACE="${RUST_BACKTRACE:-1}"

# Colors for output (disabled in CI if NO_COLOR is set)
if [[ "${NO_COLOR:-}" == "" ]]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[1;33m'
    BLUE='\033[0;34m'
    NC='\033[0m'
else
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    NC=''
fi

# Logging with timestamps for CI
timestamp() {
    date '+%Y-%m-%d %H:%M:%S'
}

log_info() {
    echo -e "${BLUE}[$(timestamp)] [INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[$(timestamp)] [SUCCESS]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[$(timestamp)] [WARNING]${NC} $1"
}

log_error() {
    echo -e "${RED}[$(timestamp)] [ERROR]${NC} $1"
}

# Build tools with release optimizations
build_release_tools() {
    log_info "Building Rue compiler and test runner in release mode..."
    
    cd "$PROJECT_ROOT"
    
    # Build with release optimizations
    if ! cargo build --release --bin rue --bin rue-runner; then
        log_error "Failed to build release binaries"
        exit 1
    fi
    
    # Verify binaries exist
    if [[ ! -f "$RUE_BINARY" ]]; then
        log_error "Rue binary not found at: $RUE_BINARY"
        exit 1
    fi
    
    if [[ ! -f "$RUNNER_BINARY" ]]; then
        log_error "Runner binary not found at: $RUNNER_BINARY"
        exit 1
    fi
    
    log_success "Release build completed successfully"
}

# Run comprehensive test suite
run_comprehensive_tests() {
    log_info "Running comprehensive test suite..."
    
    local report_file="ci-test-report.json"
    local exit_code=0
    
    # Run tests with comprehensive reporting
    if ! "$RUNNER_BINARY" \
        --rue-binary "$RUE_BINARY" \
        --test-paths tests/runner \
        --test-paths examples \
        --spec-file spec/norms.toml \
        --snapshot-dir tests/runner/snapshots \
        --report-file "$report_file" \
        --verbose; then
        exit_code=1
    fi
    
    # Parse and display results
    if [[ -f "$report_file" ]] && command -v jq &> /dev/null; then
        log_info "Test Results Summary:"
        
        local total=$(jq -r '.summary.total' "$report_file")
        local passed=$(jq -r '.summary.passed' "$report_file")
        local failed=$(jq -r '.summary.failed' "$report_file")
        local skipped=$(jq -r '.summary.skipped' "$report_file")
        local duration=$(jq -r '.summary.duration_ms' "$report_file")
        
        echo "  Total Tests: $total"
        echo "  Passed: $passed"
        echo "  Failed: $failed"
        echo "  Skipped: $skipped"
        echo "  Duration: ${duration}ms"
        
        # Calculate pass rate
        if [[ $total -gt 0 ]]; then
            local pass_rate=$(echo "scale=1; $passed * 100 / $total" | bc -l 2>/dev/null || echo "0")
            echo "  Pass Rate: ${pass_rate}%"
        fi
        
        # Show failures if any
        if [[ $failed -gt 0 ]]; then
            log_warning "Failed tests:"
            jq -r '.results[] | select(.result.status == "Fail") | "  - \(.path): \(.result.details.reason)"' "$report_file"
        fi
        
        # Specification coverage
        log_info "Specification Coverage:"
        jq -r '.results[] | select(.spec_references | length > 0) | .spec_references[]' "$report_file" | \
        sort | uniq -c | sort -nr | head -5 | \
        while read count spec; do
            echo "  $spec: $count tests"
        done
    else
        log_warning "Could not parse test report (jq not available or report missing)"
    fi
    
    return $exit_code
}

# Run unit tests for the runner itself
run_runner_unit_tests() {
    log_info "Running unit tests for test runner..."
    
    cd "$PROJECT_ROOT"
    
    if ! cargo test --package rue-runner; then
        log_error "Runner unit tests failed"
        return 1
    fi
    
    log_success "Runner unit tests passed"
}

# Validate test examples
validate_test_examples() {
    log_info "Validating test examples syntax..."
    
    local validation_errors=0
    
    # Check all .rue files in tests directory for basic syntax
    while IFS= read -r -d '' file; do
        log_info "Validating: $file"
        
        # Check for basic syntax with compile-only mode
        if ! "$RUE_BINARY" --compile-only "$file" >/dev/null 2>&1; then
            # Some tests are expected to fail compilation, so this is not always an error
            # We just log it for awareness
            log_info "  Note: $file does not compile (may be intentional for compile-fail tests)"
        fi
        
        # Check for test directives
        if ! grep -q "//!@" "$file"; then
            log_warning "  No test directives found in: $file"
        fi
        
    done < <(find tests/runner -name "*.rue" -print0)
    
    if [[ $validation_errors -eq 0 ]]; then
        log_success "Test example validation completed"
    else
        log_error "Found $validation_errors validation errors"
        return 1
    fi
}

# Performance regression check
check_performance_regression() {
    log_info "Checking for performance regressions..."
    
    local baseline_file="baseline-test-times.json"
    local current_report="current-test-times.json"
    
    # Run tests with timing
    "$RUNNER_BINARY" \
        --rue-binary "$RUE_BINARY" \
        --test-paths tests/runner \
        --spec-file spec/norms.toml \
        --snapshot-dir tests/runner/snapshots \
        --report-file "$current_report" \
        --ignore-failures >/dev/null 2>&1
    
    if [[ -f "$baseline_file" ]] && [[ -f "$current_report" ]] && command -v jq &> /dev/null; then
        local baseline_duration=$(jq -r '.summary.duration_ms' "$baseline_file" 2>/dev/null || echo "0")
        local current_duration=$(jq -r '.summary.duration_ms' "$current_report" 2>/dev/null || echo "0")
        
        if [[ $baseline_duration -gt 0 ]] && [[ $current_duration -gt 0 ]]; then
            local ratio=$(echo "scale=2; $current_duration / $baseline_duration" | bc -l 2>/dev/null || echo "1")
            
            echo "  Baseline duration: ${baseline_duration}ms"
            echo "  Current duration: ${current_duration}ms"
            echo "  Performance ratio: ${ratio}x"
            
            # Flag significant regressions (>50% slower)
            if (( $(echo "$ratio > 1.5" | bc -l 2>/dev/null || echo "0") )); then
                log_warning "Potential performance regression detected (${ratio}x slower)"
                return 1
            fi
        fi
    else
        log_info "No baseline available for performance comparison"
    fi
    
    log_success "Performance check completed"
}

# Main CI pipeline
main() {
    cd "$PROJECT_ROOT"
    
    log_info "Starting CI test pipeline for Rue Test Runner"
    
    local overall_exit_code=0
    
    # Step 1: Build tools
    if ! build_release_tools; then
        overall_exit_code=1
    fi
    
    # Step 2: Run runner unit tests
    if ! run_runner_unit_tests; then
        overall_exit_code=1
    fi
    
    # Step 3: Validate test examples
    if ! validate_test_examples; then
        overall_exit_code=1
    fi
    
    # Step 4: Run comprehensive tests
    if ! run_comprehensive_tests; then
        overall_exit_code=1
    fi
    
    # Step 5: Performance check (non-blocking)
    check_performance_regression || log_warning "Performance check had issues (non-blocking)"
    
    # Final result
    if [[ $overall_exit_code -eq 0 ]]; then
        log_success "All CI tests passed!"
    else
        log_error "Some CI tests failed"
    fi
    
    exit $overall_exit_code
}

# Handle script interruption
trap 'log_error "CI pipeline interrupted"; exit 130' INT TERM

# Run main function
main "$@"