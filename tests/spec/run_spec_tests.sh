#!/bin/bash
# Run specification compliance tests using rue-runner

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}Running Rue Specification Compliance Tests${NC}"
echo "=========================================="

# Build the compiler first
echo "Building rue compiler..."
cargo build --bin rue --quiet

# Run spec tests with rue-runner
echo "Running spec tests..."
cargo run -p rue-runner -- \
    --test-paths tests/spec \
    --rue-binary target/debug/rue \
    "$@"

exit_code=$?

if [ $exit_code -eq 0 ]; then
    echo -e "${GREEN}✓ All spec tests passed!${NC}"
else
    echo -e "${RED}✗ Some spec tests failed${NC}"
    exit $exit_code
fi