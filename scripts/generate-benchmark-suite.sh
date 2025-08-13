#!/bin/bash
set -euo pipefail

echo "Generating benchmark suite for performance testing..."
mkdir -p bench-programs

# Skip small/medium programs for CI benchmarking - they're too fast to measure reliably

# Large program - ~4400 lines (500 functions)
echo "Creating large.rue (500 functions)..."
python3 scripts/generate-large-program.py 500 > bench-programs/large.rue

# Extra large program - ~8800 lines (1000 functions)  
echo "Creating xlarge.rue (1000 functions)..."
python3 scripts/generate-large-program.py 1000 > bench-programs/xlarge.rue

# Huge program - ~17600 lines (2000 functions) - real stress test
echo "Creating huge.rue (2000 functions)..."
python3 scripts/generate-large-program.py 2000 > bench-programs/huge.rue

echo "Benchmark suite generated:"
echo "  - bench-programs/large.rue ($(wc -l < bench-programs/large.rue) lines)"
echo "  - bench-programs/xlarge.rue ($(wc -l < bench-programs/xlarge.rue) lines)"
echo "  - bench-programs/huge.rue ($(wc -l < bench-programs/huge.rue) lines)"

# Estimate compilation times (very rough)
echo ""
echo "Expected compilation times (rough estimate):"
echo "  - large.rue: ~50-100ms"
echo "  - xlarge.rue: ~100-200ms"
echo "  - huge.rue: ~200-500ms"
echo ""
echo "These should be large enough to measure reliably in CI."