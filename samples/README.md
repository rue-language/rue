# Rue Sample Programs

This directory contains sample Rue programs used for testing and demonstration. The table below lists all programs with their expected outputs, which are used by the integration test harness.

## Test Programs

| File | Description | Exit Code | Expected Stdout | Expected Stderr |
|------|-------------|-----------|-----------------|-----------------|
| assignment_demo.rue | Variable assignment demonstration | 6 | | |
| assignment_spec_example.rue | Assignment example from spec | 15 | | |
| casting.rue | Type casting demonstration | 0 | 42<br>-100<br>150 | |
| countdown.rue | Loop countdown example | 42 | | |
| division_test.rue | Division operation test | 10 | | |
| factorial.rue | Factorial calculation (5!) | 120 | | |
| fibonacci.rue | Fibonacci sequence (n=10) | 55 | | |
| hardcoded_test.rue | Basic hardcoded test | 0 | | |
| hir_demo.rue | HIR demonstration | 30 | | |
| if_demo.rue | Conditional expression demo | 5 | | |
| large_div_immediate.rue | Large immediate division | 1 | | |
| large_immediate.rue | Large immediate value test | 0 | | |
| negative_literals.rue | Negative literal handling | 209 | | |
| simple.rue | Simple return value | 42 | | |
| simple_assignment.rue | Simple assignment test | 100 | | |
| test_100.rue | Test printing 100 | 0 | 100 | |
| test_add_order.rue | Addition order test | 30 | | |
| test_all_runtime.rue | Runtime function tests | 0 | 1234567890<br>-1<br>42<br>true<br>false<br>true<br>()<br>5 | |
| test_bool_simple.rue | Boolean operations | 0 | | |
| test_compare_fib.rue | Fibonacci comparison | 2 | | |
| test_count_calls.rue | Function call counting | ? | | |
| test_different_recursion.rue | Different recursion test | 0 | 30<br>5 | |
| test_different_returns.rue | Multiple return values | 7 | | |
| test_direct_return.rue | Direct return test | 5 | | |
| test_double_calls.rue | Double function calls | 10 | | |
| test_exit.rue | Exit function test | 42 | | |
| test_fib_minimal.rue | Minimal fibonacci | 5 | | |
| test_i32_bool.rue | i32 and bool operations | ? | | |
| test_io_demo.rue | I/O demonstration | ? | | |
| test_isolated_add.rue | Isolated addition | ? | | |
| test_minimal.rue | Minimal test | 0 | | |
| test_mod_zero.rue | Modulo by zero test | ? | | (runtime error) |
| test_println.rue | Print line test | 0 | 42<br>100<br>true<br>false<br>() | |
| test_recursive_simple.rue | Simple recursion | ? | | |
| test_simple.rue | Simple test | 0 | 42 | |
| test_single_line.rue | Single line test | ? | | |
| test_two_calls.rue | Two function calls | 5 | | |
| test_two_vars.rue | Two variables test | ? | | |
| test_unit.rue | Unit type test | 0 | () | |
| test_var_between_calls.rue | Variable between calls | ? | | |
| test_zero.rue | Zero test | 0 | | |
| unit_literal.rue | Unit literal test | 0 | | |
| unit_literal_io.rue | Unit literal with I/O | 0 | ()<br>() | |
| while_demo.rue | While loop demonstration | 30 | | |

## Notes

- Exit codes marked with `?` need to be determined by analyzing the source code
- Programs with "(multiple lines)" in stdout produce multi-line output that needs exact matching
- Programs with "(requires stdin)" need input data to run correctly
- Programs with "(runtime error)" in stderr are expected to fail at runtime

## Running Tests

The integration test harness (`crates/rue/tests/corpus_tests.rs`) reads this file to determine expected outputs for each program. It will:

1. Parse this README to extract test metadata
2. Compile each `.rue` file listed
3. Run the compiled program
4. Compare actual outputs with expected values
5. Report any programs in the directory not listed in this README

## Adding New Tests

When adding a new test program:
1. Add the `.rue` file to this directory
2. Add an entry to the table above with expected outputs
3. Run the integration tests to verify