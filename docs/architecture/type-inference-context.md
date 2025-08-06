# TypeInferenceContext Design

## Overview

The `TypeInferenceContext` is a robust type inference system integrated into the Rue compiler's semantic analysis phase. It improves upon the previous ad-hoc type inference by providing:

1. **Constraint-based inference** - Collects and solves type constraints
2. **Bidirectional type checking** - Supports both synthesis and checking modes
3. **Contextual type propagation** - Better inference for literals and operations
4. **Future extensibility** - Foundation for type variables and generics

## Architecture

### Core Components

```rust
pub struct TypeInferenceContext {
    next_var_id: u32,                         // Type variable ID generator
    constraints: Vec<TypeConstraint>,         // Collected constraints
    solutions: HashMap<TypeVarId, RueType>,   // Solved type variables
    pending_vars: HashSet<TypeVarId>,         // Unsolved variables
    context_stack: Vec<InferenceScope>,       // Nested inference scopes
}
```

### Type Constraints

The system supports three types of constraints:

1. **Equality constraints** - Two types must be equal
2. **AtLeast constraints** - Type variable must be at least as general as a type
3. **Binary operation constraints** - Relates operand and result types

```rust
enum TypeConstraint {
    Equal(RueType, RueType),
    AtLeast(TypeVarId, RueType),
    Binary(BinOp, TypeVarId, TypeVarId, TypeVarId),
}
```

## Key Features

### 1. Smart Numeric Literal Inference

The context intelligently infers numeric literal types based on:
- **Value range** - Defaults to i32 if value fits, otherwise i64
- **Expected type** - Uses context from assignments, returns, parameters
- **Operation context** - Propagates types through binary operations

```rust
// Example: Return type guides literal inference
fn compute() -> i32 {
    let x = 100 + 200;  // Both literals inferred as i32
    x
}
```

### 2. Binary Operation Type Propagation

Binary operations propagate type information bidirectionally:

```rust
let x: i64 = 1000;
let y = x + 500;  // 500 inferred as i64 from x's type
```

The inference works by:
1. Checking initial operand types
2. Using expected result type to guide operand inference
3. Applying contextual inference for literals
4. Validating type compatibility

### 3. Constraint Solving

The constraint solver uses a fixed-point iteration algorithm:

1. **Collect constraints** during expression checking
2. **Iterate** until no changes occur
3. **Propagate** solutions through related constraints
4. **Default** unsolved numeric variables to i32

### 4. Scoped Type Expectations

The context maintains a stack of inference scopes, each with:
- Expected type (if any)
- Inference mode (synthesis vs checking)

This enables nested type inference contexts:

```rust
fn outer() -> i64 {
    // Scope expects i64
    let inner = {
        // Nested scope can have different expectation
        let x = 42;  // Could be i32 or i64
        to_i64(x)
    };
    inner
}
```

## Integration with TypeChecker

The `TypeInferenceContext` is integrated into the `TypeChecker` struct:

```rust
pub struct TypeChecker {
    variable_scopes: Vec<VariableScope>,
    function_signatures: HashMap<String, FunctionSignature>,
    global_scope: Scope,
    inference_context: TypeInferenceContext,  // New addition
}
```

### Usage Pattern

1. **Literal checking** - Uses `infer_numeric_literal()` for smart defaults
2. **Binary expressions** - Uses `infer_binary_operation()` for type propagation
3. **Function calls** - Propagates parameter types to argument expressions
4. **Let bindings** - Uses type annotations or return types as hints

## Benefits

### Current Benefits

1. **Better ergonomics** - Less explicit type annotations needed
2. **Smarter defaults** - Numeric literals default appropriately
3. **Consistent inference** - Centralized inference logic
4. **Performance** - Avoids redundant expression re-checking

### Future Extensions

The `TypeInferenceContext` provides a foundation for:

1. **Type variables** - Already supports TypeVarId for future generics
2. **Polymorphic functions** - Constraint system can handle type parameters
3. **Type inference for closures** - Scoped expectations support lambda inference
4. **Subtyping** - Constraint system extensible for subtype relations
5. **Type classes/traits** - Can add constraint kinds for trait bounds

## Example Usage

### Basic Inference

```rust
// No type annotation needed
fn compute() -> i32 {
    let x = 10 + 20;  // Both inferred as i32
    let y = x * 2;    // 2 inferred as i32
    y
}
```

### Contextual Inference

```rust
fn process(data: [i64; 100]) -> i64 {
    let sum = 0;  // Inferred as i64 from return type
    let i = 0;    // Could be i32 or i64
    
    while i < 100 {  // 100 inferred from i's type
        sum = sum + data[i];
        i = i + 1;
    }
    
    sum
}
```

### Mixed Types

```rust
fn mixed() {
    let a: i32 = 100;
    let b: i64 = 200;
    
    // Explicit conversion still required for safety
    let c = to_i64(a) + b;  // Result is i64
}
```

## Implementation Details

### Constraint Solving Algorithm

```
1. Initialize empty solution map
2. While constraints exist and changes occur:
   a. For each Equal(t1, t2) constraint:
      - Verify t1 == t2 or error
   b. For each AtLeast(var, type) constraint:
      - If var solved, verify compatibility
      - Otherwise, solve var = type
   c. For each Binary constraint:
      - Propagate known types to unknown variables
      - Apply operation-specific rules
3. Default remaining numeric variables to i32
4. Return success or error
```

### Performance Considerations

- Constraint solving is O(n²) worst case, O(n) typical
- Type variable creation is O(1)
- Scope push/pop is O(1)
- Most programs require minimal constraint solving

## Testing

The implementation includes comprehensive tests in `/workspace/crates/rue-semantic/src/type_inference_test.rs`:

1. Unit tests for inference functions
2. Integration tests with full programs
3. Constraint solving tests
4. Scope management tests

## Conclusion

The `TypeInferenceContext` provides a solid foundation for type inference in Rue, improving the developer experience while maintaining type safety. Its constraint-based design allows for future extensions without major architectural changes.