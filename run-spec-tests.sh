#!/bin/bash
# Run the Rue specification-based test suite

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}Building Rue compiler and test runner...${NC}"
cargo build -p rue -p rue-runner

echo -e "${GREEN}Running specification tests...${NC}"
./target/debug/rue-runner \
    --test-paths tests \
    --rue-binary ./target/debug/rue \
    --spec-file spec/norms.toml \
    "$@"

# Check exit code
if [ $? -eq 0 ]; then
    echo -e "${GREEN}✓ All tests passed!${NC}"
else
    echo -e "${RED}✗ Some tests failed${NC}"
    exit 1
fi