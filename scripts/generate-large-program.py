#!/usr/bin/env python3
"""
Generates a large Rue program for stress testing the compiler.
Creates many functions with various control flow patterns.
"""

def generate_helper_functions(count):
    """Generate helper functions with different patterns."""
    functions = []
    
    for i in range(count):
        if i % 4 == 0:
            # Simple arithmetic
            func = f"""fn helper_{i}(x: i32) -> i32 {{
    x * 2 + {i}
}}"""
        elif i % 4 == 1:
            # Conditional logic
            func = f"""fn helper_{i}(x: i32) -> i32 {{
    if x > {i} {{
        x - {i}
    }} else {{
        x + {i}
    }}
}}"""
        elif i % 4 == 2:
            # Loop-based computation
            func = f"""fn helper_{i}(x: i32) -> i32 {{
    let result: i32 = 0;
    let counter: i32 = 0;
    while counter < x {{
        result = result + counter + {i};
        counter = counter + 1;
    }};
    result
}}"""
        else:
            # Recursive function
            func = f"""fn helper_{i}(x: i32) -> i32 {{
    if x <= 1 {{
        {i}
    }} else {{
        helper_{i}(x - 1) + helper_{i}(x - 2) + {i}
    }}
}}"""
        
        functions.append(func)
    
    return functions

def generate_main_function(helper_count):
    """Generate main function that calls all helpers."""
    calls = []
    for i in range(helper_count):
        calls.append(f"    sum = sum + helper_{i}(i);")
    
    main_func = f"""fn main() -> i32 {{
    let sum: i32 = 0;
    let i: i32 = 1;
    while i <= {helper_count // 2} {{
{chr(10).join(calls[:helper_count//2])}
        i = i + 1;
    }};
    
    // Call remaining helpers with different pattern
    let j: i32 = 0;
    while j < {helper_count - helper_count//2} {{
{chr(10).join(calls[helper_count//2:])}
        j = j + 1;
    }};
    
    sum
}}"""
    
    return main_func

def main():
    import sys
    # Allow configuring size via environment variable or argument
    if len(sys.argv) > 1:
        helper_count = int(sys.argv[1])
    else:
        import os
        helper_count = int(os.environ.get('BENCH_HELPER_COUNT', '500'))  # Default to 500 functions
    
    print("// Large Rue program for performance testing")
    print("// Generated automatically - do not edit manually")
    print()
    
    # Generate helper functions
    helpers = generate_helper_functions(helper_count)
    for helper in helpers:
        print(helper)
        print()
    
    # Generate main function
    main_func = generate_main_function(helper_count)
    print(main_func)

if __name__ == "__main__":
    main()