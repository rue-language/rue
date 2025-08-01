#!/bin/bash
set -euo pipefail

echo "Generating benchmark programs..."
mkdir -p bench-programs

# Small program - baseline
echo "Creating small.rue..."
cat > bench-programs/small.rue << 'EOF'
fn main() -> i32 {
    42
}
EOF

# Medium program - realistic single file
echo "Creating medium.rue..."
cat > bench-programs/medium.rue << 'EOF'
fn factorial(n: i32) -> i32 {
    if n <= 1 { 1 } else { n * factorial(n - 1) }
}

fn fibonacci(n: i32) -> i32 {
    let a: i32 = 0;
    let b: i32 = 1;
    let i: i32 = 0;
    while i < n {
        let temp: i32 = a + b;
        a = b;
        b = temp;
        i = i + 1;
    };
    a
}

fn gcd(a: i32, b: i32) -> i32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

fn main() -> i32 {
    factorial(5) + fibonacci(10) + gcd(48, 18)
}
EOF

# Large program - stress test
echo "Creating large.rue..."
python3 scripts/generate-large-program.py > bench-programs/large.rue

echo "Benchmark programs generated successfully:"
echo "  - bench-programs/small.rue ($(wc -l < bench-programs/small.rue) lines)"
echo "  - bench-programs/medium.rue ($(wc -l < bench-programs/medium.rue) lines)"
echo "  - bench-programs/large.rue ($(wc -l < bench-programs/large.rue) lines)"