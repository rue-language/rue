#!/bin/bash
# Rue compiler wrapper script
# 
# This script sets up the environment variables needed for the Rue compiler
# and then executes the actual rue binary with all passed arguments.

set -euo pipefail

# Get the script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Get the workspace root directory - handle both direct execution and Buck2 execution
if [ -f "${SCRIPT_DIR}/../../BUCK" ]; then
    # Direct execution - script is in tools/rue/
    WORKSPACE_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
else
    # Buck2 execution - script is in buck-out somewhere
    # Find the workspace root by looking for the BUCK file
    CURRENT_DIR="${SCRIPT_DIR}"
    while [ "$CURRENT_DIR" != "/" ]; do
        if [ -f "$CURRENT_DIR/BUCK" ] && [ -d "$CURRENT_DIR/crates" ]; then
            WORKSPACE_ROOT="$CURRENT_DIR"
            break
        fi
        CURRENT_DIR="$(dirname "$CURRENT_DIR")"
    done
    
    # If we couldn't find the workspace root, use PWD as a fallback
    if [ -z "${WORKSPACE_ROOT:-}" ]; then
        WORKSPACE_ROOT="$(pwd)"
    fi
fi

# If RUE_RUNTIME_LIB is already set (e.g., by Buck2 test env), use it
# Otherwise, find the runtime library
if [ -z "${RUE_RUNTIME_LIB:-}" ]; then
    # Set the path to the consolidated runtime library
    # Find the most recent librue_runtime.a file in the Buck output directory
    RUE_RUNTIME_DEFAULT=$(find "${WORKSPACE_ROOT}/buck-out" -name "librue_runtime.a" -type f 2>/dev/null | head -1)
    export RUE_RUNTIME_LIB="${RUE_RUNTIME_DEFAULT}"
fi

# For backward compatibility, set RUE_CRT0_LIB to the same value
# since they are now the same consolidated library
export RUE_CRT0_LIB="${RUE_RUNTIME_LIB}"

# Find the rue binary - prefer v2 over rust-analyzer cache
RUE_BINARY=$(find "${WORKSPACE_ROOT}/buck-out/v2" -name "rue" -type f -path "*/crates/rue/*" 2>/dev/null | head -1)

# Check if we found the rue binary
if [ -z "$RUE_BINARY" ]; then
    echo "Error: Could not find the rue binary in buck-out. Make sure to build it first with: ./buck2 build //crates/rue:rue" >&2
    exit 1
fi

# Check if we found the runtime library
if [ -z "$RUE_RUNTIME_LIB" ] || [ ! -f "$RUE_RUNTIME_LIB" ]; then
    echo "Error: Could not find the runtime library. Make sure to build it first with: ./buck2 build //crates/rue-runtime:rue-runtime-static" >&2
    echo "Looked for: $RUE_RUNTIME_LIB" >&2
    exit 1
fi

# Enable for debugging:
# echo "Using rue binary: $RUE_BINARY" >&2
# echo "Using runtime library: $RUE_CRT0_LIB" >&2

# Execute the actual rue binary with all arguments
exec "$RUE_BINARY" "$@"