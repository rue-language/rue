#!/usr/bin/env bash
#
# Snapshot test management script for the Rue project
#
# Usage:
#   ./scripts/update-snapshots.sh           # Update all snapshots
#   ./scripts/update-snapshots.sh parser    # Update parser snapshots only
#   ./scripts/update-snapshots.sh --check   # Check all snapshots
#   ./scripts/update-snapshots.sh --list    # List available targets
#   ./scripts/update-snapshots.sh --help    # Show this help

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Change to project root
cd "$PROJECT_ROOT"

# Ensure buck2 is available
if ! command -v ./buck2 &> /dev/null; then
    echo -e "${RED}Error: buck2 not found at ./buck2${NC}"
    echo "Please ensure dotslash is installed and buck2 is set up"
    exit 1
fi

# Function to print usage
show_help() {
    cat << EOF
Snapshot test management for the Rue project

Usage:
  $(basename "$0") [OPTIONS] [TARGET]

Options:
  --check       Check all snapshots (don't update)
  --list        List all available snapshot targets
  --help        Show this help message

Target:
  If specified, only update snapshots for that target (e.g., 'parser', 'cli')
  Otherwise, updates all snapshots

Examples:
  $(basename "$0")                  # Update all snapshots
  $(basename "$0") parser           # Update parser snapshots only
  $(basename "$0") cli              # Update CLI test snapshots only
  $(basename "$0") --check          # Check all snapshots
  $(basename "$0") --list           # List available targets

EOF
}

# Function to update all snapshots
update_all() {
    echo -e "${BLUE}Updating all snapshots...${NC}"
    
    # Find all update targets and run them
    local targets=$(./buck2 query "kind(rust_test, //...)" 2>/dev/null | grep "_update$" || true)
    
    if [[ -z "$targets" ]]; then
        echo -e "${YELLOW}No snapshot update targets found${NC}"
        return 1
    fi
    
    local failed=0
    for target in $targets; do
        echo -e "${BLUE}Updating: $target${NC}"
        if ./buck2 test "$target" 2>&1 | tail -1 | grep -q "Pass 1"; then
            echo -e "${GREEN}✓ Updated${NC}"
        else
            echo -e "${RED}✗ Failed${NC}"
            failed=$((failed + 1))
        fi
    done
    
    if [[ $failed -eq 0 ]]; then
        echo -e "${GREEN}✓ All snapshots updated successfully${NC}"
    else
        echo -e "${RED}✗ $failed snapshot updates failed${NC}"
        return 1
    fi
}

# Function to check all snapshots
check_all() {
    echo -e "${BLUE}Checking all snapshots...${NC}"
    
    # Find all snapshot test targets (non-update ones)
    local targets=$(./buck2 query "kind(rust_test, //...)" 2>/dev/null | grep -E "snapshot|cli_tests" | grep -v "_update$" || true)
    
    if [[ -z "$targets" ]]; then
        echo -e "${YELLOW}No snapshot test targets found${NC}"
        return 1
    fi
    
    local failed=0
    for target in $targets; do
        echo -e "${BLUE}Checking: $target${NC}"
        if ./buck2 test "$target" 2>&1 | tail -1 | grep -q "Pass 1"; then
            echo -e "${GREEN}✓ Passed${NC}"
        else
            echo -e "${RED}✗ Failed${NC}"
            failed=$((failed + 1))
        fi
    done
    
    if [[ $failed -eq 0 ]]; then
        echo -e "${GREEN}✓ All snapshots are up to date${NC}"
    else
        echo -e "${RED}✗ $failed snapshot tests failed${NC}"
        echo "Run $(basename "$0") to update snapshots"
        return 1
    fi
}

# Function to list targets
list_targets() {
    echo -e "${BLUE}Available snapshot test targets:${NC}"
    echo
    echo -e "${YELLOW}Regular test targets (fail on mismatch):${NC}"
    ./buck2 query "kind(rust_test, //...)" 2>/dev/null | grep -E "snapshot|cli_tests" | grep -v "_update$" | sort
    echo
    echo -e "${YELLOW}Update targets (update snapshots):${NC}"
    ./buck2 query "kind(rust_test, //...)" 2>/dev/null | grep "_update$" | sort
}

# Function to update specific target
update_target() {
    local target=$1
    echo -e "${BLUE}Updating snapshots for: $target${NC}"
    
    # Map common names to actual targets
    case "$target" in
        parser)
            ./buck2 test //crates/rue-parser:test_parser_snapshots_update
            ;;
        cli|cli_tests)
            ./buck2 test //crates/rue:cli_tests_update
            ;;
        corpus)
            ./buck2 test //crates/rue:snapshot_corpus_tests_update
            ;;
        *)
            # Try to find a matching target
            echo -e "${YELLOW}Looking for target matching '$target'...${NC}"
            
            # Try direct target first
            if ./buck2 test "//crates/rue:${target}_update" 2>/dev/null; then
                echo -e "${GREEN}✓ Updated $target${NC}"
            elif ./buck2 test "//crates/rue-${target}:test_update" 2>/dev/null; then
                echo -e "${GREEN}✓ Updated $target${NC}"
            else
                echo -e "${RED}Error: Could not find snapshot update target for '$target'${NC}"
                echo "Run with --list to see available targets"
                exit 1
            fi
            ;;
    esac
}

# Main script logic
main() {
    if [[ $# -eq 0 ]]; then
        update_all
    else
        case "$1" in
            --help|-h)
                show_help
                ;;
            --check)
                check_all
                ;;
            --list)
                list_targets
                ;;
            --*)
                echo -e "${RED}Unknown option: $1${NC}"
                show_help
                exit 1
                ;;
            *)
                update_target "$1"
                ;;
        esac
    fi
}

main "$@"